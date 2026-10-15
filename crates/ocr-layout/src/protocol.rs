//! Local, sequential NDJSON protocol for the isolated warm detector helper.
//!
//! Only a basename inside the helper's private scratch directory is accepted.
//! Requests never contain document text or model credentials.

use std::io::{self, BufRead};

use serde::{Deserialize, Serialize};

use crate::PageLayout;

pub const VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 4096;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectRequest {
    pub version: u32,
    pub id: u64,
    pub image: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Ready { version: u32, model_sha256: String },
    Layout { id: u64, layout: PageLayout },
    Error { id: u64, code: String },
}

/// Read one frame without allowing `read_until` to allocate for unbounded input.
/// A partial final line is rejected, so truncated process output is never valid.
pub fn read_frame(reader: &mut impl BufRead, bound: usize) -> io::Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial frame",
                ))
            };
        }
        let length = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        if length > bound.saturating_sub(output.len()) {
            return Err(io::Error::other("frame exceeds byte bound"));
        }
        let complete = bytes[length - 1] == b'\n';
        output.extend_from_slice(&bytes[..length]);
        reader.consume(length);
        if complete {
            return Ok(Some(output));
        }
    }
}

pub fn valid_image_basename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.ends_with(".png")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && value != ".png"
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    #[test]
    fn framing_keeps_requests_separate_and_rejects_truncation_and_overflow() {
        let mut reader = BufReader::with_capacity(2, Cursor::new(b"abc\ndef\n"));
        assert_eq!(read_frame(&mut reader, 4).unwrap().unwrap(), b"abc\n");
        assert_eq!(read_frame(&mut reader, 4).unwrap().unwrap(), b"def\n");
        assert!(read_frame(&mut reader, 4).unwrap().is_none());
        assert!(read_frame(&mut Cursor::new(b"abc\n"), 3).is_err());
        assert!(read_frame(&mut Cursor::new(b"abc"), 4).is_err());
    }

    #[test]
    fn filenames_cannot_escape_the_private_directory() {
        assert!(valid_image_basename("page-01.png"));
        for input in [
            "../page.png",
            "/page.png",
            "page\\other.png",
            ".png",
            "a\nb.png",
        ] {
            assert!(!valid_image_basename(input));
        }
    }
}
