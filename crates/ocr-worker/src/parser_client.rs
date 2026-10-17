use std::{collections::HashSet, path::Path, time::Duration};

use image::RgbImage;
use ocr_extraction::SourceBlock;
use sha2::{Digest, Sha256};
use tokio::{
    net::UnixStream,
    sync::{Mutex, MutexGuard},
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    ProcessingError,
    parser_protocol::{
        ErrorCode, Frame, FrameKind, Operation, REQUEST_DATA_LIMIT, RESPONSE_DATA_LIMIT,
        RenderProfile, read_frame, write_frame,
    },
    settings::Settings,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const ADMISSION_BACKOFF_START: Duration = Duration::from_millis(100);
const ADMISSION_BACKOFF_MAX: Duration = Duration::from_secs(1);
const NATIVE_BLOCKS_LIMIT: usize = 10_000;
const NATIVE_TEXT_LIMIT: usize = 4 * 1024 * 1024;

pub struct ParserSession {
    stream: Mutex<Option<UnixStream>>,
    pages: u32,
    timeout: Duration,
    source_sha256: String,
    max_pixels: u64,
    max_image_edge: u32,
    page_max_edge: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct ParserLimits {
    pub max_pages: u32,
    pub max_pixels: u64,
    pub max_image_edge: u32,
    pub render_dpi: u32,
    pub page_max_edge: u32,
    pub timeout_seconds: u64,
    /// Upper bound for the whole session; the parser closes it afterwards.
    pub session_timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParserCapacity {
    pub max_sessions: u32,
    pub active_sessions: u32,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_pages: 20,
            max_pixels: 40_000_000,
            max_image_edge: 2000,
            render_dpi: 200,
            page_max_edge: 3500,
            timeout_seconds: 30,
            session_timeout_seconds: 600,
        }
    }
}

impl From<&Settings> for ParserLimits {
    fn from(settings: &Settings) -> Self {
        Self {
            max_pages: settings.max_pages,
            max_pixels: settings.max_image_pixels,
            max_image_edge: settings.max_image_edge,
            render_dpi: settings.pdf_render_dpi,
            page_max_edge: settings.page_max_edge,
            timeout_seconds: settings.pdf_render_timeout_seconds,
            session_timeout_seconds: settings.job_timeout_seconds,
        }
    }
}

impl ParserSession {
    pub async fn open(
        socket_path: &Path,
        input: Vec<u8>,
        media_type: &str,
        limits: ParserLimits,
        cancelled: CancellationToken,
    ) -> Result<Self, ProcessingError> {
        if input.is_empty() || input.len() as u64 > REQUEST_DATA_LIMIT {
            return Err(ProcessingError::permanent(
                "document exceeds isolated parser input limit",
            ));
        }
        let source_sha256 = format!("{:x}", Sha256::digest(&input));
        let timeout = Duration::from_secs(limits.timeout_seconds);
        let operation = Operation::Open {
            media_type: media_type.to_owned(),
            max_pages: limits.max_pages,
            max_pixels: limits.max_pixels,
            max_image_edge: limits.max_image_edge,
            render_dpi: limits.render_dpi,
            page_max_edge: limits.page_max_edge,
            timeout_seconds: limits.timeout_seconds,
            session_timeout_seconds: limits.session_timeout_seconds,
        };
        let mut stream = admit(socket_path, operation, timeout, &cancelled).await?;
        let frame = exchange(
            &mut stream,
            Operation::Upload,
            input,
            timeout,
            &cancelled,
            REQUEST_DATA_LIMIT,
        )
        .await
        .and_then(operation_result)?;
        let Operation::Opened { pages } = frame.operation else {
            return Err(unexpected_response());
        };
        if pages == 0 || pages > limits.max_pages {
            return Err(unexpected_response());
        }
        Ok(Self {
            stream: Mutex::new(Some(stream)),
            pages,
            timeout,
            source_sha256,
            max_pixels: limits.max_pixels,
            max_image_edge: limits.max_image_edge,
            page_max_edge: limits.page_max_edge,
        })
    }

    pub fn page_count(&self) -> u32 {
        self.pages
    }

    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    pub async fn page_info(
        &self,
        page: u32,
        cancelled: CancellationToken,
    ) -> Result<(f64, f64), ProcessingError> {
        let frame = self
            .request(Operation::PageInfo { page }, cancelled)
            .await?;
        let Operation::PageInfoResult {
            width_points,
            height_points,
        } = frame.operation
        else {
            return Err(unexpected_response());
        };
        if !width_points.is_finite()
            || !height_points.is_finite()
            || width_points <= 0.0
            || height_points <= 0.0
        {
            return Err(unexpected_response());
        }
        Ok((width_points, height_points))
    }

    pub async fn render_rgb(
        &self,
        page: u32,
        profile: RenderProfile,
        cancelled: CancellationToken,
    ) -> Result<RgbImage, ProcessingError> {
        let frame = self
            .request(Operation::RenderRgb { page, profile }, cancelled)
            .await?;
        let Operation::RgbResult { width, height } = frame.operation else {
            return Err(unexpected_response());
        };
        let max_edge = match profile {
            RenderProfile::FullPage => self.max_image_edge,
            RenderProfile::CourseLayout => self.page_max_edge,
        };
        if width == 0
            || height == 0
            || width > max_edge
            || height > max_edge
            || u64::from(width) * u64::from(height) > self.max_pixels
        {
            return Err(unexpected_response());
        }
        RgbImage::from_raw(width, height, frame.data).ok_or_else(unexpected_response)
    }

    pub async fn native_text(
        &self,
        cancelled: CancellationToken,
    ) -> Result<Vec<SourceBlock>, ProcessingError> {
        let frame = self.request(Operation::NativeText, cancelled).await?;
        if frame.operation != Operation::NativeTextResult || frame.data.len() > NATIVE_TEXT_LIMIT {
            return Err(unexpected_response());
        }
        let blocks: Vec<SourceBlock> = serde_json::from_slice(&frame.data)
            .map_err(|_| ProcessingError::permanent("isolated parser response is invalid"))?;
        let mut ids = HashSet::with_capacity(blocks.len());
        if blocks.len() > NATIVE_BLOCKS_LIMIT
            || blocks.iter().any(|block| {
                let prefix = format!("native:p{}:row", block.page_number);
                let valid_id = block.id.strip_prefix(&prefix).is_some_and(|row| {
                    !row.is_empty() && row.bytes().all(|byte| byte.is_ascii_digit())
                });
                block.page_number == 0
                    || block.page_number > self.pages
                    || block.id.is_empty()
                    || block.id.len() > 64
                    || !valid_id
                    || !ids.insert(block.id.as_str())
                    || block.source != "native_pdf"
                    || block.region_index.is_some()
                    || block.detector_score.is_some()
                    || block.text.is_empty()
                    || block.text.len() > 128 * 1024
                    || block.text.chars().any(|character| character.is_control())
                    || !block.bbox_2d.is_some_and(|bbox| {
                        bbox.iter().all(|value| (0..=1000).contains(value))
                            && bbox[0] < bbox[2]
                            && bbox[1] < bbox[3]
                    })
            })
            || blocks.iter().map(|block| block.text.len()).sum::<usize>() > NATIVE_TEXT_LIMIT
        {
            return Err(unexpected_response());
        }
        Ok(blocks)
    }

    async fn request(
        &self,
        operation: Operation,
        cancelled: CancellationToken,
    ) -> Result<Frame, ProcessingError> {
        let mut stream = lock_stream(&self.stream, self.timeout, &cancelled).await?;
        let mut socket = match stream.take() {
            Some(socket) => socket,
            None => return Err(parser_unavailable()),
        };
        let frame = Frame {
            operation,
            data: Vec::new(),
        };
        let exchange = async {
            write_frame(&mut socket, FrameKind::Request, &frame, REQUEST_DATA_LIMIT)
                .await
                .map_err(protocol_error)?;
            read_frame(&mut socket, FrameKind::Response, RESPONSE_DATA_LIMIT)
                .await
                .map_err(protocol_error)
        };
        let result = tokio::select! {
            biased;
            _ = cancelled.cancelled() => Err(ProcessingError::cancelled()),
            result = tokio::time::timeout(self.timeout, exchange) => match result {
                Ok(result) => result,
                Err(_) => Err(ProcessingError::transient("isolated parser operation timed out")),
            },
        };
        match result {
            Ok(frame) => {
                let outcome = operation_result(frame);
                *stream = Some(socket);
                outcome
            }
            Err(error) => {
                drop(socket);
                Err(error)
            }
        }
    }
}

pub async fn health_check(
    socket_path: &Path,
    budget: Duration,
) -> Result<ParserCapacity, ProcessingError> {
    timeout(budget, async {
        let cancelled = CancellationToken::new();
        let mut stream = connect(socket_path, budget, &cancelled).await?;
        let frame = exchange(
            &mut stream,
            Operation::HealthCheck,
            Vec::new(),
            budget,
            &cancelled,
            REQUEST_DATA_LIMIT,
        )
        .await
        .and_then(operation_result)?;
        match frame.operation {
            Operation::HealthReady {
                max_sessions,
                active_sessions,
            } if max_sessions > 0 && active_sessions <= max_sessions => Ok(ParserCapacity {
                max_sessions,
                active_sessions,
            }),
            _ => Err(unexpected_response()),
        }
    })
    .await
    .map_err(|_| ProcessingError::transient("isolated parser operation timed out"))?
}

/// Busy is the parser's normal backpressure, so admission waits within the
/// operation budget instead of turning a full parser into a failed attempt.
async fn admit(
    socket_path: &Path,
    operation: Operation,
    budget: Duration,
    cancelled: &CancellationToken,
) -> Result<UnixStream, ProcessingError> {
    let deadline = Instant::now() + budget;
    let mut backoff = ADMISSION_BACKOFF_START;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut stream = connect(socket_path, remaining, cancelled).await?;
        let frame = exchange(
            &mut stream,
            operation.clone(),
            Vec::new(),
            remaining,
            cancelled,
            REQUEST_DATA_LIMIT,
        )
        .await?;
        match frame.operation {
            Operation::Admitted if frame.data.is_empty() => return Ok(stream),
            Operation::Error {
                code: ErrorCode::Busy,
            } if Instant::now() + backoff < deadline => {
                drop(stream);
                tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => return Err(ProcessingError::cancelled()),
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(ADMISSION_BACKOFF_MAX);
            }
            _ => {
                operation_result(frame)?;
                return Err(unexpected_response());
            }
        }
    }
}

async fn connect(
    socket_path: &Path,
    budget: Duration,
    cancelled: &CancellationToken,
) -> Result<UnixStream, ProcessingError> {
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(ProcessingError::cancelled()),
        result = timeout(budget.min(CONNECT_TIMEOUT), UnixStream::connect(socket_path)) => match result {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(_)) => Err(parser_unavailable()),
            Err(_) => Err(ProcessingError::transient("isolated parser connection timed out")),
        }
    }
}

async fn exchange(
    stream: &mut UnixStream,
    operation: Operation,
    data: Vec<u8>,
    budget: Duration,
    cancelled: &CancellationToken,
    data_limit: u64,
) -> Result<Frame, ProcessingError> {
    let request = Frame { operation, data };
    let exchange = async {
        write_frame(stream, FrameKind::Request, &request, data_limit)
            .await
            .map_err(protocol_error)?;
        read_frame(stream, FrameKind::Response, 0)
            .await
            .map_err(protocol_error)
    };
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(ProcessingError::cancelled()),
        result = timeout(budget, exchange) => match result {
            Ok(result) => result,
            Err(_) => Err(ProcessingError::transient("isolated parser operation timed out")),
        }
    }
}

async fn lock_stream<'a>(
    stream: &'a Mutex<Option<UnixStream>>,
    budget: Duration,
    cancelled: &CancellationToken,
) -> Result<MutexGuard<'a, Option<UnixStream>>, ProcessingError> {
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(ProcessingError::cancelled()),
        result = timeout(budget, stream.lock()) => result
            .map_err(|_| ProcessingError::transient("isolated parser admission timed out")),
    }
}

fn operation_result(frame: Frame) -> Result<Frame, ProcessingError> {
    if let Operation::Error { code } = frame.operation {
        return Err(code.into_error());
    }
    if !frame.data.is_empty()
        && !matches!(
            frame.operation,
            Operation::RgbResult { .. } | Operation::NativeTextResult
        )
    {
        return Err(unexpected_response());
    }
    Ok(frame)
}

fn protocol_error(_error: crate::parser_protocol::ProtocolError) -> ProcessingError {
    ProcessingError::transient("isolated parser IPC failed")
}

fn parser_unavailable() -> ProcessingError {
    ProcessingError::transient("isolated parser is unavailable")
}

fn unexpected_response() -> ProcessingError {
    ProcessingError::transient("isolated parser returned an invalid response")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;
    use tokio::{io::AsyncWriteExt, net::UnixListener, sync::oneshot, task::JoinHandle};

    use super::*;
    use crate::parser_protocol::PROTOCOL_VERSION;

    #[tokio::test]
    async fn control_responses_reject_payloads_before_reading_metadata() {
        for operation in [
            Operation::HealthCheck,
            Operation::Open {
                media_type: "application/pdf".into(),
                max_pages: 1,
                max_pixels: 4000,
                max_image_edge: 40,
                render_dpi: 144,
                page_max_edge: 40,
                timeout_seconds: 1,
                session_timeout_seconds: 30,
            },
            Operation::Upload,
        ] {
            for data_length in [1_u64, 3, RESPONSE_DATA_LIMIT, u64::MAX] {
                let (mut client, mut server) = UnixStream::pair().unwrap();
                let expected_operation = operation.clone();
                let (release, held_open) = oneshot::channel();
                let helper = tokio::spawn(async move {
                    let request = read_frame(&mut server, FrameKind::Request, REQUEST_DATA_LIMIT)
                        .await
                        .unwrap();
                    assert_eq!(request.operation, expected_operation);
                    let mut header = Vec::from(*b"OCRP");
                    header.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
                    header.extend_from_slice(&(FrameKind::Response as u16).to_be_bytes());
                    header.extend_from_slice(&1_u32.to_be_bytes());
                    header.extend_from_slice(&data_length.to_be_bytes());
                    server.write_all(&header).await.unwrap();
                    held_open.await.unwrap();
                });
                let data = if operation == Operation::Upload {
                    vec![1]
                } else {
                    Vec::new()
                };
                let result = exchange(
                    &mut client,
                    operation.clone(),
                    data,
                    Duration::from_secs(1),
                    &CancellationToken::new(),
                    REQUEST_DATA_LIMIT,
                )
                .await;
                release.send(()).unwrap();
                helper.await.unwrap();
                let error = result.unwrap_err();
                assert!(error.retryable);
                assert_eq!(error.message, "isolated parser IPC failed");
            }
        }
    }

    #[tokio::test]
    async fn health_validates_capacity_and_rejects_unexpected_operations() {
        for (operation, expected) in [
            (
                Operation::HealthReady {
                    max_sessions: 1,
                    active_sessions: 0,
                },
                Ok(ParserCapacity {
                    max_sessions: 1,
                    active_sessions: 0,
                }),
            ),
            (
                Operation::HealthReady {
                    max_sessions: 1,
                    active_sessions: 1,
                },
                Ok(ParserCapacity {
                    max_sessions: 1,
                    active_sessions: 1,
                }),
            ),
            (
                Operation::HealthReady {
                    max_sessions: 0,
                    active_sessions: 0,
                },
                Err("isolated parser returned an invalid response"),
            ),
            (
                Operation::HealthReady {
                    max_sessions: 1,
                    active_sessions: 2,
                },
                Err("isolated parser returned an invalid response"),
            ),
            (
                Operation::Admitted,
                Err("isolated parser returned an invalid response"),
            ),
            (
                Operation::NativeTextResult,
                Err("isolated parser returned an invalid response"),
            ),
            (
                Operation::RgbResult {
                    width: 1,
                    height: 1,
                },
                Err("isolated parser IPC failed"),
            ),
            (
                Operation::Error {
                    code: ErrorCode::Busy,
                },
                Err("isolated parser is at capacity"),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let socket_path = directory.path().join("health.sock");
            let listener = UnixListener::bind(&socket_path).unwrap();
            let helper = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_frame(&mut stream, FrameKind::Request, 0)
                    .await
                    .unwrap();
                assert_eq!(request.operation, Operation::HealthCheck);
                let data = if matches!(operation, Operation::RgbResult { .. }) {
                    vec![255; 3]
                } else {
                    Vec::new()
                };
                write_frame(
                    &mut stream,
                    FrameKind::Response,
                    &Frame { operation, data },
                    RESPONSE_DATA_LIMIT,
                )
                .await
                .unwrap();
            });
            let result = health_check(&socket_path, Duration::from_secs(1)).await;
            helper.await.unwrap();
            match expected {
                Ok(capacity) => assert_eq!(result.unwrap(), capacity),
                Err(message) => {
                    let error = result.unwrap_err();
                    assert!(error.retryable);
                    assert_eq!(error.message, message);
                }
            }
        }
    }

    #[tokio::test]
    async fn health_stalled_reply_is_deadline_bounded_and_next_probe_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("health.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let helper = tokio::spawn(async move {
            let (mut stalled, _) = listener.accept().await.unwrap();
            let request = read_frame(&mut stalled, FrameKind::Request, 0)
                .await
                .unwrap();
            assert_eq!(request.operation, Operation::HealthCheck);
            let mut closed = [0_u8; 1];
            assert_eq!(
                tokio::io::AsyncReadExt::read(&mut stalled, &mut closed)
                    .await
                    .unwrap(),
                0
            );
            let (mut recovered, _) = listener.accept().await.unwrap();
            let request = read_frame(&mut recovered, FrameKind::Request, 0)
                .await
                .unwrap();
            assert_eq!(request.operation, Operation::HealthCheck);
            write_frame(
                &mut recovered,
                FrameKind::Response,
                &Frame {
                    operation: Operation::HealthReady {
                        max_sessions: 1,
                        active_sessions: 0,
                    },
                    data: Vec::new(),
                },
                0,
            )
            .await
            .unwrap();
        });
        timeout(Duration::from_secs(2), async {
            let error = health_check(&socket_path, Duration::from_millis(100))
                .await
                .unwrap_err();
            assert!(error.retryable);
            assert_eq!(error.message, "isolated parser operation timed out");
            let capacity = health_check(&socket_path, Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(
                capacity,
                ParserCapacity {
                    max_sessions: 1,
                    active_sessions: 0,
                }
            );
            helper.await.unwrap();
        })
        .await
        .expect("health timeout must close its socket and allow a fresh probe");
    }

    async fn malicious_session(
        requested: Operation,
        response: Frame,
    ) -> (ParserSession, JoinHandle<()>, TempDir) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket_directory = directory.path().join("socket");
        std::fs::create_dir(&socket_directory).unwrap();
        std::fs::set_permissions(&socket_directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let socket_path = socket_directory.join("parser.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let response_kind = requested.clone();
        let helper = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let open = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert!(matches!(open.operation, Operation::Open { .. }));
            write_frame(
                &mut stream,
                FrameKind::Response,
                &Frame {
                    operation: Operation::Admitted,
                    data: Vec::new(),
                },
                RESPONSE_DATA_LIMIT,
            )
            .await
            .unwrap();
            let upload = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert_eq!(upload.operation, Operation::Upload);
            write_frame(
                &mut stream,
                FrameKind::Response,
                &Frame {
                    operation: Operation::Opened { pages: 1 },
                    data: Vec::new(),
                },
                RESPONSE_DATA_LIMIT,
            )
            .await
            .unwrap();
            let request = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert_eq!(request.operation, response_kind);
            write_frame(
                &mut stream,
                FrameKind::Response,
                &response,
                RESPONSE_DATA_LIMIT,
            )
            .await
            .unwrap();
        });
        let limits = ParserLimits {
            max_pages: 1,
            max_pixels: 4000,
            max_image_edge: 40,
            render_dpi: 144,
            page_max_edge: 40,
            timeout_seconds: 3,
            session_timeout_seconds: 30,
        };
        let session = ParserSession::open(
            &socket_path,
            b"%PDF-synthetic-test".to_vec(),
            "application/pdf",
            limits,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        (session, helper, directory)
    }

    #[tokio::test]
    async fn rejects_helper_rgb_dimensions_outside_the_requested_render_bounds() {
        let request = Operation::RenderRgb {
            page: 1,
            profile: RenderProfile::CourseLayout,
        };
        let response = Frame {
            operation: Operation::RgbResult {
                width: 80,
                height: 1,
            },
            data: vec![255; 80 * 3],
        };
        let (session, helper, _directory) = malicious_session(request, response).await;
        let error = session
            .render_rgb(1, RenderProfile::CourseLayout, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.message,
            "isolated parser returned an invalid response"
        );
        helper.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_forged_native_source_and_out_of_page_geometry() {
        let request = Operation::NativeText;
        let blocks = vec![SourceBlock {
            id: "native:p1:row0".into(),
            page_number: 1,
            region_index: None,
            source: "forged_source".into(),
            text: "not trusted".into(),
            bbox_2d: Some([-1, 1, 10, 10]),
            detector_score: None,
        }];
        let response = Frame {
            operation: Operation::NativeTextResult,
            data: serde_json::to_vec(&blocks).unwrap(),
        };
        let (session, helper, _directory) = malicious_session(request, response).await;
        let error = session
            .native_text(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.message,
            "isolated parser returned an invalid response"
        );
        helper.await.unwrap();
    }

    async fn two_render_session() -> (ParserSession, UnixStream, TempDir) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket_directory = directory.path().join("socket");
        std::fs::create_dir(&socket_directory).unwrap();
        std::fs::set_permissions(&socket_directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let socket_path = socket_directory.join("parser.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (server_tx, server_rx) = oneshot::channel::<UnixStream>();
        let handshake = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let open = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert!(matches!(open.operation, Operation::Open { .. }));
            write_frame(
                &mut stream,
                FrameKind::Response,
                &Frame {
                    operation: Operation::Admitted,
                    data: Vec::new(),
                },
                RESPONSE_DATA_LIMIT,
            )
            .await
            .unwrap();
            let upload = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert_eq!(upload.operation, Operation::Upload);
            write_frame(
                &mut stream,
                FrameKind::Response,
                &Frame {
                    operation: Operation::Opened { pages: 2 },
                    data: Vec::new(),
                },
                RESPONSE_DATA_LIMIT,
            )
            .await
            .unwrap();
            let _ = server_tx.send(stream);
        });
        let limits = ParserLimits {
            max_pages: 2,
            max_pixels: 4000,
            max_image_edge: 40,
            render_dpi: 144,
            page_max_edge: 40,
            timeout_seconds: 3,
            session_timeout_seconds: 30,
        };
        let session = timeout(
            Duration::from_secs(5),
            ParserSession::open(
                &socket_path,
                b"%PDF-synthetic-two-pages".to_vec(),
                "application/pdf",
                limits,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("session open handshake must complete")
        .expect("session open must succeed");
        handshake.await.unwrap();
        let server = timeout(Duration::from_secs(5), server_rx)
            .await
            .expect("server stream must be delivered")
            .expect("handshake task must not fail");
        (session, server, directory)
    }

    fn rgb_frame(width: u32, height: u32) -> Frame {
        Frame {
            operation: Operation::RgbResult { width, height },
            data: vec![128; (width as usize) * (height as usize) * 3],
        }
    }

    #[tokio::test]
    async fn aborted_request_fails_closed_against_delayed_stale_response() {
        let (session, mut server, _directory) = two_render_session().await;
        let page1 = session.render_rgb(1, RenderProfile::CourseLayout, CancellationToken::new());
        let mut page1 = Box::pin(page1);
        let (request_sent, request_arrived) = oneshot::channel::<()>();
        let mut request_arrived = std::pin::pin!(request_arrived);
        timeout(Duration::from_secs(2), async {
            let read_request = async {
                let first = read_frame(&mut server, FrameKind::Request, REQUEST_DATA_LIMIT)
                    .await
                    .unwrap();
                assert_eq!(
                    first.operation,
                    Operation::RenderRgb {
                        page: 1,
                        profile: RenderProfile::CourseLayout,
                    }
                );
                let _ = request_sent.send(());
            };
            tokio::select! {
                _ = &mut request_arrived => {}
                _ = read_request => {}
                _ = &mut page1 => panic!("page1 resolved before its response was withheld"),
            }
        })
        .await
        .expect("first render request must reach the server while page1 stays pending");
        drop(page1);
        let stale_write = write_frame(
            &mut server,
            FrameKind::Response,
            &rgb_frame(20, 10),
            RESPONSE_DATA_LIMIT,
        )
        .await;
        assert!(
            stale_write.is_err(),
            "client must have closed the aborted exchange socket"
        );
        let page2 = session.render_rgb(2, RenderProfile::CourseLayout, CancellationToken::new());
        let result = timeout(Duration::from_secs(2), page2).await;
        let outcome = result.expect("second render must not hang after stale response");
        match outcome {
            Ok(image) => panic!(
                "stale page-1 response was accepted as page 2: {}x{}",
                image.width(),
                image.height()
            ),
            Err(error) => {
                assert!(
                    error.retryable,
                    "stale-response rejection must be transient, got {error:?}"
                );
            }
        }
        let _ = server.shutdown().await;
    }

    #[tokio::test]
    async fn aborted_request_after_partial_header_fails_closed_on_reuse() {
        let (session, mut server, _directory) = two_render_session().await;
        let page1 = session.render_rgb(1, RenderProfile::CourseLayout, CancellationToken::new());
        let mut page1 = Box::pin(page1);
        let (header_sent, header_arrived) = oneshot::channel::<()>();
        let mut header_arrived = std::pin::pin!(header_arrived);
        timeout(Duration::from_secs(2), async {
            let serve_partial = async {
                let first = read_frame(&mut server, FrameKind::Request, REQUEST_DATA_LIMIT)
                    .await
                    .unwrap();
                assert!(matches!(first.operation, Operation::RenderRgb { .. }));
                let mut header = Vec::from(*b"OCRP");
                header.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
                header.extend_from_slice(&(FrameKind::Response as u16).to_be_bytes());
                header.extend_from_slice(&1_u32.to_be_bytes());
                header.extend_from_slice(&(600_u64).to_be_bytes());
                server.write_all(&header[..header.len() - 2]).await.unwrap();
                let _ = header_sent.send(());
            };
            tokio::select! {
                _ = &mut header_arrived => {}
                _ = serve_partial => {}
                _ = &mut page1 => panic!("page1 resolved before partial header was withheld"),
            }
        })
        .await
        .expect("partial header must reach page1 while it stays pending");
        drop(page1);
        let page2 = session.render_rgb(2, RenderProfile::CourseLayout, CancellationToken::new());
        let result = timeout(Duration::from_secs(2), page2).await;
        let outcome = result.expect("reuse after partial-header abort must not hang");
        match outcome {
            Ok(image) => panic!(
                "partial stale header let page 2 through: {}x{}",
                image.width(),
                image.height()
            ),
            Err(error) => assert!(
                error.retryable,
                "partial-header rejection must be transient, got {error:?}"
            ),
        }
        let _ = server.shutdown().await;
    }

    #[tokio::test]
    async fn sequential_renders_on_one_session_still_succeed() {
        let (session, mut server, _directory) = two_render_session().await;
        let first = session.render_rgb(1, RenderProfile::CourseLayout, CancellationToken::new());
        let mut first = Box::pin(first);
        timeout(Duration::from_secs(2), async {
            let serve = async {
                let request = read_frame(&mut server, FrameKind::Request, REQUEST_DATA_LIMIT)
                    .await
                    .unwrap();
                assert_eq!(
                    request.operation,
                    Operation::RenderRgb {
                        page: 1,
                        profile: RenderProfile::CourseLayout,
                    }
                );
                write_frame(
                    &mut server,
                    FrameKind::Response,
                    &rgb_frame(20, 10),
                    RESPONSE_DATA_LIMIT,
                )
                .await
                .unwrap();
            };
            tokio::select! {
                _ = serve => {}
                result = &mut first => panic!("first render finished without a server: {result:?}"),
            }
        })
        .await
        .expect("first render request must be served");
        let image = timeout(Duration::from_secs(2), first.as_mut())
            .await
            .expect("first render must complete")
            .expect("first render must succeed");
        assert_eq!((image.width(), image.height()), (20, 10));
        let second = session.render_rgb(2, RenderProfile::CourseLayout, CancellationToken::new());
        let mut second = Box::pin(second);
        timeout(Duration::from_secs(2), async {
            let serve = async {
                let request = read_frame(&mut server, FrameKind::Request, REQUEST_DATA_LIMIT)
                    .await
                    .unwrap();
                assert_eq!(
                    request.operation,
                    Operation::RenderRgb {
                        page: 2,
                        profile: RenderProfile::CourseLayout,
                    }
                );
                write_frame(
                    &mut server,
                    FrameKind::Response,
                    &rgb_frame(10, 20),
                    RESPONSE_DATA_LIMIT,
                )
                .await
                .unwrap();
            };
            tokio::select! {
                _ = serve => {}
                result = &mut second => panic!("second render finished without a server: {result:?}"),
            }
        })
        .await
        .expect("second render request must be served");
        let image = timeout(Duration::from_secs(2), second.as_mut())
            .await
            .expect("second render must complete")
            .expect("second render must succeed");
        assert_eq!((image.width(), image.height()), (10, 20));
        let _ = server.shutdown().await;
    }
}
