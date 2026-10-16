//! One-shot native-tool supervision, separate from command/environment policy.
//!
//! A detached supervisor owns the admission permit and caller-supplied resources
//! until the direct child is reaped. Dropping the requesting future closes its
//! reply channel and requests cleanup; it cannot release the slot or temporary
//! input while cleanup still runs. Every spawned child gets a Unix process group;
//! normal exit as well as failure kills remaining members of that group.
//!
//! This is not a sandbox: a process can deliberately escape with setsid/setpgid,
//! and grandchildren need an init/subreaper to reap them. CPU, address space,
//! scratch bytes, filesystem access and network require the separate isolation
//! boundary. Runtime shutdown/process death also requires that boundary's init.
use std::{
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::{Semaphore, oneshot},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use crate::ProcessingError;

const OUTPUT_LIMIT: usize = 256 * 1024;
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);
static ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Bounds,
    Spawn,
    Pipe,
    Io,
    Exit,
    OutputLimit,
    Deadline,
    AdmissionDeadline,
    Cancelled,
    Cleanup,
    #[cfg(not(unix))]
    Platform,
}
impl Failure {
    fn processing(self) -> ProcessingError {
        match self {
            Self::Bounds => ProcessingError::permanent("native process deadline is invalid"),
            Self::Spawn => ProcessingError::transient("cannot start native document tool"),
            Self::Pipe => ProcessingError::transient("native document tool pipe is unavailable"),
            Self::Io => ProcessingError::transient("cannot read or wait for native document tool"),
            Self::Exit => ProcessingError::permanent(
                "Poppler could not process this PDF; it may be corrupt or encrypted",
            ),
            Self::OutputLimit => {
                ProcessingError::permanent("PDF process output exceeded its size limit")
            }
            Self::Deadline => ProcessingError::permanent("PDF inspection or rendering timed out"),
            Self::AdmissionDeadline => {
                ProcessingError::transient("native process admission timed out")
            }
            Self::Cancelled => ProcessingError::cancelled(),
            Self::Cleanup => ProcessingError::transient(
                "native process cleanup is still pending; processing slot remains held",
            ),
            #[cfg(not(unix))]
            Self::Platform => {
                ProcessingError::permanent("native process supervision requires Unix")
            }
        }
    }
}

/// Run a caller-built command with null stdin and independently bounded stdout/
/// stderr. Only stdout is returned. `resources` should own every temporary input
/// and output directory the child can access (usually an `Arc<TempDir>`).
///
/// A single process-wide slot covers admission, execution and cleanup; the total
/// deadline includes queue time. Cancellation while queued never spawns a child.
/// Caller cancellation is observed even if only its async future is dropped.
/// After a cleanup deadline the caller receives an error, while the detached
/// supervisor retains resources and admission until its direct child is reaped.
pub(crate) async fn capture<R: Send + 'static>(
    command: Command,
    resources: R,
    budget: Duration,
    cancelled: CancellationToken,
) -> Result<Vec<u8>, ProcessingError> {
    let expires = Instant::now()
        .checked_add(budget)
        .filter(|_| !budget.is_zero())
        .ok_or_else(|| Failure::Bounds.processing())?;
    let (reply, receive) = oneshot::channel();
    let admission = ADMISSION
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone();
    drop(tokio::spawn(supervise(
        command, resources, expires, cancelled, reply, admission,
    )));
    receive
        .await
        .map_err(|_| ProcessingError::transient("native process supervisor stopped"))?
        .map_err(Failure::processing)
}

async fn supervise<R: Send + 'static>(
    command: Command,
    resources: R,
    expires: Instant,
    cancelled: CancellationToken,
    mut reply: oneshot::Sender<Result<Vec<u8>, Failure>>,
    admission: Arc<Semaphore>,
) {
    let owned_resources = resources;
    let permit = tokio::select! {
        biased;
        _=reply.closed()=>return,
        _=cancelled.cancelled()=>{let _=reply.send(Err(Failure::Cancelled));return;},
        permit=timeout_at(expires,admission.acquire_owned())=>match permit {
            Ok(Ok(permit))=>permit,
            _=>{let _=reply.send(Err(Failure::AdmissionDeadline));return;}
        },
    };
    let _permit = permit;
    // Cancellation may have arrived as admission became available.
    if reply.is_closed() {
        return;
    }
    if cancelled.is_cancelled() {
        let _ = reply.send(Err(Failure::Cancelled));
        return;
    }
    if Instant::now() >= expires {
        let _ = reply.send(Err(Failure::Deadline));
        return;
    }
    let mut child = match Supervised::spawn(command) {
        Ok(child) => child,
        Err(error) => {
            let _ = reply.send(Err(error));
            return;
        }
    };
    let output = tokio::select! {
        biased;
        _=reply.closed()=>Err(Failure::Cancelled),
        _=cancelled.cancelled()=>Err(Failure::Cancelled),
        output=timeout_at(expires,child.output())=>output.unwrap_or(Err(Failure::Deadline)),
    };
    if !child.finish().await {
        let _ = reply.send(Err(Failure::Cleanup));
        // Fail closed: never release the slot/input merely because SIGKILL's
        // direct-child wait exceeded its budget (e.g. uninterruptible kernel I/O).
        if !child.group_stopped {
            std::future::pending::<()>().await;
        }
        match child.child.wait().await {
            Ok(_) => child.reaped = true,
            Err(_) => std::future::pending::<()>().await,
        }
        drop(owned_resources);
        return;
    }
    // Remove scratch before returning admission to another native operation.
    drop(owned_resources);
    let _ = reply.send(output);
}

struct Supervised {
    child: Child,
    group: i32,
    reaped: bool,
    group_stopped: bool,
}
impl Supervised {
    fn spawn(mut command: Command) -> Result<Self, Failure> {
        #[cfg(not(unix))]
        {
            let _ = &mut command;
            Err(Failure::Platform)
        }
        #[cfg(unix)]
        {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .process_group(0);
            let mut child = command.spawn().map_err(|_| Failure::Spawn)?;
            let Some(group) = child.id().and_then(|id| i32::try_from(id).ok()) else {
                let _ = child.start_kill();
                return Err(Failure::Spawn);
            };
            Ok(Self {
                child,
                group,
                reaped: false,
                group_stopped: false,
            })
        }
    }
    fn stop_group(&mut self) -> bool {
        let group = std::mem::replace(&mut self.group, 0);
        if group <= 0 {
            return self.group_stopped;
        }
        #[cfg(unix)]
        {
            self.group_stopped = signal_group(group);
            self.group_stopped
        }
        #[cfg(not(unix))]
        false
    }
    async fn wait(&mut self) -> Result<std::process::ExitStatus, Failure> {
        // WNOWAIT observes exit without freeing the leader's PID. Keep that
        // identity pinned until group signalling, preventing PGID reuse from
        // redirecting a delayed cleanup signal to an unrelated process group.
        observe_exit(self.group as u32)
            .await
            .map_err(|_| Failure::Io)?;
        // Stop redirected or pipe-holding descendants even when the group
        // leader itself returned success. Never wait for their EOF first.
        if !self.stop_group() {
            return Err(Failure::Cleanup);
        }
        let status = self.child.wait().await.map_err(|_| Failure::Io)?;
        self.reaped = true;
        Ok(status)
    }
    async fn output(&mut self) -> Result<Vec<u8>, Failure> {
        let stdout = self.child.stdout.take().ok_or(Failure::Pipe)?;
        let stderr = self.child.stderr.take().ok_or(Failure::Pipe)?;
        let (output, (), status) = tokio::try_join!(
            bounded(stdout, true),
            async { bounded(stderr, false).await.map(|_| ()) },
            self.wait()
        )?;
        if status.success() {
            Ok(output)
        } else {
            Err(Failure::Exit)
        }
    }
    async fn finish(&mut self) -> bool {
        let signalled = self.stop_group();
        if self.reaped {
            return signalled;
        }
        let _ = self.child.start_kill();
        if matches!(timeout(CLEANUP_BUDGET, self.child.wait()).await, Ok(Ok(_))) {
            self.reaped = true;
            signalled
        } else {
            false
        }
    }
}

#[cfg(unix)]
pub(crate) fn signal_group(group: i32) -> bool {
    if group <= 0 {
        return false;
    }
    loop {
        if unsafe { libc::kill(-group, libc::SIGKILL) } == 0 {
            return true;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ESRCH) => return true,
            // XNU excludes zombies from killpg's permission iteration and
            // returns EPERM for a zombie-only group. Do not weaken all EPERM:
            // bounded libproc inspection must prove no live member remains.
            #[cfg(target_os = "macos")]
            Some(libc::EPERM) => return macos_group_quiescent(group),
            _ => return false,
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_group_quiescent(group: i32) -> bool {
    if !matches!(exited_without_reaping(group as u32), Ok(true)) {
        return false;
    }
    const PROC_PGRP_ONLY: u32 = 2; // Apple's <sys/proc_info.h>.
    let mut members = [0_i32; 256];
    let capacity = std::mem::size_of_val(&members) as i32;
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            group as u32,
            members.as_mut_ptr().cast(),
            capacity,
        )
    };
    if bytes <= 0 || bytes >= capacity || bytes % std::mem::size_of::<i32>() as i32 != 0 {
        return false;
    }
    if !members[..bytes as usize / std::mem::size_of::<i32>()].contains(&group) {
        return false;
    }
    for pid in members
        .into_iter()
        .take(bytes as usize / std::mem::size_of::<i32>())
    {
        if pid <= 0 {
            continue;
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let count = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        if count == size {
            if info.pbi_status != libc::SZOMB {
                return false;
            }
        } else if count != 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return false;
        }
    }
    true
}

/// Observe a direct child without reaping it. Register before checking so an
/// exit between waitid and recv cannot lose the SIGCHLD notification. Tokio's
/// own Child::wait is polled only after signalling the pinned process group.
pub(crate) async fn observe_exit(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut changed = signal(SignalKind::child())?;
        loop {
            if exited_without_reaping(pid)? {
                return Ok(());
            }
            if changed.recv().await.is_none() {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}
impl Drop for Supervised {
    fn drop(&mut self) {
        let _ = self.stop_group();
        if !self.reaped {
            let _ = self.child.start_kill();
        }
        // Tokio's Unix orphan reaper and the container init are the runtime-
        // shutdown fallback; ordinary exits explicitly wait before resource drop.
    }
}

#[cfg(unix)]
fn exited_without_reaping(pid: u32) -> std::io::Result<bool> {
    // siginfo_t contains a raw pointer on some Unix platforms. Keep it in a
    // synchronous frame so no !Send native value survives an async suspension.
    loop {
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let status = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut information,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if status < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        return Ok(unsafe { information.si_pid() } != 0);
    }
}

async fn bounded(mut reader: impl AsyncRead + Unpin, retain: bool) -> Result<Vec<u8>, Failure> {
    let mut output = Vec::new();
    let mut total = 0_usize;
    let mut buffer = [0_u8; 8192];
    loop {
        let size = reader.read(&mut buffer).await.map_err(|_| Failure::Io)?;
        if size == 0 {
            return Ok(output);
        }
        if size > OUTPUT_LIMIT - total {
            return Err(Failure::OutputLimit);
        }
        total += size;
        if retain {
            output.extend_from_slice(&buffer[..size]);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, Ordering},
    };

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", script, "native-supervision-test"]);
        command
    }
    async fn isolated(
        command: Command,
        budget: Duration,
        token: CancellationToken,
    ) -> Result<Vec<u8>, Failure> {
        let (reply, receive) = oneshot::channel();
        drop(tokio::spawn(supervise(
            command,
            (),
            Instant::now() + budget,
            token,
            reply,
            Arc::new(Semaphore::new(1)),
        )));
        receive.await.unwrap()
    }
    async fn pid(path: &Path) -> i32 {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = value.parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("child must publish PID")
    }
    async fn stopped(pid: i32) {
        timeout(Duration::from_secs(3), async {
            loop {
                let output = std::process::Command::new("/bin/ps")
                    .env_clear()
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                let state = String::from_utf8_lossy(&output.stdout);
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("group member must stop running")
    }
    fn reaped(pid: i32) {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    #[tokio::test]
    async fn exit_observation_keeps_the_leader_waitable_until_group_cleanup() {
        let child = Supervised::spawn(shell("exit 0")).unwrap();
        let mut child = child;
        let pid = child.group;
        observe_exit(pid as u32).await.unwrap();
        assert!(exited_without_reaping(pid as u32).unwrap());
        // Repeating the observation succeeds: the PID is still pinned, rather
        // than exposing a numeric process-group ID after leader reaping.
        assert!(exited_without_reaping(pid as u32).unwrap());
        assert!(child.finish().await);
        reaped(pid);
    }
    #[tokio::test]
    async fn bounded_output_null_stdin_and_fixed_redacted_errors() {
        assert_eq!(
            isolated(
                shell("if read unused; then exit 7; fi; printf public; printf SECRET_SENTINEL >&2"),
                Duration::from_secs(3),
                CancellationToken::new()
            )
            .await
            .unwrap(),
            b"public"
        );
        for stream in ["", " >&2"] {
            let script = format!("/usr/bin/head -c 262145 /dev/zero{stream}; /bin/sleep 30");
            assert_eq!(
                isolated(
                    shell(&script),
                    Duration::from_secs(3),
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
                Failure::OutputLimit
            );
        }
        let error = isolated(
            shell("printf SECRET_SENTINEL >&2; exit 9"),
            Duration::from_secs(3),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, Failure::Exit);
        assert!(!error.processing().message.contains("SENTINEL"));
    }
    #[tokio::test]
    async fn timeout_and_cancellation_kill_group_and_reap_direct_child() {
        for cancel in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let parent = directory.path().join("parent");
            let descendant = directory.path().join("descendant");
            let mut command = shell(
                "printf '%s' \"$$\" > \"$1\"; /bin/sleep 30 & printf '%s' \"$!\" > \"$2\"; wait",
            );
            command.args([&parent, &descendant]);
            let token = CancellationToken::new();
            let own = token.clone();
            let operation = tokio::spawn(isolated(
                command,
                if cancel {
                    Duration::from_secs(30)
                } else {
                    Duration::from_millis(300)
                },
                own,
            ));
            let parent = pid(&parent).await;
            let descendant = pid(&descendant).await;
            if cancel {
                token.cancel();
            }
            assert_eq!(
                operation.await.unwrap().unwrap_err(),
                if cancel {
                    Failure::Cancelled
                } else {
                    Failure::Deadline
                }
            );
            stopped(parent).await;
            stopped(descendant).await;
            reaped(parent);
        }
    }
    #[tokio::test]
    async fn successful_parent_exit_also_stops_pipe_holding_descendant() {
        let directory = tempfile::tempdir().unwrap();
        let child_path = directory.path().join("child");
        let mut command =
            shell("/bin/sleep 30 & printf '%s' \"$!\" > \"$1\"; printf complete; exit 0");
        command.arg(&child_path);
        assert_eq!(
            isolated(command, Duration::from_secs(3), CancellationToken::new())
                .await
                .unwrap(),
            b"complete"
        );
        stopped(pid(&child_path).await).await;
    }
    #[tokio::test]
    async fn aborting_caller_retains_resources_and_slot_until_direct_child_reaped() {
        struct Resource {
            directory: tempfile::TempDir,
            dropped: Arc<AtomicBool>,
        }
        impl Drop for Resource {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let resource = Resource {
            directory: tempfile::tempdir().unwrap(),
            dropped: dropped.clone(),
        };
        let path = resource.directory.path().to_path_buf();
        let parent_path = path.join("parent");
        let mut command = shell("printf '%s' \"$$\" > \"$1\"; /bin/sleep 30 & wait");
        command.arg(&parent_path);
        let admission = Arc::new(Semaphore::new(1));
        let shared = admission.clone();
        let (reply, receive) = oneshot::channel();
        drop(tokio::spawn(supervise(
            command,
            resource,
            Instant::now() + Duration::from_secs(30),
            CancellationToken::new(),
            reply,
            shared,
        )));
        let parent = pid(&parent_path).await;
        assert_eq!(admission.available_permits(), 0);
        assert!(path.exists());
        assert!(!dropped.load(Ordering::SeqCst));
        drop(receive);
        timeout(Duration::from_secs(3), async {
            loop {
                if dropped.load(Ordering::SeqCst) && admission.available_permits() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        stopped(parent).await;
        reaped(parent);
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn queued_cancellation_and_deadline_never_spawn_and_next_slot_progresses() {
        let admission = Arc::new(Semaphore::new(1));
        let held = admission.clone().acquire_owned().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let started = directory.path().join("started");
        for cancel in [false, true] {
            let mut command = shell("printf started > \"$1\"");
            command.arg(&started);
            let token = CancellationToken::new();
            if cancel {
                token.cancel();
            }
            let (reply, receive) = oneshot::channel();
            drop(tokio::spawn(supervise(
                command,
                (),
                Instant::now() + Duration::from_millis(50),
                token,
                reply,
                admission.clone(),
            )));
            assert_eq!(
                receive.await.unwrap().unwrap_err(),
                if cancel {
                    Failure::Cancelled
                } else {
                    Failure::AdmissionDeadline
                }
            );
            assert!(!started.exists());
            assert_eq!(admission.available_permits(), 0);
        }
        drop(held);
        let (reply, receive) = oneshot::channel();
        drop(tokio::spawn(supervise(
            shell("printf next"),
            (),
            Instant::now() + Duration::from_secs(3),
            CancellationToken::new(),
            reply,
            admission.clone(),
        )));
        assert_eq!(receive.await.unwrap().unwrap(), b"next");
        let _permit = timeout(Duration::from_secs(1), admission.acquire())
            .await
            .unwrap()
            .unwrap();
    }
}
