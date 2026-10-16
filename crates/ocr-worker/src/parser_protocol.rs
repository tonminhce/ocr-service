use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ProcessingError;

const MAGIC: &[u8; 4] = b"OCRP";
const HEADER_BYTES: usize = 20;
const METADATA_LIMIT: usize = 64 * 1024;
pub const REQUEST_DATA_LIMIT: u64 = 64 * 1024 * 1024;
pub const RESPONSE_DATA_LIMIT: u64 = 128 * 1024 * 1024;
pub const PROTOCOL_VERSION: u16 = 2;
pub const MAX_SESSION_TIMEOUT_SECONDS: u64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Request = 1,
    Response = 2,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("parser IPC frame header is invalid")]
    Header,
    #[error("parser IPC frame version is unsupported")]
    Version,
    #[error("parser IPC frame kind is unexpected")]
    Kind,
    #[error("parser IPC frame exceeds its configured bound")]
    Limit,
    #[error("parser IPC metadata is invalid")]
    Metadata,
    #[error("parser IPC stream ended before a complete frame")]
    Truncated,
    #[error("parser IPC transport failed")]
    Io,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    HealthCheck,
    HealthReady {
        max_sessions: u32,
        active_sessions: u32,
    },
    /// Admission only; the document follows in `Upload` after `Admitted`.
    Open {
        media_type: String,
        max_pages: u32,
        max_pixels: u64,
        max_image_edge: u32,
        render_dpi: u32,
        page_max_edge: u32,
        timeout_seconds: u64,
        session_timeout_seconds: u64,
    },
    Admitted,
    Upload,
    PageInfo {
        page: u32,
    },
    RenderRgb {
        page: u32,
        profile: RenderProfile,
    },
    NativeText,
    Close,
    Opened {
        pages: u32,
    },
    PageInfoResult {
        width_points: f64,
        height_points: f64,
    },
    RgbResult {
        width: u32,
        height: u32,
    },
    NativeTextResult,
    Closed,
    Error {
        code: ErrorCode,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderProfile {
    FullPage,
    CourseLayout,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Busy,
    Cancelled,
    Unavailable,
    PageLimitExceeded { actual: u32, limit: u32 },
    ImagePixelLimit { limit: u64 },
    Rejected { reason: RejectReason },
}

macro_rules! reject_reasons {
    ($($variant:ident => $message:literal),+ $(,)?) => {
        /// Permanent document rejections. Both ends own the text, so a parser
        /// cannot place arbitrary strings into stored job errors.
        #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
        #[serde(rename_all = "snake_case")]
        pub enum RejectReason { $($variant),+ }
        impl RejectReason {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn message(self) -> &'static str {
                match self { $(Self::$variant => $message),+ }
            }
        }
    };
}
reject_reasons! {
    MissingPdfHeader => "input does not have a PDF header",
    UnsupportedMediaType => "supported media types are PDF, PNG, JPEG, and WebP",
    UnsupportedImage => "input is not a supported image",
    ImageMediaMismatch => "image bytes do not match the declared media type",
    EncryptedPdf => "encrypted PDFs are not supported",
    InvalidPageCount => "PDF metadata does not contain a valid page count",
    PdfMetadataEncoding => "PDF metadata is not valid UTF-8",
    UnprocessablePdf => "Poppler could not process this PDF; it may be corrupt or encrypted",
    ProcessOutputLimit => "PDF process output exceeded its size limit",
    ProcessTimeout => "PDF inspection or rendering timed out",
    InvalidPageSize => "PDF metadata has an invalid page size",
    InvalidPdfPageNumber => "invalid PDF page number",
    InvalidRenderParameters => "invalid PDF render parameters",
    InvalidRenderDimensions => "invalid PDF rendering dimensions",
    PageRenderLimit => "PDF page exceeds image rendering limits",
    NativeTextEncoding => "native PDF text is not UTF-8",
    InvalidNativeXml => "invalid native PDF geometry XML",
    InvalidNativeNumber => "invalid native PDF geometry number",
    InvalidNativePage => "invalid native PDF page dimensions",
    NativeTextBudget => "native PDF text budget exceeded",
    InvalidImageEncoding => "invalid image encoding",
    UnsupportedImageFormat => "image format is not PNG, JPEG, or WebP",
    UnreadableImageDimensions => "cannot read image dimensions",
    InvalidImageData => "invalid or oversized image data",
    InvalidImageOrientation => "invalid image orientation metadata",
    InvalidPageNumber => "invalid document page number",
    InvalidLimits => "parser operation limits are invalid",
    ProtocolViolation => "parser request violated the protocol",
    Unprocessable => "document could not be processed by the isolated parser",
}

impl ErrorCode {
    /// Parser side: classify by exact canonical text, never by substring.
    pub fn from_error(error: &ProcessingError) -> Self {
        if error.retryable {
            return if error.message == crate::CANCELLED_MESSAGE {
                Self::Cancelled
            } else {
                Self::Unavailable
            };
        }
        if let Some((actual, limit)) = page_limit(&error.message) {
            return Self::PageLimitExceeded { actual, limit };
        }
        if let Some(limit) = pixel_limit(&error.message) {
            return Self::ImagePixelLimit { limit };
        }
        let reason = RejectReason::ALL
            .iter()
            .copied()
            .find(|reason| reason.message() == error.message)
            .unwrap_or(RejectReason::Unprocessable);
        Self::Rejected { reason }
    }

    /// Client side: only these fixed messages and retry classes leave the parser.
    pub fn into_error(self) -> ProcessingError {
        match self {
            Self::Busy => ProcessingError::transient("isolated parser is at capacity"),
            Self::Cancelled => ProcessingError::cancelled(),
            Self::Unavailable => ProcessingError::transient("isolated parser is unavailable"),
            Self::PageLimitExceeded { actual, limit } => {
                ProcessingError::permanent(format!("PDF has {actual} pages; the limit is {limit}"))
            }
            Self::ImagePixelLimit { limit } => {
                ProcessingError::permanent(format!("image exceeds the {limit} pixel limit"))
            }
            Self::Rejected { reason } => ProcessingError::permanent(reason.message()),
        }
    }
}

fn page_limit(message: &str) -> Option<(u32, u32)> {
    let rest = message.strip_prefix("PDF has ")?;
    let (actual, limit) = rest.split_once(" pages; the limit is ")?;
    Some((actual.parse().ok()?, limit.parse().ok()?))
}

fn pixel_limit(message: &str) -> Option<u64> {
    message
        .strip_prefix("image exceeds the ")?
        .strip_suffix(" pixel limit")?
        .parse()
        .ok()
}

#[derive(Debug, PartialEq)]
pub struct Frame {
    pub operation: Operation,
    pub data: Vec<u8>,
}

pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    kind: FrameKind,
    frame: &Frame,
    data_limit: u64,
) -> Result<(), ProtocolError> {
    let metadata = serde_json::to_vec(&frame.operation).map_err(|_| ProtocolError::Metadata)?;
    let data_length = u64::try_from(frame.data.len()).map_err(|_| ProtocolError::Limit)?;
    if metadata.is_empty() || metadata.len() > METADATA_LIMIT || data_length > data_limit {
        return Err(ProtocolError::Limit);
    }
    validate_payload(kind, &frame.operation, data_length)?;

    let mut header = [0_u8; HEADER_BYTES];
    header[..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    header[6..8].copy_from_slice(&(kind as u16).to_be_bytes());
    header[8..12].copy_from_slice(
        &u32::try_from(metadata.len())
            .map_err(|_| ProtocolError::Limit)?
            .to_be_bytes(),
    );
    header[12..20].copy_from_slice(&data_length.to_be_bytes());
    writer
        .write_all(&header)
        .await
        .map_err(|_| ProtocolError::Io)?;
    writer
        .write_all(&metadata)
        .await
        .map_err(|_| ProtocolError::Io)?;
    writer
        .write_all(&frame.data)
        .await
        .map_err(|_| ProtocolError::Io)
}

pub async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
    expected_kind: FrameKind,
    data_limit: u64,
) -> Result<Frame, ProtocolError> {
    let mut header = [0_u8; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| ProtocolError::Truncated)?;
    if &header[..4] != MAGIC {
        return Err(ProtocolError::Header);
    }
    if u16::from_be_bytes([header[4], header[5]]) != PROTOCOL_VERSION {
        return Err(ProtocolError::Version);
    }
    if u16::from_be_bytes([header[6], header[7]]) != expected_kind as u16 {
        return Err(ProtocolError::Kind);
    }

    let metadata_length = u32::from_be_bytes(
        header[8..12]
            .try_into()
            .map_err(|_| ProtocolError::Header)?,
    ) as usize;
    let data_length = u64::from_be_bytes(
        header[12..20]
            .try_into()
            .map_err(|_| ProtocolError::Header)?,
    );
    if metadata_length == 0
        || metadata_length > METADATA_LIMIT
        || data_length > data_limit
        || usize::try_from(data_length).is_err()
    {
        return Err(ProtocolError::Limit);
    }

    let mut metadata = vec![0_u8; metadata_length];
    reader
        .read_exact(&mut metadata)
        .await
        .map_err(|_| ProtocolError::Truncated)?;
    let operation = serde_json::from_slice(&metadata).map_err(|_| ProtocolError::Metadata)?;
    validate_payload(expected_kind, &operation, data_length)?;
    let mut data = vec![0_u8; data_length as usize];
    reader
        .read_exact(&mut data)
        .await
        .map_err(|_| ProtocolError::Truncated)?;
    Ok(Frame { operation, data })
}

fn validate_payload(
    kind: FrameKind,
    operation: &Operation,
    data_length: u64,
) -> Result<(), ProtocolError> {
    let valid = match (kind, operation) {
        (FrameKind::Request, Operation::Upload) => (1..=REQUEST_DATA_LIMIT).contains(&data_length),
        (
            FrameKind::Request,
            Operation::HealthCheck
            | Operation::Open { .. }
            | Operation::PageInfo { .. }
            | Operation::RenderRgb { .. }
            | Operation::NativeText
            | Operation::Close,
        ) => data_length == 0,
        (
            FrameKind::Response,
            Operation::Opened { .. }
            | Operation::Admitted
            | Operation::PageInfoResult { .. }
            | Operation::Closed
            | Operation::HealthReady { .. }
            | Operation::Error { .. },
        ) => data_length == 0,
        (FrameKind::Response, Operation::RgbResult { width, height }) => {
            let Some(expected) = u64::from(*width)
                .checked_mul(u64::from(*height))
                .and_then(|pixels| pixels.checked_mul(3))
            else {
                return Err(ProtocolError::Limit);
            };
            *width > 0 && *height > 0 && data_length == expected
        }
        (FrameKind::Response, Operation::NativeTextResult) => data_length <= 4 * 1024 * 1024,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::Limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn versioned_frame_round_trips_binary_payload_exactly() {
        let frame = Frame {
            operation: Operation::RgbResult {
                width: 1,
                height: 2,
            },
            data: vec![0, 255, 12, 0, 128, 255],
        };
        let (mut client, mut server) = duplex(128);
        let writing = tokio::spawn(async move {
            write_frame(&mut client, FrameKind::Response, &frame, 32)
                .await
                .unwrap();
        });
        let decoded = read_frame(&mut server, FrameKind::Response, 32)
            .await
            .unwrap();
        writing.await.unwrap();
        assert_eq!(
            decoded.operation,
            Operation::RgbResult {
                width: 1,
                height: 2,
            }
        );
        assert_eq!(decoded.data, [0, 255, 12, 0, 128, 255]);
    }

    #[tokio::test]
    async fn rejects_rgb_payload_with_mismatched_dimensions() {
        let frame = Frame {
            operation: Operation::RgbResult {
                width: 2,
                height: 2,
            },
            data: vec![255; 11],
        };
        let (mut writer, _reader) = duplex(128);
        assert_eq!(
            write_frame(&mut writer, FrameKind::Response, &frame, 32)
                .await
                .unwrap_err(),
            ProtocolError::Limit
        );
    }

    #[tokio::test]
    async fn rejects_wrong_magic_version_and_direction_before_metadata_decode() {
        for (magic, version, kind, expected) in [
            (*b"BAD!", PROTOCOL_VERSION, 2_u16, ProtocolError::Header),
            (*MAGIC, PROTOCOL_VERSION + 1, 2_u16, ProtocolError::Version),
            (*MAGIC, PROTOCOL_VERSION, 1_u16, ProtocolError::Kind),
        ] {
            let mut bytes = Vec::from(magic);
            bytes.extend_from_slice(&version.to_be_bytes());
            bytes.extend_from_slice(&kind.to_be_bytes());
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes.extend_from_slice(&0_u64.to_be_bytes());
            assert_eq!(
                read_frame(&mut bytes.as_slice(), FrameKind::Response, 8)
                    .await
                    .unwrap_err(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn rejects_oversized_lengths_before_reading_or_allocating_payload() {
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        bytes.extend_from_slice(&(FrameKind::Request as u16).to_be_bytes());
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(&1024_u64.to_be_bytes());
        assert_eq!(
            read_frame(&mut bytes.as_slice(), FrameKind::Request, 8)
                .await
                .unwrap_err(),
            ProtocolError::Limit
        );
    }

    #[test]
    fn error_codes_round_trip_canonical_messages_and_retry_classes() {
        for reason in RejectReason::ALL {
            let error = ProcessingError::permanent(reason.message());
            let code = ErrorCode::from_error(&error);
            assert_eq!(code, ErrorCode::Rejected { reason: *reason });
            let restored = code.into_error();
            assert_eq!(restored.message, reason.message());
            assert!(!restored.retryable);
        }
        let page = ProcessingError::permanent("PDF has 31 pages; the limit is 20");
        assert_eq!(
            ErrorCode::from_error(&page).into_error().message,
            page.message
        );
        let pixels = ProcessingError::permanent("image exceeds the 4000 pixel limit");
        assert_eq!(
            ErrorCode::from_error(&pixels).into_error().message,
            pixels.message
        );

        let unknown = ErrorCode::from_error(&ProcessingError::permanent("novel failure"));
        assert_eq!(
            unknown,
            ErrorCode::Rejected {
                reason: RejectReason::Unprocessable
            }
        );
        assert!(!unknown.into_error().retryable);
        let transient = ErrorCode::from_error(&ProcessingError::transient("disk busy"));
        assert_eq!(transient, ErrorCode::Unavailable);
        assert!(transient.into_error().retryable);
        let cancelled = ErrorCode::from_error(&ProcessingError::cancelled());
        assert_eq!(cancelled, ErrorCode::Cancelled);
        assert!(ErrorCode::Busy.into_error().retryable);
    }

    #[tokio::test]
    async fn open_carries_no_document_and_upload_requires_one() {
        let open = Frame {
            operation: Operation::Open {
                media_type: "application/pdf".into(),
                max_pages: 1,
                max_pixels: 1,
                max_image_edge: 32,
                render_dpi: 72,
                page_max_edge: 32,
                timeout_seconds: 1,
                session_timeout_seconds: 1,
            },
            data: vec![1],
        };
        let upload = Frame {
            operation: Operation::Upload,
            data: Vec::new(),
        };
        let (mut writer, _reader) = duplex(128);
        for frame in [open, upload] {
            assert_eq!(
                write_frame(&mut writer, FrameKind::Request, &frame, 32)
                    .await
                    .unwrap_err(),
                ProtocolError::Limit
            );
        }
    }

    #[tokio::test]
    async fn rejects_truncated_metadata_and_payload() {
        let mut header = Vec::from(*MAGIC);
        header.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        header.extend_from_slice(&(FrameKind::Request as u16).to_be_bytes());
        header.extend_from_slice(&2_u32.to_be_bytes());
        header.extend_from_slice(&1_u64.to_be_bytes());
        header.extend_from_slice(b"{");
        assert_eq!(
            read_frame(&mut header.as_slice(), FrameKind::Request, 8)
                .await
                .unwrap_err(),
            ProtocolError::Truncated
        );
    }
}
