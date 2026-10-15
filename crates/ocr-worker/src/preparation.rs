use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage,
    codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

use crate::{ProcessingError, settings::Settings, subprocess_policy::PdfTool};

/// One document owns its temporary directory until every page is processed.
pub enum Document {
    Image(Vec<u8>),
    Pdf {
        directory: Arc<TempDir>,
        path: PathBuf,
        pages: u32,
    },
}

impl Document {
    pub fn page_count(&self) -> u32 {
        match self {
            Self::Image(_) => 1,
            Self::Pdf { pages, .. } => *pages,
        }
    }
}

pub async fn inspect(
    input: Vec<u8>,
    media_type: &str,
    settings: &Settings,
) -> Result<Document, ProcessingError> {
    inspect_with_cancellation(input, media_type, settings, CancellationToken::new()).await
}

pub(crate) async fn inspect_with_cancellation(
    input: Vec<u8>,
    media_type: &str,
    settings: &Settings,
    cancelled: CancellationToken,
) -> Result<Document, ProcessingError> {
    if cancelled.is_cancelled() {
        return Err(ProcessingError::transient("job processing cancelled"));
    }
    match media_type {
        "image/png" | "image/jpeg" | "image/webp" => {
            let actual = image::guess_format(&input)
                .map_err(|_| ProcessingError::permanent("input is not a supported image"))?;
            let expected = match media_type {
                "image/png" => ImageFormat::Png,
                "image/jpeg" => ImageFormat::Jpeg,
                _ => ImageFormat::WebP,
            };
            if actual != expected {
                return Err(ProcessingError::permanent(
                    "image bytes do not match the declared media type",
                ));
            }
            Ok(Document::Image(input))
        }
        "application/pdf" => {
            if !input.starts_with(b"%PDF-") {
                return Err(ProcessingError::permanent(
                    "input does not have a PDF header",
                ));
            }
            let directory = crate::subprocess_policy::scratch_directory()?;
            let path = directory.path().join("document.pdf");
            write_scratch_input(
                directory.clone(),
                path.clone(),
                input,
                settings.pdf_render_timeout_seconds,
                cancelled.clone(),
            )
            .await?;
            let command = PdfTool::Info {
                input: &path,
                page: None,
            }
            .command(directory.path())?;
            let output = run_pdf_command(
                command,
                directory.clone(),
                None,
                settings.pdf_render_timeout_seconds,
                cancelled,
            )
            .await?;
            let text = std::str::from_utf8(&output)
                .map_err(|_| ProcessingError::permanent("PDF metadata is not valid UTF-8"))?;
            let pages = parse_pdf_pages(text, settings.max_pages)?;
            Ok(Document::Pdf {
                directory,
                path,
                pages,
            })
        }
        _ => Err(ProcessingError::permanent(
            "supported media types are PDF, PNG, JPEG, and WebP",
        )),
    }
}

pub fn parse_pdf_pages(output: &str, max_pages: u32) -> Result<u32, ProcessingError> {
    if output
        .lines()
        .any(|line| line.starts_with("Encrypted:") && line.split_whitespace().nth(1) == Some("yes"))
    {
        return Err(ProcessingError::permanent(
            "encrypted PDFs are not supported",
        ));
    }
    let pages = output
        .lines()
        .find_map(|line| line.strip_prefix("Pages:").map(str::trim))
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| {
            ProcessingError::permanent("PDF metadata does not contain a valid page count")
        })?;
    if pages > max_pages {
        return Err(ProcessingError::permanent(format!(
            "PDF has {pages} pages; the limit is {max_pages}"
        )));
    }
    Ok(pages)
}

/// Rendering is one page at a time. Both Poppler's output dimensions and Rust's
/// decode allocations are bounded; raw document pages never accumulate in RAM.
pub async fn prepare_page(
    document: &Document,
    page: u32,
    settings: &Settings,
    decode_slots: Arc<Semaphore>,
    cancelled: CancellationToken,
) -> Result<Vec<u8>, ProcessingError> {
    if page == 0 || page > document.page_count() {
        return Err(ProcessingError::permanent("invalid document page number"));
    }
    let bytes = match document {
        Document::Image(bytes) => bytes.clone(),
        Document::Pdf {
            directory, path, ..
        } => {
            let prefix = directory.path().join(format!("page-{page}"));
            let command = PdfTool::Render {
                input: path,
                prefix: &prefix,
                page,
                dpi: 144.0,
                scale_to: Some(settings.max_image_edge),
                cropbox: false,
            }
            .command(directory.path())?;
            let output_path = prefix.with_extension("png");
            let raster = Arc::new(ScratchOutput(output_path.clone()));
            run_pdf_command(
                command,
                directory.clone(),
                Some(raster.clone()),
                settings.pdf_render_timeout_seconds,
                cancelled.clone(),
            )
            .await?;
            // The encoded file is bounded independently of decoder allocations.
            let max_bytes = settings
                .max_image_pixels
                .saturating_mul(4)
                .saturating_add(1_048_576);
            let bytes = read_file_limited(&output_path, max_bytes).await?;
            tokio::fs::remove_file(output_path)
                .await
                .map_err(|_| ProcessingError::transient("cannot remove rendered page"))?;
            bytes
        }
    };
    let permit = tokio::select! {
        _ = cancelled.cancelled() => return Err(ProcessingError::transient("job processing cancelled")),
        permit = decode_slots.acquire_owned() => permit.map_err(|_| ProcessingError::transient("image processing is stopping"))?,
    };
    let max_pixels = settings.max_image_pixels;
    let max_edge = settings.max_image_edge;
    tokio::task::spawn_blocking(move || {
        // Keep the permit inside the blocking closure, even if its async waiter
        // is cancelled. New work cannot oversubscribe still-running decoders.
        let _permit = permit;
        if cancelled.is_cancelled() {
            return Err(ProcessingError::transient("job processing cancelled"));
        }
        let result = normalize_image(&bytes, max_pixels, max_edge)?;
        if cancelled.is_cancelled() {
            return Err(ProcessingError::transient("job processing cancelled"));
        }
        Ok(result)
    })
    .await
    .map_err(|_| ProcessingError::transient("image processing task failed"))?
}

/// Prepare the upright RGB page used by layout detection and region cropping.
/// Layout coordinates therefore refer to this exact image, including any EXIF
/// transform and the PDF render scale. Page limits are independent of crop limits.
pub async fn prepare_rgb_page(
    document: &Document,
    page: u32,
    settings: &Settings,
    decode_slots: Arc<Semaphore>,
    cancelled: CancellationToken,
) -> Result<RgbImage, ProcessingError> {
    if page == 0 || page > document.page_count() {
        return Err(ProcessingError::permanent("invalid document page number"));
    }
    if cancelled.is_cancelled() {
        return Err(ProcessingError::transient("job processing cancelled"));
    }
    let bytes = match document {
        Document::Image(bytes) => bytes.clone(),
        Document::Pdf {
            directory, path, ..
        } => {
            // -scale-to always stretches small pages to the limit. Inspect the
            // physical size instead, then lower the configured DPI only when
            // the page would otherwise exceed the page edge limit.
            let metadata = PdfTool::Info {
                input: path,
                page: Some(page),
            }
            .command(directory.path())?;
            let output = run_pdf_command(
                metadata,
                directory.clone(),
                None,
                settings.pdf_render_timeout_seconds,
                cancelled.clone(),
            )
            .await?;
            let metadata = std::str::from_utf8(&output)
                .map_err(|_| ProcessingError::permanent("PDF metadata is not valid UTF-8"))?;
            let (width_points, height_points) = parse_pdf_page_size(metadata, page)?;
            let dpi = bounded_pdf_dpi(
                width_points,
                height_points,
                settings.pdf_render_dpi,
                settings.page_max_edge,
                settings.max_image_pixels,
            )?;
            if cancelled.is_cancelled() {
                return Err(ProcessingError::transient("job processing cancelled"));
            }
            let prefix = directory.path().join(format!("course-page-{page}"));
            let command = PdfTool::Render {
                input: path,
                prefix: &prefix,
                page,
                dpi,
                scale_to: None,
                cropbox: true,
            }
            .command(directory.path())?;
            let output_path = prefix.with_extension("png");
            let raster = Arc::new(ScratchOutput(output_path.clone()));
            let rendered = run_pdf_command(
                command,
                directory.clone(),
                Some(raster.clone()),
                settings.pdf_render_timeout_seconds,
                cancelled.clone(),
            )
            .await;
            // Cleanup can still be pending after its own bounded wait. Only the
            // shared guard may unlink a raster until the supervisor releases it.
            rendered?;
            if cancelled.is_cancelled() {
                let _ = tokio::fs::remove_file(&output_path).await;
                return Err(ProcessingError::transient("job processing cancelled"));
            }
            let max_bytes = settings
                .max_image_pixels
                .saturating_mul(4)
                .saturating_add(1_048_576);
            let result = read_file_limited(&output_path, max_bytes).await;
            // Remove a rendered file even when its encoded size was rejected.
            let removed = tokio::fs::remove_file(output_path).await;
            let bytes = result?;
            removed.map_err(|_| ProcessingError::transient("cannot remove rendered page"))?;
            bytes
        }
    };
    let permit = tokio::select! {
        _ = cancelled.cancelled() => return Err(ProcessingError::transient("job processing cancelled")),
        permit = decode_slots.acquire_owned() => permit.map_err(|_| ProcessingError::transient("image processing is stopping"))?,
    };
    let max_pixels = settings.max_image_pixels;
    let max_edge = settings.page_max_edge;
    tokio::task::spawn_blocking(move || {
        // A cancelled async waiter must not release this slot while decoding
        // still runs on the blocking pool.
        let _permit = permit;
        if cancelled.is_cancelled() {
            return Err(ProcessingError::transient("job processing cancelled"));
        }
        let result = normalize_rgb_image(&bytes, max_pixels, max_edge)?;
        if cancelled.is_cancelled() {
            return Err(ProcessingError::transient("job processing cancelled"));
        }
        Ok(result)
    })
    .await
    .map_err(|_| ProcessingError::transient("image processing task failed"))?
}

fn parse_pdf_page_size(output: &str, page: u32) -> Result<(f64, f64), ProcessingError> {
    let value = output.lines().find_map(|line| {
        let (label, value) = line.trim().split_once("size:")?;
        let number = label.strip_prefix("Page")?.trim();
        if number.is_empty() || number.parse::<u32>().ok() == Some(page) {
            Some(value)
        } else {
            None
        }
    });
    let invalid = || ProcessingError::permanent("PDF metadata has an invalid page size");
    let mut fields = value.ok_or_else(invalid)?.split_whitespace();
    let width = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(invalid)?;
    if fields.next() != Some("x") {
        return Err(invalid());
    }
    let height = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(invalid)?;
    if fields.next() != Some("pts")
        || !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
    {
        return Err(invalid());
    }
    Ok((width, height))
}

fn bounded_pdf_dpi(
    width_points: f64,
    height_points: f64,
    render_dpi: u32,
    max_edge: u32,
    max_pixels: u64,
) -> Result<f64, ProcessingError> {
    if !width_points.is_finite()
        || !height_points.is_finite()
        || width_points <= 0.0
        || height_points <= 0.0
        || render_dpi == 0
        || max_edge == 0
        || max_pixels == 0
    {
        return Err(ProcessingError::permanent(
            "invalid PDF rendering dimensions",
        ));
    }
    let dpi =
        f64::from(render_dpi).min(f64::from(max_edge) * 72.0 / width_points.max(height_points));
    let width = (width_points * dpi / 72.0).ceil();
    let height = (height_points * dpi / 72.0).ceil();
    if !dpi.is_finite()
        || dpi <= 0.0
        || !width.is_finite()
        || !height.is_finite()
        || width < 1.0
        || height < 1.0
        || width > f64::from(max_edge) + 1.0
        || height > f64::from(max_edge) + 1.0
        || width * height > max_pixels as f64
    {
        return Err(ProcessingError::permanent(
            "PDF page exceeds image rendering limits",
        ));
    }
    Ok(dpi)
}

pub fn normalize_image(
    input: &[u8],
    max_pixels: u64,
    max_edge: u32,
) -> Result<Vec<u8>, ProcessingError> {
    let rgb = normalize_rgb_image(input, max_pixels, max_edge)?;
    encode_jpeg(&rgb, 90)
}

fn normalize_rgb_image(
    input: &[u8],
    max_pixels: u64,
    max_edge: u32,
) -> Result<RgbImage, ProcessingError> {
    if max_pixels == 0 || max_edge == 0 {
        return Err(ProcessingError::permanent(
            "image processing limits must be positive",
        ));
    }
    let format = image::guess_format(input)
        .map_err(|_| ProcessingError::permanent("invalid image encoding"))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(ProcessingError::permanent(
            "image format is not PNG, JPEG, or WebP",
        ));
    }
    // Header inspection must precede decompression. Checking width*height after
    // decode alone would already have allocated attacker-controlled dimensions.
    let (width, height) = ImageReader::with_format(Cursor::new(input), format)
        .into_dimensions()
        .map_err(|_| ProcessingError::permanent("cannot read image dimensions"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > max_pixels {
        return Err(ProcessingError::permanent(format!(
            "image exceeds the {max_pixels} pixel limit"
        )));
    }
    let mut reader = ImageReader::with_format(Cursor::new(input), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(max_pixels.saturating_mul(8).min(512 * 1024 * 1024));
    reader.limits(limits.clone());
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| ProcessingError::permanent("invalid or oversized image data"))?;
    // ImageReader::decode reserves the output buffer before passing the
    // remaining budget to the decoder. into_decoder does not, so retain that
    // allocation check explicitly when reading orientation metadata ourselves.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|_| ProcessingError::permanent("invalid or oversized image data"))?;
    decoder
        .set_limits(limits)
        .map_err(|_| ProcessingError::permanent("invalid or oversized image data"))?;
    let orientation = decoder
        .orientation()
        .map_err(|_| ProcessingError::permanent("invalid image orientation metadata"))?;
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|_| ProcessingError::permanent("invalid or oversized image data"))?;
    // Re-encoding removes metadata. Apply its rotation/mirroring first so phone
    // scans retain the same orientation the user sees in their photo viewer.
    decoded.apply_orientation(orientation);
    let resized = if width > max_edge || height > max_edge {
        decoded.thumbnail(max_edge, max_edge)
    } else {
        decoded
    };
    // Transparent backgrounds become white, so glyphs retain their document
    // appearance when sent as a JPEG to inference.
    let rgba = resized.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, source) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u16::from(source[3]);
        for channel in 0..3 {
            out[channel] =
                ((u16::from(source[channel]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        }
    }
    Ok(rgb)
}

fn encode_jpeg(rgb: &RgbImage, quality: u8) -> Result<Vec<u8>, ProcessingError> {
    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, quality)
        .encode_image(rgb)
        .map_err(|_| ProcessingError::transient("cannot encode image for inference"))?;
    Ok(output)
}

const COURSE_FACTOR: u32 = 28;
const COURSE_MIN_PIXELS: u64 = 112 * 112;
const COURSE_MAX_PIXELS: u64 = 14 * 14 * 4 * 15_000;
const MAX_CROP_SOURCE_PIXELS: u64 = 100_000_000;
const MAX_POLYGON_POINTS: usize = 4096;

/// Encode a region with the course SDK's normalized crop, white polygon mask,
/// smart resize and default JPEG quality. This CPU work belongs in a blocking
/// task whose concurrency permit remains held until this function returns.
pub fn encode_course_region_crop(
    page: &RgbImage,
    bbox_2d: [i32; 4],
    polygon: &[[i32; 2]],
    max_edge: u32,
) -> Result<Vec<u8>, ProcessingError> {
    encode_course_region_crop_with_padding(page, bbox_2d, polygon, max_edge, 0)
}

/// Optional model-input margin after the unchanged course crop/resize. This is
/// an explicit serving compatibility extension; page-space geometry stays the
/// measured source region. Padding never adds source pixels or inferred text.
pub fn encode_course_region_crop_with_padding(
    page: &RgbImage,
    bbox_2d: [i32; 4],
    polygon: &[[i32; 2]],
    max_edge: u32,
    padding: u32,
) -> Result<Vec<u8>, ProcessingError> {
    if padding > 512 || max_edge.saturating_add(padding.saturating_mul(2)) > 8192 {
        return Err(ProcessingError::permanent(
            "padded recognition crop exceeds its dimension limit",
        ));
    }
    let crop = compute_crop_bitmap(page, bbox_2d, polygon, max_edge)?;
    let resized = if padding == 0 {
        crop
    } else {
        let mut canvas = RgbImage::from_pixel(
            crop.width() + 2 * padding,
            crop.height() + 2 * padding,
            image::Rgb([255, 255, 255]),
        );
        image::imageops::replace(&mut canvas, &crop, i64::from(padding), i64::from(padding));
        canvas
    };
    let width = u16::try_from(resized.width())
        .map_err(|_| ProcessingError::permanent("region width exceeds JPEG encoding limits"))?;
    let height = u16::try_from(resized.height())
        .map_err(|_| ProcessingError::permanent("region height exceeds JPEG encoding limits"))?;
    let mut output = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut output, 75);
    // Pillow's default RGB JPEG uses 4:2:0 chroma subsampling.
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder
        .encode(
            resized.as_raw(),
            width,
            height,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|_| ProcessingError::transient("cannot encode region for inference"))?;
    Ok(output)
}

/// Build the course region bitmap before its lossy JPEG encoding. Keeping this
/// stage separate allows fixture checks against SDK mask and resize pixels.
pub(crate) fn compute_crop_bitmap(
    page: &RgbImage,
    bbox_2d: [i32; 4],
    polygon: &[[i32; 2]],
    max_edge: u32,
) -> Result<RgbImage, ProcessingError> {
    let bounds = crop_bounds(page, bbox_2d, polygon)?;
    let width = bounds[2] - bounds[0];
    let height = bounds[3] - bounds[1];
    // Validate the destination before either copying the crop or resizing it.
    let (resized_width, resized_height) = course_resize_dimensions(width, height, max_edge)?;
    let mut cropped =
        image::imageops::crop_imm(page, bounds[0], bounds[1], width, height).to_image();
    if polygon.len() >= 3 {
        let vertices = polygon
            .iter()
            .map(|point| {
                // The SDK multiplies by a precomputed floating page scale for
                // polygons, which can truncate one pixel below bbox conversion.
                [
                    polygon_pixel(point[0], page.width()) - bounds[0] as i32,
                    polygon_pixel(point[1], page.height()) - bounds[1] as i32,
                ]
            })
            .collect::<Vec<_>>();
        apply_polygon_mask(&mut cropped, &vertices);
    }
    // Catmull-Rom is the cubic convolution kernel used by Pillow BICUBIC.
    // Encoder and resampler implementations can differ in the final bytes.
    Ok(image::imageops::resize(
        &cropped,
        resized_width,
        resized_height,
        FilterType::CatmullRom,
    ))
}

fn normalized_pixel(value: i32, extent: u32) -> u32 {
    (value as u64 * u64::from(extent) / 1000) as u32
}

fn polygon_pixel(value: i32, extent: u32) -> i32 {
    (f64::from(value) * (f64::from(extent) / 1000.0)) as i32
}

fn crop_bounds(
    page: &RgbImage,
    bbox: [i32; 4],
    polygon: &[[i32; 2]],
) -> Result<[u32; 4], ProcessingError> {
    if page.width() == 0
        || page.height() == 0
        || page.width() > i32::MAX as u32
        || page.height() > i32::MAX as u32
        || u64::from(page.width()) * u64::from(page.height()) > MAX_CROP_SOURCE_PIXELS
    {
        return Err(ProcessingError::permanent(
            "invalid or oversized crop source image",
        ));
    }
    if bbox.iter().any(|value| !(0..=1000).contains(value))
        || bbox[0] >= bbox[2]
        || bbox[1] >= bbox[3]
    {
        return Err(ProcessingError::permanent(
            "invalid normalized region bounding box",
        ));
    }
    if polygon.len() > MAX_POLYGON_POINTS
        || polygon
            .iter()
            .flatten()
            .any(|value| !(0..=1000).contains(value))
    {
        return Err(ProcessingError::permanent(
            "invalid normalized region polygon",
        ));
    }
    let bounds = [
        normalized_pixel(bbox[0], page.width()),
        normalized_pixel(bbox[1], page.height()),
        normalized_pixel(bbox[2], page.width()),
        normalized_pixel(bbox[3], page.height()),
    ];
    if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
        return Err(ProcessingError::permanent(
            "region is empty at page pixel resolution",
        ));
    }
    Ok(bounds)
}

fn course_resize_dimensions(
    width: u32,
    height: u32,
    max_edge: u32,
) -> Result<(u32, u32), ProcessingError> {
    if width == 0 || height == 0 || !(COURSE_FACTOR..=8192).contains(&max_edge) {
        return Err(ProcessingError::permanent(
            "invalid region resize dimensions",
        ));
    }
    // Python round() uses ties to even, unlike Rust's float round(). Dividing
    // integers also avoids introducing a float error at half-factor boundaries.
    let round_factor = |value: u32| {
        let quotient = value / COURSE_FACTOR;
        let remainder = value % COURSE_FACTOR;
        let up =
            remainder > COURSE_FACTOR / 2 || (remainder == COURSE_FACTOR / 2 && quotient % 2 == 1);
        (quotient + u32::from(up)) * COURSE_FACTOR
    };
    let mut resized_width = round_factor(width);
    let mut resized_height = round_factor(height);
    let rounded_pixels = u64::from(resized_width) * u64::from(resized_height);
    let source_pixels = f64::from(width) * f64::from(height);
    if rounded_pixels > COURSE_MAX_PIXELS {
        let beta = (source_pixels / COURSE_MAX_PIXELS as f64).sqrt();
        resized_width =
            (f64::from(width) / beta / f64::from(COURSE_FACTOR)).floor() as u32 * COURSE_FACTOR;
        resized_height =
            (f64::from(height) / beta / f64::from(COURSE_FACTOR)).floor() as u32 * COURSE_FACTOR;
    } else if rounded_pixels < COURSE_MIN_PIXELS {
        let beta = (COURSE_MIN_PIXELS as f64 / source_pixels).sqrt();
        resized_width =
            (f64::from(width) * beta / f64::from(COURSE_FACTOR)).ceil() as u32 * COURSE_FACTOR;
        resized_height =
            (f64::from(height) * beta / f64::from(COURSE_FACTOR)).ceil() as u32 * COURSE_FACTOR;
    }
    if resized_width.max(resized_height) > max_edge {
        let capped_edge = max_edge / COURSE_FACTOR * COURSE_FACTOR;
        let denominator = u64::from(resized_width.max(resized_height)) * u64::from(COURSE_FACTOR);
        resized_width = (u64::from(resized_width) * u64::from(capped_edge) / denominator) as u32
            * COURSE_FACTOR;
        resized_height = (u64::from(resized_height) * u64::from(capped_edge) / denominator) as u32
            * COURSE_FACTOR;
    }
    let pixels = u64::from(resized_width) * u64::from(resized_height);
    if resized_width == 0
        || resized_height == 0
        || resized_width > max_edge
        || resized_height > max_edge
        || !(COURSE_MIN_PIXELS..=COURSE_MAX_PIXELS).contains(&pixels)
    {
        return Err(ProcessingError::permanent(
            "region aspect ratio cannot satisfy crop edge and pixel limits",
        ));
    }
    Ok((resized_width, resized_height))
}

#[derive(Clone, Copy)]
struct PolygonEdge {
    start: [i32; 2],
    end: [i32; 2],
    slope: f32,
}

impl PolygonEdge {
    fn min_y(self) -> i32 {
        self.start[1].min(self.end[1])
    }

    fn max_y(self) -> i32 {
        self.start[1].max(self.end[1])
    }

    fn x_at(self, y: i32) -> f32 {
        (y - self.start[1]) as f32 * self.slope + self.start[0] as f32
    }
}

fn apply_polygon_mask(image: &mut RgbImage, vertices: &[[i32; 2]]) {
    // Match Pillow's inclusive scanline fill and half-pixel edge rounding.
    // The mask is one row wide, so its memory use is independent of page area.
    let edges = vertices
        .iter()
        .copied()
        .zip(vertices.iter().copied().cycle().skip(1))
        .map(|(start, end)| PolygonEdge {
            start,
            end,
            slope: if start[1] == end[1] {
                0.0
            } else {
                (end[0] - start[0]) as f32 / (end[1] - start[1]) as f32
            },
        })
        .collect::<Vec<_>>();
    let sloped = edges
        .iter()
        .copied()
        .filter(|edge| edge.min_y() != edge.max_y())
        .collect::<Vec<_>>();
    let polygon_last_y = edges
        .iter()
        .map(|edge| edge.max_y())
        .max()
        .unwrap_or(0)
        .min(image.height() as i32);
    let mut row = vec![false; image.width() as usize];
    let mut intersections = Vec::with_capacity(sloped.len() * 2);
    for y in 0..image.height() as i32 {
        row.fill(false);
        let fill = |row: &mut [bool], left: i32, right: i32| {
            let start = left.max(0) as usize;
            let end = right.min(row.len() as i32 - 1);
            if end >= 0 && start <= end as usize {
                row[start..=end as usize].fill(true);
            }
        };
        for edge in &edges {
            if edge.min_y() == y && edge.min_y() == edge.max_y() {
                fill(
                    &mut row,
                    edge.start[0].min(edge.end[0]),
                    edge.start[0].max(edge.end[0]),
                );
            }
        }
        intersections.clear();
        for (index, edge) in sloped.iter().enumerate() {
            if y < edge.min_y() || y > edge.max_y() {
                continue;
            }
            let mut x = edge.x_at(y);
            if y == edge.max_y() && y < polygon_last_y {
                intersections.extend_from_slice(&[x, x]);
                continue;
            }
            // Pillow connects steep corners using the adjacent row. Preserve
            // this rule for concave contours and non-axis-aligned polygons.
            if (y == edge.min_y() || y == edge.max_y()) && edge.slope != 0.0 {
                for other in &sloped[..index] {
                    if (y != other.min_y() && y != other.max_y())
                        || other.slope == 0.0
                        || x.round() != other.x_at(y).round()
                    {
                        continue;
                    }
                    let adjacent = if y == edge.max_y() { y - 1 } else { y + 1 };
                    if adjacent >= other.min_y() && adjacent <= other.max_y() {
                        let a = edge.x_at(adjacent);
                        let b = other.x_at(adjacent);
                        if x > a + 1.0 && x > b + 1.0 {
                            x = a.max(b).round() + 1.0;
                        } else if x < a - 1.0 && x < b - 1.0 {
                            x = a.min(b).round() - 1.0;
                        }
                        break;
                    }
                }
            }
            intersections.push(x);
        }
        intersections.sort_by(f32::total_cmp);
        for pair in intersections.as_chunks::<2>().0 {
            let left = pair[0].round() as i32;
            let right = if pair[1] >= 0.0 {
                (pair[1] - 0.5).ceil() as i32
            } else {
                -((-pair[1] - 0.5).ceil() as i32)
            };
            fill(&mut row, left, right);
        }
        for (x, &inside) in row.iter().enumerate() {
            if !inside {
                image.put_pixel(x as u32, y as u32, image::Rgb([255; 3]));
            }
        }
    }
}

async fn read_file_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ProcessingError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ProcessingError::transient("cannot open rendered page"))?;
    read_limited(file, max_bytes).await
}

async fn read_limited(
    reader: impl AsyncRead + Unpin,
    max_bytes: u64,
) -> Result<Vec<u8>, ProcessingError> {
    let mut output = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut output)
        .await
        .map_err(|_| ProcessingError::transient("cannot read PDF process output"))?;
    if output.len() as u64 > max_bytes {
        return Err(ProcessingError::permanent(
            "PDF process output exceeded its size limit",
        ));
    }
    Ok(output)
}

/// The file guard is shared by caller and supervisor: caller abort cannot
/// unlink/recreate a partial raster while the process is still running.
struct ScratchOutput(PathBuf);
impl Drop for ScratchOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

static SCRATCH_WRITES: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// Blocking filesystem work retains both its slot and scratch if its async
/// waiter disappears. No detached write can recreate a deleted directory.
pub(crate) async fn write_scratch_input(
    directory: Arc<TempDir>,
    path: PathBuf,
    bytes: Vec<u8>,
    timeout_seconds: u64,
    cancelled: CancellationToken,
) -> Result<(), ProcessingError> {
    let expires = tokio::time::Instant::now()
        .checked_add(Duration::from_secs(timeout_seconds))
        .filter(|_| timeout_seconds > 0 && timeout_seconds <= 600)
        .ok_or_else(|| ProcessingError::permanent("PDF input preparation deadline is invalid"))?;
    let permit = tokio::select! {
        biased;
        _ = cancelled.cancelled() => return Err(ProcessingError::transient("job processing cancelled")),
        permit = tokio::time::timeout_at(expires, SCRATCH_WRITES.clone().acquire_owned()) => permit
            .map_err(|_| ProcessingError::transient("PDF input preparation admission timed out"))?
            .map_err(|_| ProcessingError::transient("PDF input preparation is stopping"))?,
    };
    let writing = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _directory = directory;
        std::fs::write(path, bytes)
            .map_err(|_| ProcessingError::transient("cannot write PDF temporary input"))
    });
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(ProcessingError::transient("job processing cancelled")),
        result = tokio::time::timeout_at(expires, writing) => result
            .map_err(|_| ProcessingError::transient("PDF input preparation timed out"))?
            .map_err(|_| ProcessingError::transient("PDF input preparation task failed"))?,
    }
}

async fn run_pdf_command(
    command: Command,
    directory: Arc<TempDir>,
    output: Option<Arc<ScratchOutput>>,
    timeout_seconds: u64,
    cancelled: CancellationToken,
) -> Result<Vec<u8>, ProcessingError> {
    crate::child_process::capture(
        command,
        (directory, output),
        Duration::from_secs(timeout_seconds),
        cancelled,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_rgb_decode_bounds_dimensions_and_flattens_alpha_to_white() {
        let pixels = image::RgbaImage::from_pixel(100, 80, image::Rgba([0, 0, 0, 0]));
        let mut input = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(pixels)
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        assert!(normalize_rgb_image(input.get_ref(), 7999, 3500).is_err());
        assert!(normalize_rgb_image(input.get_ref(), 8000, 0).is_err());
        let rgb = normalize_rgb_image(input.get_ref(), 8000, 32).unwrap();
        assert_eq!(rgb.dimensions(), (32, 26));
        assert!(rgb.pixels().all(|pixel| pixel.0 == [255; 3]));
    }

    #[test]
    fn course_page_rgb_applies_exif_rotation_before_coordinates_are_assigned() {
        let rgb = RgbImage::from_fn(80, 40, |x, y| {
            image::Rgb(match (x < 40, y < 20) {
                (true, true) => [255, 0, 0],
                (false, true) => [0, 255, 0],
                (true, false) => [0, 0, 255],
                (false, false) => [255, 255, 0],
            })
        });
        let jpeg = encode_jpeg(&rgb, 100).unwrap();
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&6_u16.to_le_bytes());
        exif.extend_from_slice(&[0; 6]);
        let mut input = jpeg[..2].to_vec();
        input.extend_from_slice(&[0xff, 0xe1]);
        input.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        input.extend_from_slice(&exif);
        input.extend_from_slice(&jpeg[2..]);
        let upright = normalize_rgb_image(&input, 3200, 3500).unwrap();
        assert_eq!(upright.dimensions(), (40, 80));
        for (x, y, expected) in [
            (10, 20, [0, 0, 255]),
            (30, 20, [255, 0, 0]),
            (10, 60, [255, 255, 0]),
            (30, 60, [0, 255, 0]),
        ] {
            assert!(
                upright
                    .get_pixel(x, y)
                    .0
                    .iter()
                    .zip(expected)
                    .all(|(&actual, expected)| actual.abs_diff(expected) < 20)
            );
        }
    }

    #[test]
    fn pdf_size_and_dpi_use_physical_page_size_without_upscaling_to_edge_limit() {
        assert_eq!(
            parse_pdf_page_size("Page    2 size:  612 x 792 pts (letter)\n", 2).unwrap(),
            (612.0, 792.0)
        );
        assert_eq!(
            parse_pdf_page_size("Page size:  72 x 144 pts\n", 1).unwrap(),
            (72.0, 144.0)
        );
        assert_eq!(
            bounded_pdf_dpi(72.0, 144.0, 200, 3500, 40_000_000).unwrap(),
            200.0
        );
        assert_eq!(
            bounded_pdf_dpi(612.0, 792.0, 200, 3500, 40_000_000).unwrap(),
            200.0
        );
        assert_eq!(
            bounded_pdf_dpi(3000.0, 600.0, 200, 3500, 40_000_000).unwrap(),
            84.0
        );
        for output in [
            "Page size: NaN x 144 pts",
            "Page size: 72 x inf pts",
            "Page size: 0 x 144 pts",
            "Page size: 72 x -144 pts",
            "Page size: 72 x 144 mm",
            "Page    2 size: 72 x 144 pts",
            "Pages: 1",
        ] {
            assert!(parse_pdf_page_size(output, 1).is_err(), "accepted {output}");
        }
        assert!(bounded_pdf_dpi(f64::NAN, 792.0, 200, 3500, 40_000_000).is_err());
        assert!(bounded_pdf_dpi(612.0, 792.0, 200, 3500, 1000).is_err());
    }

    #[test]
    fn normalized_crops_are_truncated_and_right_bottom_exclusive() {
        let page = RgbImage::from_fn(200, 200, |x, _| {
            image::Rgb(if x < 100 { [0; 3] } else { [255; 3] })
        });
        let jpeg = encode_course_region_crop(&page, [0, 0, 500, 1000], &[], 3500).unwrap();
        let cropped = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(cropped.dimensions(), (112, 196));
        assert!(cropped.pixels().all(|pixel| pixel.0.iter().all(|&v| v < 3)));

        // These values are from the Python SDK's two different conversions.
        assert_eq!(normalized_pixel(375, 144), 54);
        assert_eq!(polygon_pixel(375, 144), 53);
        assert_eq!(normalized_pixel(333, 137), 45);
        for bbox in [[-1, 0, 500, 500], [0, 0, 1001, 1000], [500, 0, 500, 1000]] {
            assert!(encode_course_region_crop(&page, bbox, &[], 3500).is_err());
        }
        assert!(encode_course_region_crop(&page, [0, 0, 1, 1000], &[], 3500).is_err());
        assert!(encode_course_region_crop(&page, [0, 0, 1000, 1000], &[[1001, 0]], 3500).is_err());
    }

    #[test]
    fn crop_polygon_uses_local_coordinates_and_white_background() {
        let page = RgbImage::from_pixel(280, 280, image::Rgb([0; 3]));
        let jpeg = encode_course_region_crop(
            &page,
            [250, 250, 750, 750],
            &[[250, 250], [750, 250], [250, 750]],
            3500,
        )
        .unwrap();
        let crop = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(crop.dimensions(), (140, 140));
        assert!(crop.get_pixel(20, 20).0.iter().all(|&v| v < 3));
        assert!(crop.get_pixel(120, 120).0.iter().all(|&v| v > 250));
    }

    #[test]
    fn course_jpeg_uses_pillow_quality_75_and_420_sampling() {
        let page = RgbImage::from_pixel(140, 140, image::Rgb([17, 81, 203]));
        let jpeg = encode_course_region_crop(&page, [0, 0, 1000, 1000], &[], 3500).unwrap();
        // Inspect the JPEG frame and quantization headers independently of its
        // encoder: Y uses 2x2 sampling; Cb and Cr use 1x1 sampling.
        let frame = jpeg
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xc0])
            .unwrap();
        let frame = &jpeg[frame + 4..];
        assert_eq!(frame[5], 3);
        assert_eq!([frame[7], frame[10], frame[13]], [0x22, 0x11, 0x11]);
        let quantization = jpeg
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xdb])
            .unwrap();
        assert_eq!(jpeg[quantization + 5], 8); // Default quality-75 luma DC quantizer.

        let legacy = encode_jpeg(&page, 90).unwrap();
        let frame = legacy
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xc0])
            .unwrap();
        let frame = &legacy[frame + 4..];
        assert_eq!([frame[7], frame[10], frame[13]], [0x11; 3]);
    }

    #[test]
    fn polygon_scanline_boundaries_match_pillow_fixtures() {
        // Recorded with ImageDraw.Draw(Image.new("L", (8, 8))).polygon(...).
        let fixtures = [
            (
                vec![[0, 0], [5, 2], [2, 5]],
                [
                    "#.......", "###.....", ".#####..", ".####...", "..##....", "..#.....",
                    "........", "........",
                ],
            ),
            (
                vec![[1, 0], [3, 4], [6, 1]],
                [
                    ".#......", "..#####.", "..####..", "...##...", "...#....", "........",
                    "........", "........",
                ],
            ),
            (
                vec![[0, 0], [7, 7], [0, 7], [7, 0]],
                [
                    "########", ".######.", "..####..", "...##...", "...##...", "..####..",
                    ".######.", "########",
                ],
            ),
        ];
        for (vertices, expected) in fixtures {
            let mut rgb = RgbImage::from_pixel(8, 8, image::Rgb([0; 3]));
            apply_polygon_mask(&mut rgb, &vertices);
            for (y, expected) in expected.iter().enumerate() {
                let actual = (0..8)
                    .map(|x| {
                        if rgb.get_pixel(x, y as u32).0 == [0; 3] {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect::<String>();
                assert_eq!(&actual, expected, "polygon {vertices:?}, row {y}");
            }
        }
    }

    #[test]
    fn smart_resize_matches_sdk_rounding_and_rejects_impossible_aspect_ratios() {
        // Results from SDK smart_resize(t=1, ...), before any configured cap.
        for (width, height, expected) in [
            (1, 1, (112, 112)),
            (42, 70, (112, 168)),
            (154, 182, (168, 168)), // 5.5 and 6.5 both round to 6.
            (196, 252, (196, 252)),
            (3500, 2000, (3500, 1988)),
            (4000, 4000, (3416, 3416)),
        ] {
            assert_eq!(
                course_resize_dimensions(width, height, 8192).unwrap(),
                expected
            );
        }
        let (width, height) = course_resize_dimensions(3500, 2000, 2048).unwrap();
        assert_eq!((width, height), (2044, 1148));
        assert_eq!(width % COURSE_FACTOR, 0);
        assert_eq!(height % COURSE_FACTOR, 0);
        assert!(
            (COURSE_MIN_PIXELS..=COURSE_MAX_PIXELS)
                .contains(&(u64::from(width) * u64::from(height)))
        );
        assert!(course_resize_dimensions(1, 100_000, 3500).is_err());
        assert!(course_resize_dimensions(0, 1, 3500).is_err());
        assert!(course_resize_dimensions(1, 1, 32).is_err());
    }

    #[tokio::test]
    async fn cancelled_scratch_write_never_creates_input() {
        let directory = crate::subprocess_policy::scratch_directory().unwrap();
        let path = directory.path().join("document.pdf");
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = write_scratch_input(
            directory,
            path.clone(),
            b"%PDF-fixture".to_vec(),
            1,
            cancelled,
        )
        .await
        .unwrap_err();
        assert!(error.retryable && error.message == "job processing cancelled");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn subprocess_output_remains_bounded() {
        let error = read_limited(&b"12345"[..], 4).await.unwrap_err();
        assert!(!error.retryable);
        assert_eq!(read_limited(&b"1234"[..], 4).await.unwrap(), b"1234");
    }

    #[tokio::test]
    async fn cancelled_subprocess_is_killed_and_reaped_before_its_timeout() {
        let cancelled = CancellationToken::new();
        let token = cancelled.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            token.cancel();
        });
        let mut command = Command::new("sleep");
        command.arg("30");
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            crate::child_process::capture(command, (), Duration::from_secs(30), cancelled.clone()),
        )
        .await
        .expect("cancelled command must finish before its process timeout");
        let error = result.unwrap_err();
        assert!(error.retryable);
        assert_eq!(error.message, "job processing cancelled");
        canceller.await.unwrap();
    }

    #[tokio::test]
    async fn poppler_render_respects_cropbox_rotation_and_dpi_without_stretching() {
        // Keep unit tests portable to developer machines without Poppler. The
        // deployment image installs it; failures after successful discovery
        // remain test failures.
        let directory = Arc::new(tempfile::tempdir().unwrap());
        if (PdfTool::Info {
            input: &directory.path().join("probe.pdf"),
            page: None,
        })
        .command(directory.path())
        .is_err()
        {
            return;
        }
        for (name, page, expected) in [
            (
                "rotated",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /CropBox [10 10 82 154] /Rotate 90 /Contents 4 0 R >>",
                (400, 200),
            ),
            (
                "large",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 3000 600] /Contents 4 0 R >>",
                (3500, 700),
            ),
        ] {
            let objects = [
                "<< /Type /Catalog /Pages 2 0 R >>",
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                page,
                "<< /Length 0 >>\nstream\n\nendstream",
            ];
            let mut pdf = b"%PDF-1.4\n".to_vec();
            let mut offsets = Vec::new();
            for (index, object) in objects.iter().enumerate() {
                offsets.push(pdf.len());
                pdf.extend_from_slice(
                    format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes(),
                );
            }
            let xref = pdf.len();
            pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
            for offset in offsets {
                pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            pdf.extend_from_slice(
                format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                    .as_bytes(),
            );
            let path = directory.path().join(format!("{name}.pdf"));
            tokio::fs::write(&path, pdf).await.unwrap();
            let metadata = PdfTool::Info {
                input: &path,
                page: Some(1),
            }
            .command(directory.path())
            .unwrap();
            let output = run_pdf_command(
                metadata,
                directory.clone(),
                None,
                10,
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let (width, height) =
                parse_pdf_page_size(std::str::from_utf8(&output).unwrap(), 1).unwrap();
            let dpi = bounded_pdf_dpi(width, height, 200, 3500, 40_000_000).unwrap();
            let prefix = directory.path().join(name);
            let render = PdfTool::Render {
                input: &path,
                prefix: &prefix,
                page: 1,
                dpi,
                scale_to: None,
                cropbox: true,
            }
            .command(directory.path())
            .unwrap();
            run_pdf_command(
                render,
                directory.clone(),
                None,
                10,
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let encoded = read_file_limited(&prefix.with_extension("png"), 40_000_000)
                .await
                .unwrap();
            let rgb = normalize_rgb_image(&encoded, 40_000_000, 3500).unwrap();
            assert_eq!(rgb.dimensions(), expected);
        }
    }
}
