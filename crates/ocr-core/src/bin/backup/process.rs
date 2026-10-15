//! Bounded standard-tool supervision. Commands/paths/credentials are prepared by
//! the caller; this module never formats them or exposes child stderr.
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, timeout, timeout_at},
};

const BUFFER_BYTES: usize = 64 * 1024;
pub const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoDigest {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineDigest {
    pub plain: IoDigest,
    pub cipher: IoDigest,
}

/// Fixed diagnostics only: no underlying I/O error, stderr, path, argv or input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProcessError {
    #[error("tool operation has invalid bounds or deadline")]
    InvalidBounds,
    #[error("tool stdin exceeds the capture input budget")]
    InputLimit,
    #[error("tool stdout exceeds its byte budget")]
    OutputLimit,
    #[error("tool could not be started (details redacted)")]
    Spawn,
    #[error("tool pipe is unavailable (details redacted)")]
    Pipe,
    #[error("tool stream or output synchronization failed (details redacted)")]
    Io,
    #[error("tool exited unsuccessfully (details redacted)")]
    Exit,
    #[error("tool operation deadline elapsed")]
    Deadline,
    #[error("tool cleanup did not complete within its deadline")]
    Cleanup,
    #[cfg(not(unix))]
    #[error("tool process-group supervision requires Unix")]
    UnsupportedPlatform,
}

/// The process-group identity survives a direct child's exit so failure cleanup
/// also reaches descendants with redirected stdio. No command data is retained.
struct Supervised {
    child: Option<Child>,
    group: i32,
    reaped: bool,
}

impl Supervised {
    fn spawn(command: Command, input: bool) -> Result<Self, ProcessError> {
        Self::spawn_input(command, if input { Stdio::piped() } else { Stdio::null() })
    }

    fn spawn_input(mut command: Command, input: Stdio) -> Result<Self, ProcessError> {
        #[cfg(not(unix))]
        {
            let _ = (&mut command, input);
            return Err(ProcessError::UnsupportedPlatform);
        }
        #[cfg(unix)]
        {
            command
                .stdin(input)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .process_group(0);
            let mut child = command.spawn().map_err(|_| ProcessError::Spawn)?;
            let Some(group) = child.id().and_then(|pid| i32::try_from(pid).ok()) else {
                let _ = child.start_kill();
                return Err(ProcessError::Spawn);
            };
            Ok(Self {
                child: Some(child),
                group,
                reaped: false,
            })
        }
    }

    fn stdout(&mut self) -> Result<ChildStdout, ProcessError> {
        self.child
            .as_mut()
            .and_then(|c| c.stdout.take())
            .ok_or(ProcessError::Pipe)
    }

    fn stdin(&mut self) -> Result<ChildStdin, ProcessError> {
        self.child
            .as_mut()
            .and_then(|c| c.stdin.take())
            .ok_or(ProcessError::Pipe)
    }

    async fn wait_success(&mut self) -> Result<(), ProcessError> {
        let status = self
            .child
            .as_mut()
            .ok_or(ProcessError::Pipe)?
            .wait()
            .await
            .map_err(|_| ProcessError::Io)?;
        self.reaped = true;
        if status.success() {
            Ok(())
        } else {
            Err(ProcessError::Exit)
        }
    }

    fn signal_group(&mut self) {
        #[cfg(unix)]
        if self.group > 0 {
            // Each spawn establishes PGID=its PID. Negative PID targets only
            // that group; never signal the caller's process group or PID zero.
            unsafe {
                libc::kill(-self.group, libc::SIGKILL);
            }
        }
        if !self.reaped
            && let Some(child) = self.child.as_mut()
        {
            let _ = child.start_kill();
        }
    }

    async fn terminate(&mut self) -> bool {
        self.signal_group();
        let settled = if self.reaped {
            true
        } else if let Some(child) = self.child.as_mut() {
            matches!(timeout(CLEANUP_BUDGET, child.wait()).await, Ok(Ok(_)))
        } else {
            true
        };
        if settled {
            self.reaped = true;
            self.disarm();
        }
        settled
    }

    fn disarm(&mut self) {
        self.group = 0;
        drop(self.child.take());
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if self.group == 0 {
            return;
        }
        self.signal_group();
        if let Some(mut child) = self.child.take()
            && !self.reaped
        {
            // Drop cannot await. Signal synchronously above, then retain the
            // child in a bounded reaper task. kill_on_drop and Tokio's Unix
            // reaper remain the fallback if the runtime is shutting down.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                drop(runtime.spawn(async move {
                    let _ = timeout(CLEANUP_BUDGET, child.wait()).await;
                }));
            }
        }
    }
}

fn deadline(budget: Duration, valid_bounds: bool) -> Result<Instant, ProcessError> {
    if budget.is_zero() || !valid_bounds {
        return Err(ProcessError::InvalidBounds);
    }
    Instant::now()
        .checked_add(budget)
        .ok_or(ProcessError::InvalidBounds)
}

async fn copy_digest<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    input: &mut R,
    output: &mut W,
    max_bytes: u64,
) -> Result<IoDigest, ProcessError> {
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    loop {
        let count = input
            .read(&mut buffer)
            .await
            .map_err(|_| ProcessError::Io)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or(ProcessError::OutputLimit)?;
        if bytes > max_bytes {
            return Err(ProcessError::OutputLimit);
        }
        output
            .write_all(&buffer[..count])
            .await
            .map_err(|_| ProcessError::Io)?;
        hash.update(&buffer[..count]);
    }
    output.flush().await.map_err(|_| ProcessError::Io)?;
    Ok(IoDigest {
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    })
}

async fn capture<R: AsyncRead + Unpin>(
    input: &mut R,
    max_bytes: usize,
) -> Result<Vec<u8>, ProcessError> {
    let mut output = Vec::with_capacity(max_bytes.min(BUFFER_BYTES));
    let mut buffer = vec![0_u8; BUFFER_BYTES.min(max_bytes + 1)];
    loop {
        let count = input
            .read(&mut buffer)
            .await
            .map_err(|_| ProcessError::Io)?;
        if count == 0 {
            return Ok(output);
        }
        if count > max_bytes - output.len() {
            return Err(ProcessError::OutputLimit);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

/// Stream pg_dump → age without a plaintext archive file. Caller must remove
/// partial output on failure; no artifact is publishable until this succeeds.
pub async fn encrypt_pipeline(
    dump: Command,
    age: Command,
    mut cipher_output: File,
    max_plain: u64,
    max_cipher: u64,
    budget: Duration,
) -> Result<PipelineDigest, ProcessError> {
    let expires = deadline(budget, max_plain > 0 && max_cipher > 0)?;
    let mut producer = Supervised::spawn(dump, false)?;
    let mut encryptor = match Supervised::spawn(age, true) {
        Ok(child) => child,
        Err(error) => {
            return if producer.terminate().await {
                Err(error)
            } else {
                Err(ProcessError::Cleanup)
            };
        }
    };
    let outcome = timeout_at(expires, async {
        let mut plain_output = producer.stdout()?;
        let mut plain_input = encryptor.stdin()?;
        let mut encrypted_output = encryptor.stdout()?;
        let plain = async {
            let digest = copy_digest(&mut plain_output, &mut plain_input, max_plain).await?;
            plain_input.shutdown().await.map_err(|_| ProcessError::Io)?;
            drop(plain_input);
            Ok::<_, ProcessError>(digest)
        };
        let cipher = copy_digest(&mut encrypted_output, &mut cipher_output, max_cipher);
        let (plain, cipher) = tokio::try_join!(plain, cipher)?;
        tokio::try_join!(producer.wait_success(), encryptor.wait_success())?;
        cipher_output
            .sync_all()
            .await
            .map_err(|_| ProcessError::Io)?;
        Ok(PipelineDigest { plain, cipher })
    })
    .await
    .unwrap_or(Err(ProcessError::Deadline));
    match outcome {
        Ok(digest) => {
            producer.disarm();
            encryptor.disarm();
            Ok(digest)
        }
        Err(error) => {
            let (first, second) = tokio::join!(producer.terminate(), encryptor.terminate());
            if first && second {
                Err(error)
            } else {
                Err(ProcessError::Cleanup)
            }
        }
    }
}

/// Stream a tool's stdout to a caller-created private file, with no stderr data.
#[cfg(test)]
pub async fn run_to_file(
    command: Command,
    output: File,
    max_bytes: u64,
    budget: Duration,
) -> Result<IoDigest, ProcessError> {
    run_output(command, Stdio::null(), output, max_bytes, budget).await
}

/// Feed an already opened, checked file directly to child stdin. The caller
/// controls its offset; no input pathname is reopened or added to argv here.
pub async fn run_file_to_file(
    command: Command,
    input: std::fs::File,
    output: File,
    max_bytes: u64,
    budget: Duration,
) -> Result<IoDigest, ProcessError> {
    run_output(command, Stdio::from(input), output, max_bytes, budget).await
}

async fn run_output(
    command: Command,
    input: Stdio,
    mut output: File,
    max_bytes: u64,
    budget: Duration,
) -> Result<IoDigest, ProcessError> {
    let expires = deadline(budget, max_bytes > 0)?;
    let mut child = Supervised::spawn_input(command, input)?;
    let outcome = timeout_at(expires, async {
        let digest = copy_digest(&mut child.stdout()?, &mut output, max_bytes).await?;
        child.wait_success().await?;
        output.sync_all().await.map_err(|_| ProcessError::Io)?;
        Ok(digest)
    })
    .await
    .unwrap_or(Err(ProcessError::Deadline));
    match outcome {
        Ok(digest) => {
            child.disarm();
            Ok(digest)
        }
        Err(error) => {
            if child.terminate().await {
                Err(error)
            } else {
                Err(ProcessError::Cleanup)
            }
        }
    }
}

/// Small version/manifest exchanges only. Both captured output and optional
/// stdin have an absolute 256KiB ceiling; the caller may choose a smaller output.
pub async fn run_capture(
    command: Command,
    stdin: Option<Vec<u8>>,
    max_bytes: usize,
    budget: Duration,
) -> Result<Vec<u8>, ProcessError> {
    if stdin
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_CAPTURE_BYTES)
    {
        return Err(ProcessError::InputLimit);
    }
    let expires = deadline(budget, (1..=MAX_CAPTURE_BYTES).contains(&max_bytes))?;
    let mut child = Supervised::spawn(command, stdin.is_some())?;
    let outcome = timeout_at(expires, async {
        let mut stdout = child.stdout()?;
        let input = if stdin.is_some() {
            Some(child.stdin()?)
        } else {
            None
        };
        let write = async move {
            if let (Some(mut input), Some(bytes)) = (input, stdin) {
                input
                    .write_all(&bytes)
                    .await
                    .map_err(|_| ProcessError::Io)?;
                input.shutdown().await.map_err(|_| ProcessError::Io)?;
            }
            Ok::<_, ProcessError>(())
        };
        let (output, ()) = tokio::try_join!(capture(&mut stdout, max_bytes), write)?;
        child.wait_success().await?;
        Ok(output)
    })
    .await
    .unwrap_or(Err(ProcessError::Deadline));
    match outcome {
        Ok(output) => {
            child.disarm();
            Ok(output)
        }
        Err(error) => {
            if child.terminate().await {
                Err(error)
            } else {
                Err(ProcessError::Cleanup)
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    const BUDGET: Duration = Duration::from_secs(5);

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", script, "backup-process-test"]);
        command
    }

    fn cat() -> Command {
        let mut command = Command::new("/bin/cat");
        command.env_clear();
        command
    }

    fn file() -> (tempfile::NamedTempFile, File) {
        let private = tempfile::NamedTempFile::new().unwrap();
        let output = File::from_std(private.reopen().unwrap());
        (private, output)
    }

    fn digest(bytes: &[u8]) -> IoDigest {
        IoDigest {
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }

    async fn pid(path: &Path) -> i32 {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = text.parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture must publish its process identity")
    }

    async fn not_running(pid: i32) {
        timeout(Duration::from_secs(2), async {
            loop {
                let status = std::process::Command::new("/bin/ps")
                    .env_clear()
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .unwrap();
                let state = String::from_utf8_lossy(&status.stdout);
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervised process and descendants must stop running")
    }

    #[tokio::test]
    async fn capture_is_duplex_bounded_and_discards_stderr() {
        let bytes = vec![b'x'; 200 * 1024];
        assert_eq!(
            run_capture(cat(), Some(bytes.clone()), MAX_CAPTURE_BYTES, BUDGET)
                .await
                .unwrap(),
            bytes
        );
        assert_eq!(
            run_capture(
                shell("printf public; printf PRIVATE_STDERR_SENTINEL >&2"),
                None,
                6,
                BUDGET
            )
            .await
            .unwrap(),
            b"public"
        );
        assert_eq!(
            run_capture(shell("printf 12345"), None, 4, BUDGET)
                .await
                .unwrap_err(),
            ProcessError::OutputLimit
        );
    }

    #[tokio::test]
    async fn rejected_bounds_and_spawn_or_exit_errors_are_fixed_and_redacted() {
        let mut missing = Command::new("/nonexistent/PRIVATE_TOOL_SENTINEL");
        missing.env_clear();
        assert_eq!(
            run_capture(missing, None, 8, BUDGET).await.unwrap_err(),
            ProcessError::Spawn
        );
        let error = run_capture(
            shell("printf PRIVATE_STDERR_SENTINEL >&2; exit 23"),
            None,
            8,
            BUDGET,
        )
        .await
        .unwrap_err();
        assert_eq!(error, ProcessError::Exit);
        assert!(!format!("{error:?} {error}").contains("SENTINEL"));
        assert!(std::error::Error::source(&error).is_none());
        for (limit, budget) in [
            (0, BUDGET),
            (MAX_CAPTURE_BYTES + 1, BUDGET),
            (8, Duration::ZERO),
        ] {
            assert_eq!(
                run_capture(cat(), None, limit, budget).await.unwrap_err(),
                ProcessError::InvalidBounds
            );
        }
        assert_eq!(
            run_capture(cat(), Some(vec![0; MAX_CAPTURE_BYTES + 1]), 8, BUDGET)
                .await
                .unwrap_err(),
            ProcessError::InputLimit
        );
    }

    #[tokio::test]
    async fn file_and_pipeline_hash_exact_streams_and_require_success() {
        let (path, output) = file();
        assert_eq!(
            run_to_file(shell("printf archive-data"), output, 100, BUDGET)
                .await
                .unwrap(),
            digest(b"archive-data")
        );
        assert_eq!(tokio::fs::read(path.path()).await.unwrap(), b"archive-data");
        let (path, output) = file();
        let mut upper = Command::new("/usr/bin/tr");
        upper.env_clear().args(["a-z", "A-Z"]);
        let value = encrypt_pipeline(
            shell("printf archive-data"),
            upper,
            output,
            100,
            100,
            BUDGET,
        )
        .await
        .unwrap();
        assert_eq!(
            value,
            PipelineDigest {
                plain: digest(b"archive-data"),
                cipher: digest(b"ARCHIVE-DATA")
            }
        );
        assert_eq!(tokio::fs::read(path.path()).await.unwrap(), b"ARCHIVE-DATA");
        let (_path, output) = file();
        assert_eq!(
            encrypt_pipeline(
                shell("printf data; exit 23"),
                cat(),
                output,
                100,
                100,
                BUDGET
            )
            .await
            .unwrap_err(),
            ProcessError::Exit
        );
    }

    #[tokio::test]
    async fn each_pipeline_byte_limit_and_file_limit_fails_closed() {
        for (plain, cipher) in [(4, 100), (100, 4)] {
            let (_path, output) = file();
            assert_eq!(
                encrypt_pipeline(shell("printf 12345"), cat(), output, plain, cipher, BUDGET)
                    .await
                    .unwrap_err(),
                ProcessError::OutputLimit
            );
        }
        let (path, output) = file();
        assert_eq!(
            run_to_file(shell("printf 12345"), output, 4, BUDGET)
                .await
                .unwrap_err(),
            ProcessError::OutputLimit
        );
        assert!(tokio::fs::metadata(path.path()).await.unwrap().len() <= 4);
    }

    #[tokio::test]
    async fn checked_file_input_is_consumed_through_stdin_without_an_argument() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), b"checked-file-input").unwrap();
        let (path, output) = file();
        let result = run_file_to_file(cat(), input.reopen().unwrap(), output, 100, BUDGET)
            .await
            .unwrap();
        assert_eq!(result, digest(b"checked-file-input"));
        assert_eq!(
            tokio::fs::read(path.path()).await.unwrap(),
            b"checked-file-input"
        );
    }

    #[tokio::test]
    async fn timeout_kills_group_and_reaps_direct_child() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent");
        let descendant = directory.path().join("descendant");
        let mut command =
            shell("printf '%s' \"$$\" > \"$1\"; /bin/sleep 30 & printf '%s' \"$!\" > \"$2\"; wait");
        command.args([&parent, &descendant]);
        let operation = tokio::spawn(run_capture(command, None, 8, Duration::from_millis(500)));
        let parent_pid = pid(&parent).await;
        let descendant_pid = pid(&descendant).await;
        assert_eq!(
            operation.await.unwrap().unwrap_err(),
            ProcessError::Deadline
        );
        not_running(parent_pid).await;
        not_running(descendant_pid).await;
        // The supervisor, rather than this test, already reaped the direct child.
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(parent_pid, &raw mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[tokio::test]
    async fn cancellation_guard_kills_descendants_and_reaps_without_waiting_for_budget() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent");
        let descendant = directory.path().join("descendant");
        let mut command =
            shell("printf '%s' \"$$\" > \"$1\"; /bin/sleep 30 & printf '%s' \"$!\" > \"$2\"; wait");
        command.args([&parent, &descendant]);
        let operation = tokio::spawn(run_capture(command, None, 8, Duration::from_secs(30)));
        let parent_pid = pid(&parent).await;
        let descendant_pid = pid(&descendant).await;
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());
        not_running(parent_pid).await;
        not_running(descendant_pid).await;
    }

    #[tokio::test]
    async fn pipeline_timeout_stops_both_process_groups_and_their_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            ["dump", "dump-child", "age", "age-child"].map(|name| directory.path().join(name));
        let script =
            "printf '%s' \"$$\" > \"$1\"; /bin/sleep 30 & printf '%s' \"$!\" > \"$2\"; wait";
        let mut dump = shell(script);
        dump.args([&paths[0], &paths[1]]);
        let mut age = shell(script);
        age.args([&paths[2], &paths[3]]);
        let (_path, output) = file();
        let operation = tokio::spawn(encrypt_pipeline(
            dump,
            age,
            output,
            100,
            100,
            Duration::from_millis(500),
        ));
        let mut pids = Vec::new();
        for path in &paths {
            pids.push(pid(path).await);
        }
        assert_eq!(
            operation.await.unwrap().unwrap_err(),
            ProcessError::Deadline
        );
        for pid in pids {
            not_running(pid).await;
        }
    }

    #[tokio::test]
    async fn failure_kills_descendant_after_group_leader_already_exited() {
        let directory = tempfile::tempdir().unwrap();
        let descendant = directory.path().join("descendant");
        let mut command = shell(
            "/bin/sleep 30 </dev/null >/dev/null 2>&1 & printf '%s' \"$!\" > \"$1\"; exit 17",
        );
        command.arg(&descendant);
        assert_eq!(
            run_capture(command, None, 8, BUDGET).await.unwrap_err(),
            ProcessError::Exit
        );
        not_running(pid(&descendant).await).await;
    }

    #[tokio::test]
    async fn failing_downstream_terminates_unbounded_upstream() {
        let (_path, output) = file();
        let result = timeout(
            Duration::from_secs(3),
            encrypt_pipeline(
                shell("while :; do printf upstream; done"),
                shell("exit 19"),
                output,
                u64::MAX,
                u64::MAX,
                BUDGET,
            ),
        )
        .await
        .expect("broken downstream must not leave its producer running");
        assert!(matches!(result, Err(ProcessError::Io | ProcessError::Exit)));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn enospc_and_sync_failure_never_return_successful_digest() {
        let output = tokio::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .await
            .unwrap();
        assert_eq!(
            run_to_file(shell("printf data"), output, 100, BUDGET)
                .await
                .unwrap_err(),
            ProcessError::Io
        );
        let output = tokio::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .await
            .unwrap();
        assert_eq!(
            run_to_file(shell("printf data"), output, 100, BUDGET)
                .await
                .unwrap_err(),
            ProcessError::Io
        );
    }
}
