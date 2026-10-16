use std::{
    future::Future,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use image::{ImageFormat, ImageReader, RgbImage};
use ocr_extraction::SourceBlock;
use tempfile::TempDir;
use tokio::{
    io::AsyncReadExt,
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use crate::{
    ProcessingError, child_process,
    parser_protocol::{
        ErrorCode, Frame, FrameKind, MAX_SESSION_TIMEOUT_SECONDS, Operation, REQUEST_DATA_LIMIT,
        RESPONSE_DATA_LIMIT, RejectReason, RenderProfile, read_frame, write_frame,
    },
    preparation,
    subprocess_policy::PdfTool,
};

pub const DEFAULT_MAX_SESSIONS: u32 = 8;
const MAX_SESSIONS_LIMIT: u32 = 64;
// Health checks and admission replies must not wait behind document sessions.
const HANDSHAKE_CONNECTIONS: usize = 4;
const MAX_NATIVE_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_IMAGE_EDGE: u32 = 8192;
const MAX_PAGE_COUNT: u32 = 1000;
const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Frame timeout bounds a started request; the session deadline bounds idle
/// time between requests, which spans model inference in the worker.
#[derive(Clone, Copy, Debug)]
pub struct ServiceLimits {
    pub max_sessions: u32,
    pub frame_timeout: Duration,
    pub session_grace: Duration,
}

impl Default for ServiceLimits {
    fn default() -> Self {
        Self {
            max_sessions: DEFAULT_MAX_SESSIONS,
            frame_timeout: Duration::from_secs(10),
            session_grace: Duration::from_secs(60),
        }
    }
}

impl ServiceLimits {
    pub fn from_env() -> Result<Self, ProcessingError> {
        let max_sessions = match std::env::var("OCR_PARSER_MAX_SESSIONS") {
            Ok(value) => value
                .parse::<u32>()
                .ok()
                .filter(|sessions| (1..=MAX_SESSIONS_LIMIT).contains(sessions))
                .ok_or_else(|| {
                    ProcessingError::permanent("OCR_PARSER_MAX_SESSIONS must be 1..=64")
                })?,
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_SESSIONS,
            Err(_) => {
                return Err(ProcessingError::permanent(
                    "OCR_PARSER_MAX_SESSIONS must be valid Unicode",
                ));
            }
        };
        Ok(Self {
            max_sessions,
            ..Self::default()
        })
    }
}

#[derive(Clone, Copy)]
struct RenderLimits {
    max_pixels: u64,
    max_image_edge: u32,
    render_dpi: u32,
    page_max_edge: u32,
    timeout: Duration,
}

struct ParserDocument {
    directory: Arc<TempDir>,
    input_path: PathBuf,
    media_type: String,
    pages: u32,
    limits: RenderLimits,
}

pub async fn serve(
    socket_path: &Path,
    scratch_root: &Path,
    limits: ServiceLimits,
    shutdown: CancellationToken,
) -> Result<(), ProcessingError> {
    if !socket_path.is_absolute() {
        return Err(ProcessingError::permanent(
            "parser socket path must be absolute",
        ));
    }
    if !(1..=MAX_SESSIONS_LIMIT).contains(&limits.max_sessions) {
        return Err(ProcessingError::permanent(
            "parser session capacity must be 1..=64",
        ));
    }
    validate_private_directory(scratch_root, "parser scratch")?;
    validate_socket_directory(
        socket_path
            .parent()
            .ok_or_else(|| ProcessingError::permanent("parser socket path has no parent"))?,
    )?;
    prepare_socket_path(socket_path).await?;
    let listener = UnixListener::bind(socket_path)
        .map_err(|_| ProcessingError::transient("cannot bind isolated parser socket"))?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))
        .map_err(|_| ProcessingError::transient("cannot secure isolated parser socket"))?;
    let sessions = Arc::new(Semaphore::new(limits.max_sessions as usize));
    let connections = Arc::new(Semaphore::new(
        limits.max_sessions as usize + HANDSHAKE_CONNECTIONS,
    ));
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_err() {
                    tracing::warn!("isolated parser session task failed");
                }
            }
            accepted = listener.accept(), if connections.available_permits() > 0 => {
                let (stream, _) = accepted
                    .map_err(|_| ProcessingError::transient("parser socket accept failed"))?;
                let Ok(connection) = connections.clone().try_acquire_owned() else {
                    continue;
                };
                let stop = shutdown.clone();
                let scratch_root = scratch_root.to_owned();
                let sessions = sessions.clone();
                tasks.spawn(async move {
                    let _connection = connection;
                    handle_connection(stream, &scratch_root, sessions, limits, stop).await
                });
            }
        }
    }

    drop(listener);
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            tracing::warn!("isolated parser session task failed during shutdown");
        }
    }
    remove_socket_if_owned(socket_path);
    Ok(())
}

pub fn socket_path_from_env() -> Result<PathBuf, ProcessingError> {
    match std::env::var("OCR_PARSER_SOCKET_PATH") {
        Ok(value) if value.starts_with('/') => Ok(PathBuf::from(value)),
        Ok(_) => Err(ProcessingError::permanent(
            "OCR_PARSER_SOCKET_PATH must be absolute",
        )),
        Err(std::env::VarError::NotPresent) => Ok(PathBuf::from("/run/ocr-parser/parser.sock")),
        Err(_) => Err(ProcessingError::permanent(
            "OCR_PARSER_SOCKET_PATH must be valid Unicode",
        )),
    }
}

pub fn scratch_root_from_env() -> Result<PathBuf, ProcessingError> {
    match std::env::var("OCR_PARSER_SCRATCH_DIR") {
        Ok(value) if value.starts_with('/') => Ok(PathBuf::from(value)),
        Ok(_) => Err(ProcessingError::permanent(
            "OCR_PARSER_SCRATCH_DIR must be absolute",
        )),
        Err(std::env::VarError::NotPresent) => Ok(PathBuf::from("/scratch")),
        Err(_) => Err(ProcessingError::permanent(
            "OCR_PARSER_SCRATCH_DIR must be valid Unicode",
        )),
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    scratch_root: &Path,
    sessions: Arc<Semaphore>,
    service: ServiceLimits,
    shutdown: CancellationToken,
) -> Result<(), ProcessingError> {
    let handshake = Instant::now() + service.frame_timeout;
    let first = read_request(&mut stream, handshake, service.frame_timeout, &shutdown).await?;
    match first.operation {
        Operation::HealthCheck => {
            let active = service
                .max_sessions
                .saturating_sub(sessions.available_permits() as u32);
            write_response(
                &mut stream,
                Operation::HealthReady {
                    max_sessions: service.max_sessions,
                    active_sessions: active,
                },
                Vec::new(),
            )
            .await
        }
        Operation::Open {
            media_type,
            max_pages,
            max_pixels,
            max_image_edge,
            render_dpi,
            page_max_edge,
            timeout_seconds,
            session_timeout_seconds,
        } => {
            let limits = match validate_limits(
                max_pages,
                max_pixels,
                max_image_edge,
                render_dpi,
                page_max_edge,
                timeout_seconds,
                session_timeout_seconds,
            ) {
                Ok(limits) => limits,
                Err(error) => return reject(&mut stream, error).await,
            };
            let Ok(_session) = sessions.try_acquire_owned() else {
                return write_response(
                    &mut stream,
                    Operation::Error {
                        code: ErrorCode::Busy,
                    },
                    Vec::new(),
                )
                .await;
            };
            let deadline = Instant::now()
                + Duration::from_secs(session_timeout_seconds)
                + service.session_grace;
            write_response(&mut stream, Operation::Admitted, Vec::new()).await?;
            let upload = read_request(
                &mut stream,
                Instant::now() + service.frame_timeout,
                service.frame_timeout,
                &shutdown,
            )
            .await?;
            if upload.operation != Operation::Upload {
                return reject(&mut stream, protocol_violation()).await;
            }
            let cancelled = CancellationToken::new();
            let open = open_document(
                scratch_root,
                &media_type,
                upload.data,
                max_pages,
                limits,
                cancelled.clone(),
            );
            let document = match while_connected(&stream, &shutdown, &cancelled, open).await {
                Ok(document) => document,
                Err(error) => return reject(&mut stream, error).await,
            };
            write_response(
                &mut stream,
                Operation::Opened {
                    pages: document.pages,
                },
                Vec::new(),
            )
            .await?;
            serve_document(&mut stream, document, deadline, service, shutdown).await
        }
        _ => reject(&mut stream, protocol_violation()).await,
    }
}

/// Waits for the next request until the session deadline, then requires the
/// whole frame within one frame timeout so a stalled writer cannot pin a slot.
async fn read_request(
    stream: &mut UnixStream,
    idle_deadline: Instant,
    frame_timeout: Duration,
    shutdown: &CancellationToken,
) -> Result<Frame, ProcessingError> {
    let first = tokio::select! {
        biased;
        _ = shutdown.cancelled() => return Err(ProcessingError::transient("parser service is stopping")),
        byte = timeout_at(idle_deadline, stream.read_u8()) => byte
            .map_err(|_| ProcessingError::transient("parser session deadline elapsed"))?
            .map_err(|_| ProcessingError::transient("parser session disconnected"))?,
    };
    let prefix = [first];
    let mut reader = (&prefix[..]).chain(&mut *stream);
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err(ProcessingError::transient("parser service is stopping")),
        frame = timeout(frame_timeout, read_frame(&mut reader, FrameKind::Request, REQUEST_DATA_LIMIT)) => frame
            .map_err(|_| ProcessingError::transient("parser IPC frame timed out"))?
            .map_err(|_| ProcessingError::transient("invalid parser IPC request frame")),
    }
}

async fn reject(stream: &mut UnixStream, error: ProcessingError) -> Result<(), ProcessingError> {
    let code = ErrorCode::from_error(&error);
    let _ = write_response(stream, Operation::Error { code }, Vec::new()).await;
    Err(error)
}

fn protocol_violation() -> ProcessingError {
    ProcessingError::permanent(RejectReason::ProtocolViolation.message())
}

async fn serve_document(
    stream: &mut UnixStream,
    document: ParserDocument,
    deadline: Instant,
    service: ServiceLimits,
    shutdown: CancellationToken,
) -> Result<(), ProcessingError> {
    loop {
        let frame = read_request(stream, deadline, service.frame_timeout, &shutdown).await?;
        if !frame.data.is_empty() {
            return Err(ProcessingError::permanent(
                "parser operation does not accept a data payload",
            ));
        }
        if frame.operation == Operation::Close {
            write_response(stream, Operation::Closed, Vec::new()).await?;
            return Ok(());
        }
        let cancelled = CancellationToken::new();
        let result = match frame.operation {
            Operation::PageInfo { page } => while_connected(
                stream,
                &shutdown,
                &cancelled,
                page_info(&document, page, cancelled.clone()),
            )
            .await
            .map(|(width_points, height_points)| {
                (
                    Operation::PageInfoResult {
                        width_points,
                        height_points,
                    },
                    Vec::new(),
                )
            }),
            Operation::RenderRgb { page, profile } => while_connected(
                stream,
                &shutdown,
                &cancelled,
                render_rgb(&document, page, profile, cancelled.clone()),
            )
            .await
            .and_then(rgb_frame),
            Operation::NativeText => while_connected(
                stream,
                &shutdown,
                &cancelled,
                native_text(&document, cancelled.clone()),
            )
            .await
            .and_then(native_text_frame),
            _ => Err(protocol_violation()),
        };
        match result {
            Ok((operation, data)) => write_response(stream, operation, data).await?,
            Err(error) => {
                let code = ErrorCode::from_error(&error);
                write_response(stream, Operation::Error { code }, Vec::new()).await?;
                if error.retryable {
                    return Err(error);
                }
            }
        }
    }
}

async fn open_document(
    scratch_root: &Path,
    media_type: &str,
    input: Vec<u8>,
    max_pages: u32,
    limits: RenderLimits,
    cancelled: CancellationToken,
) -> Result<ParserDocument, ProcessingError> {
    let directory = tempfile::tempdir_in(scratch_root)
        .map_err(|_| ProcessingError::transient("cannot create parser scratch"))?;
    let directory = Arc::new(directory);
    let filename = match media_type {
        "application/pdf" => {
            if !input.starts_with(b"%PDF-") {
                return Err(ProcessingError::permanent(
                    "input does not have a PDF header",
                ));
            }
            "document.pdf"
        }
        "image/png" => "document.png",
        "image/jpeg" => "document.jpg",
        "image/webp" => "document.webp",
        _ => {
            return Err(ProcessingError::permanent(
                "supported media types are PDF, PNG, JPEG, and WebP",
            ));
        }
    };
    let input_path = directory.path().join(filename);
    preparation::write_scratch_input(
        directory.clone(),
        input_path.clone(),
        input,
        limits.timeout.as_secs(),
        cancelled.clone(),
    )
    .await?;

    let pages = if media_type == "application/pdf" {
        let command = PdfTool::Info {
            input: &input_path,
            page: None,
        }
        .command(directory.path())?;
        let output = run_pdf_command(command, directory.clone(), None, limits, cancelled).await?;
        let text = std::str::from_utf8(&output)
            .map_err(|_| ProcessingError::permanent("PDF metadata is not valid UTF-8"))?;
        preparation::parse_pdf_pages(text, max_pages)?
    } else {
        let expected = match media_type {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            _ => ImageFormat::WebP,
        };
        let bytes = read_file_limited(&input_path, REQUEST_DATA_LIMIT).await?;
        let actual = image::guess_format(&bytes)
            .map_err(|_| ProcessingError::permanent("input is not a supported image"))?;
        if actual != expected {
            return Err(ProcessingError::permanent(
                "image bytes do not match the declared media type",
            ));
        }
        let dimensions = ImageReader::with_format(std::io::Cursor::new(bytes), actual)
            .into_dimensions()
            .map_err(|_| ProcessingError::permanent("cannot read image dimensions"))?;
        if dimensions.0 == 0
            || dimensions.1 == 0
            || u64::from(dimensions.0) * u64::from(dimensions.1) > limits.max_pixels
        {
            return Err(ProcessingError::permanent(format!(
                "image exceeds the {} pixel limit",
                limits.max_pixels
            )));
        }
        1
    };
    Ok(ParserDocument {
        directory,
        input_path,
        media_type: media_type.to_owned(),
        pages,
        limits,
    })
}

async fn page_info(
    document: &ParserDocument,
    page: u32,
    cancelled: CancellationToken,
) -> Result<(f64, f64), ProcessingError> {
    validate_page(document, page)?;
    if document.media_type != "application/pdf" {
        return Err(ProcessingError::permanent("page metadata is PDF-only"));
    }
    let command = PdfTool::Info {
        input: &document.input_path,
        page: Some(page),
    }
    .command(document.directory.path())?;
    let output = run_pdf_command(
        command,
        document.directory.clone(),
        None,
        document.limits,
        cancelled,
    )
    .await?;
    let text = std::str::from_utf8(&output)
        .map_err(|_| ProcessingError::permanent("PDF metadata is not valid UTF-8"))?;
    preparation::parse_pdf_page_size(text, page)
}

async fn render_rgb(
    document: &ParserDocument,
    page: u32,
    profile: RenderProfile,
    cancelled: CancellationToken,
) -> Result<RgbImage, ProcessingError> {
    validate_page(document, page)?;
    let edge = match profile {
        RenderProfile::FullPage => document.limits.max_image_edge,
        RenderProfile::CourseLayout => document.limits.page_max_edge,
    };
    if document.media_type == "application/pdf" {
        let (dpi, scale_to, cropbox) = match profile {
            RenderProfile::FullPage => (144.0, Some(document.limits.max_image_edge), false),
            RenderProfile::CourseLayout => {
                let (width, height) = page_info(document, page, cancelled.clone()).await?;
                (
                    preparation::bounded_pdf_dpi(
                        width,
                        height,
                        document.limits.render_dpi,
                        document.limits.page_max_edge,
                        document.limits.max_pixels,
                    )?,
                    None,
                    true,
                )
            }
        };
        let prefix = document.directory.path().join(format!("page-{page}"));
        let output_path = prefix.with_extension("png");
        let command = PdfTool::Render {
            input: &document.input_path,
            prefix: &prefix,
            page,
            dpi,
            scale_to,
            cropbox,
        }
        .command(document.directory.path())?;
        let raster = Arc::new(preparation::ScratchOutput::new(output_path.clone()));
        run_pdf_command(
            command,
            document.directory.clone(),
            Some(raster.clone()),
            document.limits,
            cancelled.clone(),
        )
        .await?;
        let max_bytes = document
            .limits
            .max_pixels
            .saturating_mul(4)
            .saturating_add(1_048_576);
        let read = read_file_limited(&output_path, max_bytes).await;
        let removed = tokio::fs::remove_file(&output_path).await;
        let bytes = read?;
        removed.map_err(|_| ProcessingError::transient("cannot remove parser raster"))?;
        drop(raster);
        decode_rgb(
            bytes,
            document.limits.max_pixels,
            edge,
            document.directory.clone(),
            cancelled,
        )
        .await
    } else {
        let bytes = read_file_limited(&document.input_path, REQUEST_DATA_LIMIT).await?;
        decode_rgb(
            bytes,
            document.limits.max_pixels,
            edge,
            document.directory.clone(),
            cancelled,
        )
        .await
    }
}

async fn native_text(
    document: &ParserDocument,
    cancelled: CancellationToken,
) -> Result<Vec<SourceBlock>, ProcessingError> {
    if document.media_type != "application/pdf" {
        return Ok(Vec::new());
    }
    let command = PdfTool::Text {
        input: &document.input_path,
    }
    .command(document.directory.path())?;
    let output = child_process::capture(
        command,
        document.directory.clone(),
        document.limits.timeout,
        cancelled,
    )
    .await?;
    crate::post_processing::parse_native_xml(
        std::str::from_utf8(&output)
            .map_err(|_| ProcessingError::permanent("native PDF text is not UTF-8"))?,
    )
}

async fn decode_rgb(
    bytes: Vec<u8>,
    max_pixels: u64,
    max_edge: u32,
    directory: Arc<TempDir>,
    cancelled: CancellationToken,
) -> Result<RgbImage, ProcessingError> {
    let permit = crate::permit_or_cancelled(
        &DECODE_SLOTS,
        &cancelled,
        "parser decode admission is stopping",
    )
    .await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _directory = directory;
        if let Some(error) = crate::cancelled_error(&cancelled) {
            return Err(error);
        }
        let rgb = preparation::normalize_rgb_image(&bytes, max_pixels, max_edge)?;
        if let Some(error) = crate::cancelled_error(&cancelled) {
            return Err(error);
        }
        Ok(rgb)
    })
    .await
    .map_err(|_| ProcessingError::transient("parser image decode task failed"))?
}

fn validate_page(document: &ParserDocument, page: u32) -> Result<(), ProcessingError> {
    if page == 0 || page > document.pages {
        Err(ProcessingError::permanent("invalid document page number"))
    } else {
        Ok(())
    }
}

static DECODE_SLOTS: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(1)));

fn validate_limits(
    max_pages: u32,
    max_pixels: u64,
    max_image_edge: u32,
    render_dpi: u32,
    page_max_edge: u32,
    timeout_seconds: u64,
    session_timeout_seconds: u64,
) -> Result<RenderLimits, ProcessingError> {
    if max_pages == 0
        || max_pages > MAX_PAGE_COUNT
        || max_pixels == 0
        || max_pixels > MAX_IMAGE_PIXELS
        || max_pixels > RESPONSE_DATA_LIMIT / 3
        || !(32..=MAX_IMAGE_EDGE).contains(&max_image_edge)
        || !(36..=600).contains(&render_dpi)
        || !(32..=MAX_IMAGE_EDGE).contains(&page_max_edge)
        || timeout_seconds == 0
        || timeout_seconds > 600
        || !(1..=MAX_SESSION_TIMEOUT_SECONDS).contains(&session_timeout_seconds)
    {
        return Err(ProcessingError::permanent(
            RejectReason::InvalidLimits.message(),
        ));
    }
    Ok(RenderLimits {
        max_pixels,
        max_image_edge,
        render_dpi,
        page_max_edge,
        timeout: Duration::from_secs(timeout_seconds),
    })
}

async fn run_pdf_command(
    command: tokio::process::Command,
    directory: Arc<TempDir>,
    output: Option<Arc<preparation::ScratchOutput>>,
    limits: RenderLimits,
    cancelled: CancellationToken,
) -> Result<Vec<u8>, ProcessingError> {
    preparation::run_pdf_command(
        command,
        directory,
        output,
        limits.timeout.as_secs(),
        cancelled,
    )
    .await
}

async fn read_file_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ProcessingError> {
    preparation::read_file_limited(path, max_bytes).await
}

async fn write_response(
    stream: &mut UnixStream,
    operation: Operation,
    data: Vec<u8>,
) -> Result<(), ProcessingError> {
    timeout(
        IPC_WRITE_TIMEOUT,
        write_frame(
            stream,
            FrameKind::Response,
            &Frame { operation, data },
            RESPONSE_DATA_LIMIT,
        ),
    )
    .await
    .map_err(|_| ProcessingError::transient("parser IPC response timed out"))?
    .map_err(|_| ProcessingError::transient("cannot write parser IPC response"))
}

async fn while_connected<T, F>(
    stream: &UnixStream,
    shutdown: &CancellationToken,
    cancelled: &CancellationToken,
    operation: F,
) -> Result<T, ProcessingError>
where
    F: Future<Output = Result<T, ProcessingError>>,
{
    tokio::pin!(operation);
    let mut probe = [0_u8; 1];
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                cancelled.cancel();
                return Err(ProcessingError::transient("parser service is stopping"));
            }
            _ = cancelled.cancelled() => return Err(ProcessingError::transient("parser operation cancelled")),
            result = &mut operation => return result,
            ready = stream.readable() => {
                ready.map_err(|_| ProcessingError::transient("parser client disconnected"))?;
                match stream.try_read(&mut probe) {
                    Ok(0) => {
                        cancelled.cancel();
                        return Err(ProcessingError::transient("parser client disconnected"));
                    }
                    Ok(_) => {
                        cancelled.cancel();
                        return Err(ProcessingError::permanent("parser client pipelined requests"));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(_) => {
                        cancelled.cancel();
                        return Err(ProcessingError::transient("parser client disconnected"));
                    }
                }
            }
        }
    }
}

fn rgb_frame(image: RgbImage) -> Result<(Operation, Vec<u8>), ProcessingError> {
    let width = image.width();
    let height = image.height();
    let data = image.into_raw();
    if data.len() as u64 > RESPONSE_DATA_LIMIT {
        return Err(ProcessingError::permanent("RGB output limit exceeded"));
    }
    Ok((Operation::RgbResult { width, height }, data))
}

fn native_text_frame(blocks: Vec<SourceBlock>) -> Result<(Operation, Vec<u8>), ProcessingError> {
    let data = serde_json::to_vec(&blocks)
        .map_err(|_| ProcessingError::permanent("cannot encode native text response"))?;
    if data.len() > MAX_NATIVE_JSON_BYTES {
        return Err(ProcessingError::permanent(
            "native text output limit exceeded",
        ));
    }
    Ok((Operation::NativeTextResult, data))
}

async fn prepare_socket_path(socket_path: &Path) -> Result<(), ProcessingError> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| ProcessingError::permanent("parser socket path has no parent"))?;
    validate_socket_directory(parent)?;
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            match timeout(Duration::from_millis(250), UnixStream::connect(socket_path)).await {
                Ok(Ok(_)) => Err(ProcessingError::permanent(
                    "an isolated parser is already listening on this socket",
                )),
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    std::fs::remove_file(socket_path).map_err(|_| {
                        ProcessingError::transient("cannot remove stale parser socket")
                    })
                }
                Ok(Err(_)) | Err(_) => Err(ProcessingError::transient(
                    "cannot verify existing parser socket",
                )),
            }
        }
        Ok(_) => Err(ProcessingError::permanent(
            "parser socket path is occupied by a non-socket file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ProcessingError::transient(
            "cannot inspect parser socket path",
        )),
    }
}

fn validate_private_directory(path: &Path, purpose: &str) -> Result<(), ProcessingError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| ProcessingError::permanent("parser private directory is unavailable"))?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ProcessingError::permanent(format!(
            "{purpose} directory must be owned by the parser and mode 0700"
        )));
    }
    Ok(())
}

fn validate_socket_directory(path: &Path) -> Result<(), ProcessingError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| ProcessingError::permanent("parser socket directory is unavailable"))?;
    let mode = metadata.permissions().mode();
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.gid() != unsafe { libc::getegid() }
        || mode & 0o700 != 0o700
        || mode & 0o010 == 0
        || mode & 0o067 != 0
        || mode & 0o5000 != 0
    {
        return Err(ProcessingError::permanent(
            "parser socket directory must be parser-owned and worker-traverse-only",
        ));
    }
    Ok(())
}

fn remove_socket_if_owned(socket_path: &Path) {
    if std::fs::symlink_metadata(socket_path).is_ok_and(|metadata| metadata.file_type().is_socket())
    {
        let _ = std::fs::remove_file(socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_protocol::RenderProfile;
    use crate::settings::Settings;
    use tokio::task::JoinHandle;

    struct TestServer {
        _directory: TempDir,
        path: PathBuf,
        scratch_root: PathBuf,
        shutdown: CancellationToken,
        task: JoinHandle<Result<(), ProcessingError>>,
    }

    impl TestServer {
        async fn start() -> Self {
            Self::start_with(ServiceLimits::default()).await
        }

        async fn start_with(limits: ServiceLimits) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let socket_root = directory.path().join("socket");
            std::fs::create_dir(&socket_root).unwrap();
            std::fs::set_permissions(&socket_root, std::fs::Permissions::from_mode(0o2710))
                .unwrap();
            let path = socket_root.join("parser.sock");
            let scratch_root = directory.path().join("scratch");
            std::fs::create_dir(&scratch_root).unwrap();
            std::fs::set_permissions(&scratch_root, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let shutdown = CancellationToken::new();
            let service_path = path.clone();
            let service_scratch = scratch_root.clone();
            let service_shutdown = shutdown.clone();
            let mut task = tokio::spawn(async move {
                serve(&service_path, &service_scratch, limits, service_shutdown).await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                while !path.exists() {
                    if task.is_finished() {
                        match (&mut task).await {
                            Ok(Ok(())) => panic!("parser service exited before bind"),
                            Ok(Err(error)) => panic!("parser service failed before bind: {error}"),
                            Err(error) => panic!("parser service task failed before bind: {error}"),
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            Self {
                _directory: directory,
                path,
                scratch_root,
                shutdown,
                task,
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown.cancel();
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn socket_accepts_health_then_closes_without_document_state() {
        let server = TestServer::start().await;
        let capacity = crate::parser_client::health_check(&server.path, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(capacity.max_sessions, DEFAULT_MAX_SESSIONS);
        assert_eq!(capacity.active_sessions, 0);
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(width, height, image::Rgb([255; 3]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    async fn open_png(
        server: &TestServer,
        limits: crate::parser_client::ParserLimits,
    ) -> Result<crate::parser_client::ParserSession, ProcessingError> {
        crate::parser_client::ParserSession::open(
            &server.path,
            png(8, 4),
            "image/png",
            limits,
            CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn session_survives_idle_gaps_longer_than_a_frame_until_its_deadline() {
        let server = TestServer::start_with(ServiceLimits {
            frame_timeout: Duration::from_millis(200),
            session_grace: Duration::ZERO,
            ..ServiceLimits::default()
        })
        .await;
        let limits = crate::parser_client::ParserLimits {
            session_timeout_seconds: 1,
            ..Default::default()
        };
        let session = open_png(&server, limits).await.unwrap();
        // Model inference between parser calls is far longer than one frame.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let page = session
            .render_rgb(1, RenderProfile::FullPage, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.dimensions(), (8, 4));

        tokio::time::sleep(Duration::from_millis(700)).await;
        let expired = session
            .render_rgb(1, RenderProfile::FullPage, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(expired.retryable, "{expired}");
        let capacity = crate::parser_client::health_check(&server.path, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(capacity.active_sessions, 0);
    }

    #[tokio::test]
    async fn stalled_partial_frame_is_closed_after_one_frame_timeout() {
        use tokio::io::AsyncWriteExt;

        let server = TestServer::start_with(ServiceLimits {
            frame_timeout: Duration::from_millis(200),
            ..ServiceLimits::default()
        })
        .await;
        let mut stream = UnixStream::connect(&server.path).await.unwrap();
        stream.write_all(b"OCR").await.unwrap();
        let mut buffer = [0_u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
            .await
            .expect("stalled request must not pin a parser connection");
        assert_eq!(closed.unwrap(), 0);
    }

    #[tokio::test]
    async fn health_reports_capacity_when_full_and_busy_open_waits_for_a_slot() {
        let server = TestServer::start_with(ServiceLimits {
            max_sessions: 1,
            ..ServiceLimits::default()
        })
        .await;
        let first = open_png(&server, Default::default()).await.unwrap();
        let capacity = crate::parser_client::health_check(&server.path, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!((capacity.max_sessions, capacity.active_sessions), (1, 1));

        let short = crate::parser_client::ParserLimits {
            timeout_seconds: 1,
            ..Default::default()
        };
        let busy = open_png(&server, short).await.err().unwrap();
        assert_eq!(busy.message, "isolated parser is at capacity");
        assert!(busy.retryable);

        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(first);
        });
        let waited = open_png(&server, Default::default()).await.unwrap();
        assert_eq!(waited.page_count(), 1);
        release.await.unwrap();
    }

    #[tokio::test]
    async fn typed_errors_keep_canonical_messages_and_permanent_errors_keep_the_session() {
        let server = TestServer::start().await;
        let error = crate::parser_client::ParserSession::open(
            &server.path,
            b"not a pdf".to_vec(),
            "application/pdf",
            Default::default(),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.message, "input does not have a PDF header");
        assert!(!error.retryable);

        let mismatch = crate::parser_client::ParserSession::open(
            &server.path,
            png(2, 2),
            "image/jpeg",
            Default::default(),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            mismatch.message,
            "image bytes do not match the declared media type"
        );

        let session = open_png(&server, Default::default()).await.unwrap();
        let invalid = session
            .render_rgb(2, RenderProfile::FullPage, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(invalid.message, "invalid document page number");
        assert!(!invalid.retryable);
        session
            .render_rgb(1, RenderProfile::FullPage, CancellationToken::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejects_non_socket_path_instead_of_unlinking_it() {
        let directory = tempfile::tempdir().unwrap();
        let socket_root = directory.path().join("socket");
        std::fs::create_dir(&socket_root).unwrap();
        std::fs::set_permissions(&socket_root, std::fs::Permissions::from_mode(0o710)).unwrap();
        let path = socket_root.join("occupied");
        std::fs::write(&path, b"keep").unwrap();
        assert!(prepare_socket_path(&path).await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn refuses_to_unlink_an_active_parser_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket_root = directory.path().join("socket");
        std::fs::create_dir(&socket_root).unwrap();
        std::fs::set_permissions(&socket_root, std::fs::Permissions::from_mode(0o710)).unwrap();
        let path = socket_root.join("active.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(prepare_socket_path(&path).await.is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(listener);
    }

    #[tokio::test]
    async fn removes_only_a_stale_socket_after_bounded_probe() {
        let directory = tempfile::tempdir().unwrap();
        let socket_root = directory.path().join("socket");
        std::fs::create_dir(&socket_root).unwrap();
        std::fs::set_permissions(&socket_root, std::fs::Permissions::from_mode(0o710)).unwrap();
        let path = socket_root.join("stale.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(listener);
        prepare_socket_path(&path).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn socket_directory_rejects_group_write_and_world_access() {
        let directory = tempfile::tempdir().unwrap();
        for mode in [0o770, 0o711, 0o0700] {
            let path = directory.path().join(format!("socket-{mode:o}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                validate_socket_directory(&path).is_err(),
                "accepted {mode:o}"
            );
        }
    }

    #[tokio::test]
    async fn image_rgb_matches_existing_normalizer_for_alpha_orientation_and_resize() {
        let server = TestServer::start().await;
        let rgba = image::RgbaImage::from_fn(80, 40, |x, y| {
            image::Rgba([x as u8, y as u8, 127, if x < 40 { 255 } else { 0 }])
        });
        let mut input = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        let expected = preparation::normalize_rgb_image(input.get_ref(), 4000, 40).unwrap();
        let settings = test_settings(&server.path);
        let session = crate::parser_client::ParserSession::open(
            &server.path,
            input.into_inner(),
            "image/png",
            crate::parser_client::ParserLimits::from(&settings),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let actual = session
            .render_rgb(1, RenderProfile::CourseLayout, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(actual.dimensions(), expected.dimensions());
        assert_eq!(actual.as_raw(), expected.as_raw());
    }

    #[tokio::test]
    async fn disconnected_document_releases_scratch_and_next_session_succeeds() {
        let server = TestServer::start().await;
        let limits = crate::parser_client::ParserLimits::default();
        let session = crate::parser_client::ParserSession::open(
            &server.path,
            include_bytes!("../tests/fixtures/two_pages.pdf").to_vec(),
            "application/pdf",
            limits,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(session.page_count(), 2);
        drop(session);

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_dir(&server.scratch_root)
                    .unwrap()
                    .next()
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("disconnected parser session must release its scratch");

        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8, 4, image::Rgb([255; 3]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let next = crate::parser_client::ParserSession::open(
            &server.path,
            png.into_inner(),
            "image/png",
            limits,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let page = next
            .render_rgb(1, RenderProfile::FullPage, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.dimensions(), (8, 4));
    }

    #[tokio::test]
    async fn pdf_render_and_native_text_match_the_existing_poppler_semantics() {
        use crate::{
            child_process,
            parser_protocol::RenderProfile,
            subprocess_policy::{self, PdfTool},
        };

        let server = TestServer::start().await;
        let input = include_bytes!("../tests/fixtures/two_pages.pdf").to_vec();
        let limits = crate::parser_client::ParserLimits::default();
        let timeout = Duration::from_secs(limits.timeout_seconds);
        let session = crate::parser_client::ParserSession::open(
            &server.path,
            input.clone(),
            "application/pdf",
            limits,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(session.page_count(), 2);

        let directory = subprocess_policy::scratch_directory().unwrap();
        let input_path = directory.path().join("reference.pdf");
        preparation::write_scratch_input(
            directory.clone(),
            input_path.clone(),
            input,
            limits.timeout_seconds,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let info = PdfTool::Info {
            input: &input_path,
            page: None,
        }
        .command(directory.path())
        .unwrap();
        let info =
            child_process::capture(info, directory.clone(), timeout, CancellationToken::new())
                .await
                .unwrap();
        let info = String::from_utf8(info).unwrap();
        assert_eq!(
            preparation::parse_pdf_pages(&info, limits.max_pages).unwrap(),
            2
        );

        for page in 1..=2 {
            let (width_points, height_points) = session
                .page_info(page, CancellationToken::new())
                .await
                .unwrap();
            let dpi = preparation::bounded_pdf_dpi(
                width_points,
                height_points,
                limits.render_dpi,
                limits.page_max_edge,
                limits.max_pixels,
            )
            .unwrap();
            let prefix = directory.path().join(format!("reference-{page}"));
            let output = prefix.with_extension("png");
            let raster = Arc::new(preparation::ScratchOutput::new(output.clone()));
            let render = PdfTool::Render {
                input: &input_path,
                prefix: &prefix,
                page,
                dpi,
                scale_to: None,
                cropbox: true,
            }
            .command(directory.path())
            .unwrap();
            child_process::capture(
                render,
                (directory.clone(), raster.clone()),
                timeout,
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let encoded = read_file_limited(&output, limits.max_pixels * 4 + 1_048_576)
                .await
                .unwrap();
            drop(raster);
            let expected =
                preparation::normalize_rgb_image(&encoded, limits.max_pixels, limits.page_max_edge)
                    .unwrap();
            let actual = session
                .render_rgb(page, RenderProfile::CourseLayout, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(actual.dimensions(), expected.dimensions());
            assert_eq!(actual.as_raw(), expected.as_raw(), "page {page}");
        }

        let command = PdfTool::Text { input: &input_path }
            .command(directory.path())
            .unwrap();
        let xml = child_process::capture(command, directory, timeout, CancellationToken::new())
            .await
            .unwrap();
        let reference =
            crate::post_processing::parse_native_xml(std::str::from_utf8(&xml).unwrap()).unwrap();
        let native = session.native_text(CancellationToken::new()).await.unwrap();
        assert_eq!(
            serde_json::to_value(native).unwrap(),
            serde_json::to_value(reference).unwrap()
        );
    }

    fn test_settings(socket: &Path) -> Settings {
        Settings {
            database_url: "postgres://unused".into(),
            redis_url: "redis://localhost".into(),
            redis_stream: "test".into(),
            redis_consumer_group: "test".into(),
            redis_timeout_ms: 1000,
            redis_redispatch_seconds: 10,
            vllm_base_url: "http://127.0.0.1:8000/v1".into(),
            vllm_model: "test".into(),
            vllm_api_key: None,
            worker_concurrency: 1,
            lease_seconds: 30,
            max_attempts: 1,
            poll_interval_ms: 100,
            request_timeout_seconds: 10,
            job_timeout_seconds: 30,
            max_pages: 4,
            max_image_pixels: 4000,
            max_image_edge: 40,
            table_crop_padding_pixels: 0,
            pdf_render_dpi: 144,
            page_max_edge: 40,
            strategy: crate::settings::OcrStrategy::CourseLayout,
            layout_binary: "layout-detect".into(),
            layout_model_path: "/models/layout/model.onnx".into(),
            layout_model_sha256: "a".repeat(64),
            layout_timeout_seconds: 10,
            layout_pool_size: 1,
            max_regions_per_page: 10,
            max_regions_per_document: 20,
            region_concurrency: 1,
            inference_revision: String::new(),
            inference_deployment: None,
            checkpoint_enabled: false,
            recovery_max_regions: 0,
            recovery_expansion_pixels: 16,
            recognize_image_regions: false,
            pdf_render_timeout_seconds: 3,
            max_output_tokens: 128,
            retention_days: 7,
            parser_socket_path: socket.to_path_buf(),
        }
    }
}
