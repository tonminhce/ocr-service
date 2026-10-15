//! Trusted command construction. Document bytes never select an executable,
//! environment entry, shell fragment or option. This is hygiene, not a sandbox:
//! local children still share the worker's filesystem/user/network boundary.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::ProcessingError;

pub(crate) fn scratch_directory() -> Result<std::sync::Arc<tempfile::TempDir>, ProcessingError> {
    let root = std::path::absolute(std::env::temp_dir())
        .map_err(|_| ProcessingError::transient("cannot resolve document scratch directory"))?;
    tempfile::tempdir_in(root)
        .map(std::sync::Arc::new)
        .map_err(|_| ProcessingError::transient("cannot create document scratch directory"))
}

#[cfg(target_os = "macos")]
const TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
#[cfg(not(target_os = "macos"))]
const TOOL_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/local/bin"];

/// Only the three packaged Poppler tools may be resolved by name. Inherited
/// PATH, current directory and executable names from document content are ignored.
fn poppler_binary(name: &str) -> Result<PathBuf, ProcessingError> {
    if !matches!(name, "pdfinfo" | "pdftoppm" | "pdftotext") {
        return Err(ProcessingError::permanent("unsupported PDF tool"));
    }
    TOOL_DIRS
        .iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| executable(path))
        .ok_or_else(|| ProcessingError::transient("required Poppler tool is unavailable"))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// Layout is a trusted operator configuration. Bare `layout-detect` resolves
/// beside the worker executable or in fixed installation directories. Native
/// development and fixtures may use an explicit absolute executable path.
pub(crate) fn layout_binary(configured: &Path) -> Result<PathBuf, ProcessingError> {
    let candidate = if configured.is_absolute() {
        configured.to_owned()
    } else if configured == Path::new("layout-detect") {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.parent()
                    .map(|directory| directory.join("layout-detect"))
            })
            .filter(|path| executable(path));
        sibling
            .or_else(|| {
                TOOL_DIRS
                    .iter()
                    .map(|directory| Path::new(directory).join("layout-detect"))
                    .find(|path| executable(path))
            })
            .ok_or_else(|| ProcessingError::transient("Rust layout detector is unavailable"))?
    } else {
        return Err(ProcessingError::permanent(
            "LAYOUT_BINARY must be layout-detect or an absolute executable path",
        ));
    };
    if !executable(&candidate) {
        return Err(ProcessingError::transient(
            "Rust layout detector is unavailable",
        ));
    }
    Ok(candidate)
}

pub(crate) fn clean_environment(command: &mut Command, scratch: &Path) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .env("HOME", "/nonexistent")
        .env("TMPDIR", scratch)
        .current_dir(scratch);
}

/// ORT explicitly loads this library; no LD_*/DYLD_* or arbitrary runtime env
/// is forwarded. Missing runtime remains an ordinary detector startup error.
pub(crate) fn layout_environment(
    command: &mut Command,
    scratch: &Path,
    runtime: Option<&Path>,
) -> Result<(), ProcessingError> {
    clean_environment(command, scratch);
    if let Some(runtime) = runtime {
        if !runtime.is_absolute() || !runtime.is_file() {
            return Err(ProcessingError::permanent(
                "ORT_DYLIB_PATH must identify an absolute runtime library file",
            ));
        }
        command.env("ORT_DYLIB_PATH", runtime);
    }
    Ok(())
}

/// All options are selected by Rust. Paths come from private generated scratch
/// directories, and all numeric limits have been checked by Settings.
pub(crate) enum PdfTool<'a> {
    Info {
        input: &'a Path,
        page: Option<u32>,
    },
    Render {
        input: &'a Path,
        prefix: &'a Path,
        page: u32,
        dpi: f64,
        scale_to: Option<u32>,
        cropbox: bool,
    },
    Text {
        input: &'a Path,
    },
}

impl PdfTool<'_> {
    pub(crate) fn command(&self, scratch: &Path) -> Result<Command, ProcessingError> {
        let mut command = match self {
            Self::Info { .. } => Command::new(poppler_binary("pdfinfo")?),
            Self::Render { .. } => Command::new(poppler_binary("pdftoppm")?),
            Self::Text { .. } => Command::new(poppler_binary("pdftotext")?),
        };
        match self {
            Self::Info { input, page } => {
                if let Some(page) = page {
                    if *page == 0 {
                        return Err(ProcessingError::permanent("invalid PDF page number"));
                    }
                    command.args(["-f", &page.to_string(), "-l", &page.to_string(), "-box"]);
                }
                command.arg(input);
            }
            Self::Render {
                input,
                prefix,
                page,
                dpi,
                scale_to,
                cropbox,
            } => {
                if *page == 0
                    || !dpi.is_finite()
                    || *dpi <= 0.0
                    || *dpi > 600.0
                    || scale_to.is_some_and(|edge| !(32..=8192).contains(&edge))
                {
                    return Err(ProcessingError::permanent("invalid PDF render parameters"));
                }
                command.args([
                    "-f",
                    &page.to_string(),
                    "-l",
                    &page.to_string(),
                    "-singlefile",
                    "-png",
                    "-r",
                    &dpi.to_string(),
                ]);
                if *cropbox {
                    command.arg("-cropbox");
                }
                if let Some(edge) = scale_to {
                    command.args(["-scale-to", &edge.to_string()]);
                }
                command.arg(input).arg(prefix);
            }
            Self::Text { input } => {
                command
                    .args(["-bbox-layout", "-cropbox", "-enc", "UTF-8"])
                    .arg(input)
                    .arg("-");
            }
        }
        clean_environment(&mut command, scratch);
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_arbitrary_programs_relative_layout_and_invalid_render_options() {
        assert!(poppler_binary("sh").is_err());
        assert!(layout_binary(Path::new("./target/debug/layout-detect")).is_err());
        assert!(layout_binary(Path::new("fake-from-path")).is_err());
        for dpi in [f64::NAN, f64::INFINITY, 0.0, -1.0, 601.0] {
            assert!(
                PdfTool::Render {
                    input: Path::new("/a"),
                    prefix: Path::new("/b"),
                    page: 1,
                    dpi,
                    scale_to: None,
                    cropbox: true
                }
                .command(Path::new("/tmp"))
                .is_err()
            );
        }
    }

    #[test]
    fn scrub_removes_explicit_and_inherited_environment_in_actual_child() {
        // Re-exec this one test with synthetic secrets, avoiding unsafe global
        // environment mutation in a multithreaded test process.
        const SENTINEL: &str = "a06-synthetic-parent-secret-do-not-forward";
        if std::env::var("OCR_A06_ENV_PROBE").as_deref() != Ok(SENTINEL) {
            let poisoned = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "subprocess_policy::tests::scrub_removes_explicit_and_inherited_environment_in_actual_child"])
                .env("OCR_A06_ENV_PROBE", SENTINEL)
                .env("DATABASE_URL", SENTINEL)
                .env("DATABASE_URL_FILE", SENTINEL)
                .env("VLLM_API_KEY", SENTINEL)
                .env("REDIS_URL", SENTINEL)
                .env("HTTPS_PROXY", SENTINEL)
                .env("PATH", poisoned.path())
                .output().unwrap();
            assert!(output.status.success(), "isolated environment probe failed");
            return;
        }
        let scratch = tempfile::tempdir().unwrap();
        if let Ok(tool) = poppler_binary("pdfinfo") {
            assert!(tool.is_absolute());
            assert!(
                TOOL_DIRS
                    .iter()
                    .any(|directory| tool.parent() == Some(Path::new(directory)))
            );
            assert_ne!(
                tool.parent(),
                Some(Path::new(&std::env::var("PATH").unwrap()))
            );
        }
        let mut command = Command::new("/usr/bin/env");
        command
            .env("EXPLICIT_SECRET", SENTINEL)
            .env("LD_PRELOAD", SENTINEL)
            .env("LD_LIBRARY_PATH", SENTINEL)
            .env("DYLD_INSERT_LIBRARIES", SENTINEL);
        clean_environment(&mut command, scratch.path());
        let output = command.as_std_mut().output().unwrap();
        assert!(output.status.success());
        let value = String::from_utf8(output.stdout).unwrap();
        assert!(!value.contains(SENTINEL));
        assert!(!value.contains("DATABASE_URL"));
        assert!(!value.contains("PROXY"));
        assert!(value.contains("LC_ALL=C\n"));
        assert!(value.contains("PATH=/usr/bin:/bin\n"));
        assert!(value.contains("TMPDIR="));
        assert_eq!(value.lines().count(), 6);
    }

    #[test]
    fn ort_is_the_only_explicit_layout_extension() {
        let scratch = tempfile::tempdir().unwrap();
        let library = scratch.path().join("trusted-runtime");
        std::fs::write(&library, b"fixture").unwrap();
        let mut command = Command::new("unused");
        assert!(
            layout_environment(&mut command, scratch.path(), Some(Path::new("relative"))).is_err()
        );
        layout_environment(&mut command, scratch.path(), Some(&library)).unwrap();
        let values: Vec<_> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect();
        assert_eq!(values.len(), 7);
        assert!(
            values.iter().any(|(key, value)| key == "ORT_DYLIB_PATH"
                && value.as_deref() == Some(library.as_os_str()))
        );
    }
}
