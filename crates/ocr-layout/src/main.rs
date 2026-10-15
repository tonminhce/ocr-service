use std::{
    env,
    io::{self, BufReader, Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use image::{ImageReader, Limits, RgbImage};
use ocr_layout::{
    model::Detector,
    preprocessing,
    protocol::{self, DetectRequest, Response},
};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let mut image = None;
    let mut directory = None;
    let mut model = None;
    let mut sha = None;
    let mut tensor_output = None;
    let mut max_pixels = 40_000_000_u64;
    while let Some(flag) = args.next() {
        let value = args.next().context("expected a flag value")?;
        match flag.as_str() {
            "--image" => image = Some(PathBuf::from(value)),
            "--serve-directory" => directory = Some(PathBuf::from(value)),
            "--model" => model = Some(PathBuf::from(value)),
            "--sha256" => sha = Some(value),
            "--tensor-output" => tensor_output = Some(PathBuf::from(value)),
            "--max-pixels" => max_pixels = value.parse()?,
            _ => bail!("unknown detector flag"),
        }
    }
    ensure!(
        (1..=100_000_000).contains(&max_pixels),
        "invalid detector pixel budget"
    );
    ensure!(
        image.is_some() != directory.is_some(),
        "exactly one of --image and --serve-directory is required"
    );
    if let Some(input) = image {
        let page = read_page(&input, max_pixels)?;
        if let Some(path) = tensor_output {
            std::fs::write(path, preprocessing::resize_rgb(&page).as_raw())?;
            return Ok(());
        }
        let mut detector = load_detector(model, sha.as_deref())?;
        let output = detector.detect(&page)?;
        println!("{}", serde_json::to_string(&output)?);
        return Ok(());
    }
    ensure!(
        tensor_output.is_none(),
        "tensor output is not a server operation"
    );
    let directory = directory.expect("checked input mode").canonicalize()?;
    ensure!(directory.is_dir(), "scratch path is not a directory");
    let mut detector = load_detector(model, sha.as_deref())?;
    let mut output = io::stdout().lock();
    write_response(
        &mut output,
        &Response::Ready {
            version: protocol::VERSION,
            model_sha256: sha.expect("validated model digest").to_ascii_lowercase(),
        },
    )?;
    let mut input = BufReader::new(io::stdin().lock());
    while let Some(frame) = protocol::read_frame(&mut input, protocol::MAX_REQUEST_BYTES)? {
        let request: DetectRequest = serde_json::from_slice(&frame)?;
        ensure!(
            request.version == protocol::VERSION,
            "unsupported protocol version"
        );
        ensure!(request.id > 0, "invalid request id");
        let page = read_request_page(&directory, &request, max_pixels);
        let response = match page {
            Ok(page) => match detector.detect(&page) {
                Ok(layout) => Response::Layout {
                    id: request.id,
                    layout,
                },
                Err(_) => Response::Error {
                    id: request.id,
                    code: "inference_failed".into(),
                },
            },
            Err(_) => Response::Error {
                id: request.id,
                code: "invalid_image".into(),
            },
        };
        write_response(&mut output, &response)?;
    }
    Ok(())
}

fn load_detector(model: Option<PathBuf>, sha: Option<&str>) -> Result<Detector> {
    let runtime = env::var("ORT_DYLIB_PATH").context("ORT_DYLIB_PATH is required")?;
    ort::init_from(runtime)?.commit();
    Detector::load(
        &model.context("--model is required")?,
        sha.context("--sha256 is required")?,
    )
}

fn read_request_page(root: &Path, request: &DetectRequest, max_pixels: u64) -> Result<RgbImage> {
    ensure!(
        protocol::valid_image_basename(&request.image),
        "invalid image basename"
    );
    ensure!(
        request.width > 0
            && request.height > 0
            && request.width <= 8192
            && request.height <= 8192
            && u64::from(request.width) * u64::from(request.height) <= max_pixels,
        "invalid declared dimensions"
    );
    let path = root.join(&request.image);
    ensure!(
        std::fs::symlink_metadata(&path)?.file_type().is_file(),
        "not a regular file"
    );
    ensure!(
        path.canonicalize()?.parent() == Some(root),
        "image outside scratch directory"
    );
    let page = read_page(&path, max_pixels)?;
    // The private scratch image is no longer needed once decoded. A warm
    // helper must not retain the last customer's raster while sitting idle.
    std::fs::remove_file(&path).context("cannot clear scratch image")?;
    ensure!(
        (page.width(), page.height()) == (request.width, request.height),
        "dimensions disagree"
    );
    Ok(page)
}

fn read_page(input: &Path, max_pixels: u64) -> Result<RgbImage> {
    ensure!(
        std::fs::metadata(input)?.len() <= max_pixels * 4 + 1_048_576,
        "page file exceeds byte budget"
    );
    let bytes = std::fs::read(input)?;
    let format = image::guess_format(&bytes)?;
    let (width, height) =
        ImageReader::with_format(Cursor::new(&bytes), format).into_dimensions()?;
    ensure!(
        width > 0
            && height > 0
            && width <= 8192
            && height <= 8192
            && u64::from(width) * u64::from(height) <= max_pixels,
        "page exceeds pixel/dimension budget"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some((max_pixels * 8).min(512 * 1024 * 1024));
    reader.limits(limits);
    Ok(reader.decode()?.to_rgb8())
}

fn write_response(output: &mut impl Write, response: &Response) -> Result<()> {
    let mut frame = serde_json::to_vec(response)?;
    if frame.len() >= protocol::MAX_RESPONSE_BYTES {
        if let Response::Layout { id, .. } = response {
            frame = serde_json::to_vec(&Response::Error {
                id: *id,
                code: "output_limit".into(),
            })?;
        } else {
            bail!("response exceeds byte bound");
        }
    }
    frame.push(b'\n');
    output.write_all(&frame)?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_reads_only_its_scratch_image_and_removes_it_after_decode() {
        let root = tempfile_path();
        let path = root.join("page.png");
        RgbImage::new(4, 3).save(&path).unwrap();
        let request = DetectRequest {
            version: protocol::VERSION,
            id: 1,
            image: "page.png".into(),
            width: 4,
            height: 3,
        };
        let page = read_request_page(&root, &request, 100).unwrap();
        assert_eq!((page.width(), page.height()), (4, 3));
        assert!(
            !path.exists(),
            "idle helpers must not retain the page raster"
        );
        let mut request = request;
        request.image = "../outside.png".into();
        assert!(read_request_page(&root, &request, 100).is_err());
        std::fs::remove_dir(root).unwrap();
    }

    // Avoid an additional dependency for these binary-only boundary tests.
    fn tempfile_path() -> PathBuf {
        // Wall-clock resolution is not a uniqueness guarantee when tests run
        // concurrently. Atomic allocation also tolerates leftovers after a
        // crashed run or process-ID reuse.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let path = env::temp_dir().join(format!(
                "ocr-layout-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return path.canonicalize().unwrap(),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot allocate test directory: {error}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn server_rejects_symlink_even_when_it_points_inside_scratch() {
        let root = tempfile_path();
        let real = root.join("real.png");
        RgbImage::new(4, 3).save(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("page.png")).unwrap();
        let request = DetectRequest {
            version: protocol::VERSION,
            id: 1,
            image: "page.png".into(),
            width: 4,
            height: 3,
        };
        assert!(read_request_page(&root, &request, 100).is_err());
        assert!(real.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
