//! Startup-only environment/file secret resolution shared by every binary.
//!
//! A configured `NAME` and `NAME_FILE` always conflict, even when `NAME` is
//! empty. An empty optional environment value disables the setting for legacy
//! Compose compatibility; an empty file (or file setting) is an error. Files
//! may contain one terminal LF/CRLF, which is removed without trimming spaces.
//! Other control characters and non-UTF-8 values are rejected. Reads are capped
//! at 64 KiB, including the optional line ending.
//!
//! On Unix, the final path component must not be a symlink, and the opened file
//! descriptor must be regular. `O_NONBLOCK` prevents a FIFO substitution from
//! hanging startup. Parent-directory symlinks are allowed. A safe file reader
//! is not provided on other platforms; environment values still work there.
//! Errors retain only the configuration key and a fixed reason, never the
//! secret, file path, or underlying I/O error. Do not log resolved values.

use std::{env, ffi::OsString, fmt, path::Path};

#[cfg(unix)]
use std::io::Read;

/// Maximum encoded input size, before removal of a file's terminal line ending.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretErrorKind {
    Missing,
    ConflictingSources,
    EmptyFileSetting,
    FileUnavailable,
    NotRegularFile,
    UnsupportedPlatform,
    TooLarge,
    InvalidUnicode,
    EmptyValue,
    ControlCharacter,
}

/// Deliberately excludes secret values, paths, and source errors from Debug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretError {
    pub variable: &'static str,
    pub kind: SecretErrorKind,
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            SecretErrorKind::Missing => "is required (direct value or _FILE)",
            SecretErrorKind::ConflictingSources => {
                "and its _FILE setting cannot both be configured"
            }
            SecretErrorKind::EmptyFileSetting => "_FILE must name a nonempty file path",
            SecretErrorKind::FileUnavailable => "_FILE could not be opened or read safely",
            SecretErrorKind::NotRegularFile => "_FILE must refer to a regular file",
            SecretErrorKind::UnsupportedPlatform => "_FILE is unsupported on this platform",
            SecretErrorKind::TooLarge => "exceeds the 65536-byte input limit",
            SecretErrorKind::InvalidUnicode => "must contain valid UTF-8",
            SecretErrorKind::EmptyValue => "must not be empty",
            SecretErrorKind::ControlCharacter => "must not contain control characters",
        };
        write!(formatter, "{} {reason}", self.variable)
    }
}

impl std::error::Error for SecretError {}

pub fn required(variable: &'static str) -> Result<String, SecretError> {
    required_with_env(variable, |key| env::var_os(key))
}

pub fn optional(variable: &'static str) -> Result<Option<String>, SecretError> {
    optional_with_env(variable, |key| env::var_os(key))
}

/// Apply a non-secret default only when neither setting is present. A present
/// empty direct value is invalid, rather than silently selecting the default.
pub fn with_default(variable: &'static str, default: &str) -> Result<String, SecretError> {
    Ok(resolve(variable, |key| env::var_os(key), false)?.unwrap_or_else(|| default.to_owned()))
}

/// Injectable environment lookup keeps tests independent of process globals.
pub fn required_with_env(
    variable: &'static str,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<String, SecretError> {
    resolve(variable, lookup, false)?.ok_or(SecretError {
        variable,
        kind: SecretErrorKind::Missing,
    })
}

/// Unlike required settings, a present-but-empty direct value means disabled.
/// An empty file is never silently treated as disabled.
pub fn optional_with_env(
    variable: &'static str,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<String>, SecretError> {
    resolve(variable, lookup, true)
}

fn resolve(
    variable: &'static str,
    lookup: impl Fn(&str) -> Option<OsString>,
    optional: bool,
) -> Result<Option<String>, SecretError> {
    let direct = lookup(variable);
    let file = lookup(&format!("{variable}_FILE"));
    let error = |kind| SecretError { variable, kind };
    match (direct, file) {
        (Some(_), Some(_)) => Err(error(SecretErrorKind::ConflictingSources)),
        (Some(value), None) => {
            let value = value
                .into_string()
                .map_err(|_| error(SecretErrorKind::InvalidUnicode))?;
            if optional && value.is_empty() {
                return Ok(None);
            }
            validate(value, variable).map(Some)
        }
        (None, Some(path)) => {
            if path.is_empty() {
                return Err(error(SecretErrorKind::EmptyFileSetting));
            }
            let mut bytes = read_file(Path::new(&path)).map_err(error)?;
            if bytes.ends_with(b"\r\n") {
                bytes.truncate(bytes.len() - 2);
            } else if bytes.ends_with(b"\n") {
                bytes.truncate(bytes.len() - 1);
            }
            let value =
                String::from_utf8(bytes).map_err(|_| error(SecretErrorKind::InvalidUnicode))?;
            validate(value, variable).map(Some)
        }
        (None, None) => Ok(None),
    }
}

fn validate(value: String, variable: &'static str) -> Result<String, SecretError> {
    let kind = if value.len() > MAX_SECRET_BYTES {
        Some(SecretErrorKind::TooLarge)
    } else if value.is_empty() {
        Some(SecretErrorKind::EmptyValue)
    } else if value.chars().any(char::is_control) {
        Some(SecretErrorKind::ControlCharacter)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(SecretError { variable, kind })
    } else {
        Ok(value)
    }
}

#[cfg(unix)]
fn read_file(path: &Path) -> Result<Vec<u8>, SecretErrorKind> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| SecretErrorKind::FileUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| SecretErrorKind::FileUnavailable)?;
    if !metadata.is_file() {
        return Err(SecretErrorKind::NotRegularFile);
    }
    if metadata.len() > MAX_SECRET_BYTES as u64 {
        return Err(SecretErrorKind::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SecretErrorKind::FileUnavailable)?;
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(SecretErrorKind::TooLarge);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_file(_path: &Path) -> Result<Vec<u8>, SecretErrorKind> {
    Err(SecretErrorKind::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, fs};

    fn resolve_from(
        values: &[(&str, OsString)],
        optional: bool,
    ) -> Result<Option<String>, SecretError> {
        let values: HashMap<_, _> = values.iter().cloned().collect();
        let lookup = |key: &str| values.get(key).cloned();
        if optional {
            optional_with_env("TEST_SECRET", lookup)
        } else {
            required_with_env("TEST_SECRET", lookup).map(Some)
        }
    }

    fn from_file(bytes: &[u8]) -> Result<Option<String>, SecretError> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        fs::write(&path, bytes).unwrap();
        resolve_from(&[("TEST_SECRET_FILE", path.into_os_string())], false)
    }

    #[test]
    fn direct_and_file_are_exclusive_even_when_either_is_empty() {
        for (direct, file) in [("value", "path"), ("", "path"), ("value", ""), ("", "")] {
            let error = resolve_from(
                &[
                    ("TEST_SECRET", direct.into()),
                    ("TEST_SECRET_FILE", file.into()),
                ],
                true,
            )
            .unwrap_err();
            assert_eq!(error.kind, SecretErrorKind::ConflictingSources);
        }
    }

    #[test]
    fn every_supported_runtime_and_bootstrap_key_uses_the_same_file_contract() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        fs::write(&path, "fixture-value\n").unwrap();
        for variable in [
            "DATABASE_URL",
            "OCR_API_TOKEN",
            "OCR_REVIEW_TOKEN",
            "VLLM_API_KEY",
            "REDIS_URL",
            "METRICS_BEARER_TOKEN",
            "OCR_MIGRATOR_PASSWORD",
            "OCR_API_PASSWORD",
            "OCR_WORKER_PASSWORD",
            "OCR_MAINTENANCE_PASSWORD",
            "OCR_BACKUP_PASSWORD",
        ] {
            let file_key = format!("{variable}_FILE");
            let value = required_with_env(variable, |key| {
                (key == file_key).then(|| path.as_os_str().into())
            })
            .unwrap();
            assert_eq!(value, "fixture-value");
        }
    }

    #[test]
    fn missing_required_and_empty_required_fail_but_empty_optional_env_disables() {
        assert_eq!(
            resolve_from(&[], false).unwrap_err().kind,
            SecretErrorKind::Missing
        );
        assert_eq!(resolve_from(&[], true).unwrap(), None);
        assert_eq!(
            resolve_from(&[("TEST_SECRET", "".into())], true).unwrap(),
            None
        );
        assert_eq!(
            resolve_from(&[("TEST_SECRET", "".into())], false)
                .unwrap_err()
                .kind,
            SecretErrorKind::EmptyValue
        );
        assert_eq!(
            from_file(b"").unwrap_err().kind,
            SecretErrorKind::EmptyValue
        );
        assert_eq!(
            from_file(b"\n").unwrap_err().kind,
            SecretErrorKind::EmptyValue
        );
        assert_eq!(
            resolve_from(&[("TEST_SECRET_FILE", "".into())], true)
                .unwrap_err()
                .kind,
            SecretErrorKind::EmptyFileSetting
        );
    }

    #[test]
    fn files_remove_exactly_one_terminal_line_ending_and_preserve_other_bytes() {
        for bytes in [b" secret ".as_slice(), b" secret \n", b" secret \r\n"] {
            assert_eq!(from_file(bytes).unwrap().as_deref(), Some(" secret "));
        }
        assert_eq!(
            from_file("mật-khẩu".as_bytes()).unwrap().as_deref(),
            Some("mật-khẩu")
        );
        for bytes in [
            b"secret\n\n".as_slice(),
            b"secret\r",
            b"sec\0ret",
            b"sec\tret",
            b"secret\r\n\r\n",
        ] {
            assert_eq!(
                from_file(bytes).unwrap_err().kind,
                SecretErrorKind::ControlCharacter
            );
        }
        assert_eq!(
            resolve_from(&[("TEST_SECRET", "secret\n".into())], false)
                .unwrap_err()
                .kind,
            SecretErrorKind::ControlCharacter
        );
        assert_eq!(
            from_file("secret\u{0085}".as_bytes()).unwrap_err().kind,
            SecretErrorKind::ControlCharacter
        );
        assert_eq!(
            from_file(&[0xff]).unwrap_err().kind,
            SecretErrorKind::InvalidUnicode
        );
    }

    #[test]
    fn input_limit_is_enforced_for_files_and_environment() {
        let at_limit = vec![b'x'; MAX_SECRET_BYTES];
        assert_eq!(
            from_file(&at_limit).unwrap().unwrap().len(),
            MAX_SECRET_BYTES
        );
        assert_eq!(
            from_file(&vec![b'x'; MAX_SECRET_BYTES + 1])
                .unwrap_err()
                .kind,
            SecretErrorKind::TooLarge
        );
        assert_eq!(
            resolve_from(
                &[("TEST_SECRET", "x".repeat(MAX_SECRET_BYTES + 1).into())],
                false
            )
            .unwrap_err()
            .kind,
            SecretErrorKind::TooLarge
        );
    }

    #[test]
    fn unavailable_paths_and_debug_never_expose_paths_or_contents() {
        let path = "/unavailable/private-password-1234";
        let error = resolve_from(&[("TEST_SECRET_FILE", path.into())], false).unwrap_err();
        assert_eq!(error.kind, SecretErrorKind::FileUnavailable);
        for description in [format!("{error}"), format!("{error:?}")] {
            assert!(description.contains("TEST_SECRET"));
            assert!(!description.contains(path));
            assert!(!description.contains("private-password"));
        }
        let value = "private-secret\0hidden";
        let error = resolve_from(&[("TEST_SECRET", value.into())], false).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private-secret"));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_environment_without_retaining_the_value() {
        use std::os::unix::ffi::OsStringExt;
        let error =
            resolve_from(&[("TEST_SECRET", OsString::from_vec(vec![0xff]))], false).unwrap_err();
        assert_eq!(error.kind, SecretErrorKind::InvalidUnicode);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_directories_and_fifos_without_waiting_for_a_writer() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::symlink},
            time::Instant,
        };
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, "secret").unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        assert_eq!(
            resolve_from(&[("TEST_SECRET_FILE", link.into_os_string())], false)
                .unwrap_err()
                .kind,
            SecretErrorKind::FileUnavailable
        );
        assert_eq!(
            resolve_from(
                &[("TEST_SECRET_FILE", directory.path().as_os_str().into())],
                false
            )
            .unwrap_err()
            .kind,
            SecretErrorKind::NotRegularFile
        );
        let fifo = directory.path().join("fifo");
        let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is a valid, live NUL-terminated string; mode is owner-only.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert_eq!(
            resolve_from(&[("TEST_SECRET_FILE", fifo.into_os_string())], false)
                .unwrap_err()
                .kind,
            SecretErrorKind::NotRegularFile
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
