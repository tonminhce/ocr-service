//! Bounded persistent process pool for Rust ONNX layout inference.
//!
//! Each actor owns one helper and serves one page at a time. A dropped caller,
//! cancellation, deadline, protocol error or helper exit kills and reaps that
//! process before its slot is made available again. The next page starts fresh.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use image::{ImageFormat, RgbImage};
use ocr_layout::{
    PageLayout,
    protocol::{self, DetectRequest, Response},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{ProcessingError, settings::Settings};

#[derive(Clone)]
struct Config {
    binary: std::path::PathBuf,
    model: std::path::PathBuf,
    digest: String,
    max_pixels: u64,
    timeout: Duration,
    size: usize,
    runtime_library: Option<std::path::PathBuf>,
}

/// Independent stage durations; a warm request reports zero startup seconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct LayoutTimings {
    pub queue_seconds: f64,
    pub startup_seconds: f64,
    pub encoding_seconds: f64,
    pub inference_seconds: f64,
}

type Detection = Result<(PageLayout, LayoutTimings), ProcessingError>;

/// Clones share the same lazy, fixed-size pool. Construction performs no I/O and
/// needs no Tokio runtime; actors start on the first asynchronous detect call.
#[derive(Clone)]
pub struct LayoutPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    config: Config,
    runtime: OnceLock<PoolRuntime>,
    stopped: CancellationToken,
}

struct PoolRuntime {
    senders: Vec<mpsc::Sender<Request>>,
    idle: Arc<Mutex<VecDeque<usize>>>,
    available: Arc<Semaphore>,
    stopped: CancellationToken,
    handles: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for PoolRuntime {
    fn drop(&mut self) {
        self.stopped.cancel();
        self.available.close();
    }
}

struct Request {
    page: Arc<RgbImage>,
    cpu_slots: Arc<Semaphore>,
    cancelled: CancellationToken,
    reply: oneshot::Sender<Detection>,
    permit: OwnedSemaphorePermit,
    queue_seconds: f64,
}

impl LayoutPool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                config: Config {
                    binary: settings.layout_binary.clone(),
                    model: settings.layout_model_path.clone(),
                    digest: settings.layout_model_sha256.to_ascii_lowercase(),
                    max_pixels: settings.max_image_pixels,
                    timeout: Duration::from_secs(settings.layout_timeout_seconds),
                    size: settings.layout_pool_size,
                    runtime_library: std::env::var_os("ORT_DYLIB_PATH").map(Into::into),
                },
                runtime: OnceLock::new(),
                stopped: CancellationToken::new(),
            }),
        }
    }

    pub async fn detect(
        &self,
        page: Arc<RgbImage>,
        cpu_slots: Arc<Semaphore>,
        cancelled: CancellationToken,
    ) -> Result<PageLayout, ProcessingError> {
        self.detect_with_timings(page, cpu_slots, cancelled)
            .await
            .map(|(layout, _)| layout)
    }

    pub async fn detect_with_timings(
        &self,
        page: Arc<RgbImage>,
        cpu_slots: Arc<Semaphore>,
        cancelled: CancellationToken,
    ) -> Detection {
        if page.width() == 0
            || page.height() == 0
            || page.width() > 8192
            || page.height() > 8192
            || u64::from(page.width()) * u64::from(page.height()) > self.inner.config.max_pixels
        {
            return Err(ProcessingError::permanent(
                "layout page exceeds dimension/pixel bound",
            ));
        }
        if self.inner.stopped.is_cancelled() {
            return Err(ProcessingError::transient("layout is stopping"));
        }
        let runtime = self.inner.runtime.get_or_init(|| self.start());
        let queued = Instant::now();
        let permit = tokio::select! {
            biased;
            _ = cancelled.cancelled() => return Err(ProcessingError::transient("layout cancelled")),
            _ = runtime.stopped.cancelled() => return Err(ProcessingError::transient("layout is stopping")),
            permit = runtime.available.clone().acquire_owned() =>
                permit.map_err(|_| ProcessingError::transient("layout is stopping"))?,
        };
        let index = runtime
            .idle
            .lock()
            .expect("layout idle lock poisoned")
            .pop_front()
            .expect("available layout slot must be idle");
        let (reply, response) = oneshot::channel();
        let request = Request {
            page,
            cpu_slots,
            cancelled,
            reply,
            permit,
            queue_seconds: queued.elapsed().as_secs_f64(),
        };
        // No await between reserving an index and handing ownership to its
        // actor: cancellation can never leak a slot or release an active one.
        if let Err(error) = runtime.senders[index].try_send(request) {
            runtime
                .idle
                .lock()
                .expect("layout idle lock poisoned")
                .push_back(index);
            drop(error.into_inner());
            return Err(ProcessingError::transient("layout helper actor stopped"));
        }
        response
            .await
            .map_err(|_| ProcessingError::transient("layout helper actor stopped"))?
    }

    fn start(&self) -> PoolRuntime {
        let idle = Arc::new(Mutex::new((0..self.inner.config.size).collect()));
        let stopped = self.inner.stopped.clone();
        let mut senders = Vec::new();
        let mut handles = Vec::new();
        for index in 0..self.inner.config.size {
            let (sender, receiver) = mpsc::channel(1);
            senders.push(sender);
            handles.push(tokio::spawn(actor(
                index,
                self.inner.config.clone(),
                receiver,
                idle.clone(),
                stopped.clone(),
            )));
        }
        PoolRuntime {
            senders,
            idle,
            available: Arc::new(Semaphore::new(self.inner.config.size)),
            stopped,
            handles: tokio::sync::Mutex::new(handles),
        }
    }

    /// Graceful process cleanup, useful before terminating the Tokio runtime.
    /// Once shut down, a pool is deliberately not restartable.
    pub async fn shutdown(&self) {
        self.inner.stopped.cancel();
        let Some(runtime) = self.inner.runtime.get() else {
            return;
        };
        runtime.stopped.cancel();
        runtime.available.close();
        // Concurrent shutdown callers must all wait until cleanup finishes.
        let mut handles = runtime.handles.lock().await;
        while let Some(handle) = handles.last_mut() {
            // Keep the handle in shared state across await. If this caller is
            // aborted, a later shutdown still observes the cleanup barrier.
            let _ = handle.await;
            handles.pop();
        }
    }
}

/// Compatibility entry point for callers that do not retain a recognizer. The
/// worker should retain `LayoutPool` across documents to avoid model reloads.
pub async fn detect(
    settings: &Settings,
    page: Arc<RgbImage>,
    slots: Arc<Semaphore>,
    cancelled: CancellationToken,
) -> Result<PageLayout, ProcessingError> {
    let pool = LayoutPool::new(settings);
    let result = pool.detect(page, slots, cancelled).await;
    pool.shutdown().await;
    result
}

async fn actor(
    index: usize,
    config: Config,
    mut incoming: mpsc::Receiver<Request>,
    idle: Arc<Mutex<VecDeque<usize>>>,
    stopped: CancellationToken,
) {
    let mut helper: Option<Helper> = None;
    loop {
        let request = if let Some(process) = helper.as_mut() {
            tokio::select! {
                biased;
                _ = stopped.cancelled() => None,
                _ = process.exited() => {
                    helper.take().expect("helper exists").shutdown().await;
                    continue;
                },
                request = incoming.recv() => request,
            }
        } else {
            tokio::select! {
                biased;
                _ = stopped.cancelled() => None,
                request = incoming.recv() => request,
            }
        };
        let Some(request) = request else {
            break;
        };
        let Request {
            page,
            cpu_slots,
            cancelled,
            mut reply,
            permit,
            queue_seconds,
        } = request;
        let work = async {
            if helper.is_none() {
                helper = Some(Helper::spawn(&config)?);
            }
            let mut detection = helper
                .as_mut()
                .expect("helper started")
                .detect(page, cpu_slots, &config)
                .await?;
            detection.1.queue_seconds = queue_seconds;
            Ok(detection)
        };
        let result = tokio::select! {
            biased;
            _ = stopped.cancelled() => Err(ProcessingError::transient("layout is stopping")),
            _ = cancelled.cancelled() => Err(ProcessingError::transient("layout cancelled")),
            _ = reply.closed() => Err(ProcessingError::transient("layout caller cancelled")),
            result = tokio::time::timeout(config.timeout, work) => match result {
                Ok(result) => result,
                Err(_) => Err(ProcessingError::transient("layout detector timed out")),
            },
        };
        if result.is_err()
            && let Some(process) = helper.take()
        {
            process.shutdown().await;
        }
        // Publish availability only after killed processes have been reaped.
        let _ = reply.send(result);
        idle.lock()
            .expect("layout idle lock poisoned")
            .push_back(index);
        drop(permit);
    }
    if let Some(process) = helper {
        process.shutdown().await;
    }
    // Close and reject already queued work, releasing its permits and callers.
    incoming.close();
}

struct Helper {
    // Fields drop in declaration order: signal the group before dropping its
    // direct-child handle and private scratch on abrupt runtime teardown.
    group: ProcessGroup,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Option<JoinHandle<std::io::Result<()>>>,
    directory: Arc<tempfile::TempDir>,
    ready: bool,
    started: Instant,
    next_id: u64,
}

/// Abrupt actor/runtime drop still signals the original Unix process group.
/// Normal shutdown below retains scratch and the actor slot until direct reap.
struct ProcessGroup(i32);
impl ProcessGroup {
    fn kill(&mut self) -> bool {
        if self.0 == 0 {
            return true;
        }
        #[cfg(unix)]
        {
            if crate::child_process::signal_group(self.0) {
                self.0 = 0;
                true
            } else {
                false
            }
        }
        #[cfg(not(unix))]
        false
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Helper {
    fn spawn(config: &Config) -> Result<Self, ProcessingError> {
        let directory = crate::subprocess_policy::scratch_directory()?;
        let started = Instant::now();
        let binary = crate::subprocess_policy::layout_binary(&config.binary)?;
        // Changing child cwd must not reinterpret repository-relative native
        // development model paths. This is trusted operator configuration.
        let model = std::path::absolute(&config.model)
            .map_err(|_| ProcessingError::permanent("cannot resolve layout model path"))?;
        let mut command = Command::new(binary);
        crate::subprocess_policy::layout_environment(
            &mut command,
            directory.path(),
            config.runtime_library.as_deref(),
        )?;
        command
            .arg("--serve-directory")
            .arg(directory.path())
            .arg("--model")
            .arg(&model)
            .arg("--sha256")
            .arg(&config.digest)
            .arg("--max-pixels")
            .arg(config.max_pixels.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(not(unix))]
        return Err(ProcessingError::permanent(
            "layout process supervision requires Unix",
        ));
        let mut child = command
            .spawn()
            .map_err(|_| ProcessingError::transient("cannot start the Rust layout detector"))?;
        let group = ProcessGroup(
            child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .ok_or_else(|| ProcessingError::transient("cannot supervise layout detector"))?,
        );
        let stdin = child.stdin.take().expect("piped detector stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped detector stdout"));
        let stderr = child.stderr.take().expect("piped detector stderr");
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr: Some(tokio::spawn(drain_stderr(stderr))),
            directory,
            ready: false,
            started,
            next_id: 1,
            group,
        })
    }

    async fn exited(&mut self) {
        tokio::select! {
            _ = crate::child_process::observe_exit(self.group.0 as u32) => (),
            _ = self.stderr.as_mut().expect("stderr monitor exists") => { self.stderr.take(); },
        }
    }

    async fn read_response(&mut self) -> Result<Response, ProcessingError> {
        let frame = tokio::select! {
            result = read_frame(&mut self.stdout, protocol::MAX_RESPONSE_BYTES) =>
                result.map_err(|_| ProcessingError::transient("layout detector output truncated or exceeded its bound"))?,
            _ = crate::child_process::observe_exit(self.group.0 as u32) => return Err(ProcessingError::transient("layout detector exited unexpectedly")),
            _ = self.stderr.as_mut().expect("stderr monitor exists") => {
                self.stderr.take();
                return Err(ProcessingError::transient("layout detector diagnostics closed or exceeded their bound"));
            },
        };
        serde_json::from_slice(&frame)
            .map_err(|_| ProcessingError::permanent("layout detector returned malformed JSON"))
    }

    async fn detect(
        &mut self,
        page: Arc<RgbImage>,
        slots: Arc<Semaphore>,
        config: &Config,
    ) -> Detection {
        let mut timings = LayoutTimings::default();
        if !self.ready {
            match self.read_response().await? {
                Response::Ready {
                    version,
                    model_sha256,
                } if version == protocol::VERSION && model_sha256 == config.digest => (),
                _ => {
                    return Err(ProcessingError::permanent(
                        "layout detector returned an invalid ready handshake",
                    ));
                }
            }
            self.ready = true;
            timings.startup_seconds = self.started.elapsed().as_secs_f64();
        }
        let encode_started = Instant::now();
        let permit = slots
            .acquire_owned()
            .await
            .map_err(|_| ProcessingError::transient("layout encoding is stopping"))?;
        let directory = self.directory.clone();
        let raster = page.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            raster
                .save_with_format(directory.path().join("page.png"), ImageFormat::Png)
                .map_err(|_| ProcessingError::transient("cannot encode layout page"))
        })
        .await
        .map_err(|_| ProcessingError::transient("layout encoding task failed"))??;
        timings.encoding_seconds = encode_started.elapsed().as_secs_f64();
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| ProcessingError::transient("layout request id exhausted"))?;
        let request = DetectRequest {
            version: protocol::VERSION,
            id,
            image: "page.png".into(),
            width: page.width(),
            height: page.height(),
        };
        let mut frame = serde_json::to_vec(&request)
            .map_err(|_| ProcessingError::permanent("cannot serialize layout request"))?;
        frame.push(b'\n');
        let inference_started = Instant::now();
        self.stdin
            .write_all(&frame)
            .await
            .map_err(|_| ProcessingError::transient("layout detector input closed"))?;
        self.stdin
            .flush()
            .await
            .map_err(|_| ProcessingError::transient("layout detector input closed"))?;
        let layout = match self.read_response().await? {
            Response::Layout {
                id: received,
                layout,
            } if received == id => layout,
            Response::Error { id: received, code } if received == id => {
                return Err(if code == "invalid_image" || code == "output_limit" {
                    ProcessingError::permanent("layout detector rejected bounded image/output")
                } else {
                    ProcessingError::transient("layout detector inference failed")
                });
            }
            _ => {
                return Err(ProcessingError::permanent(
                    "layout detector returned an invalid response id/type",
                ));
            }
        };
        timings.inference_seconds = inference_started.elapsed().as_secs_f64();
        validate(&layout, &page)?;
        Ok((layout, timings))
    }

    async fn shutdown(mut self) {
        let group_stopped = self.group.kill();
        let _ = self.child.start_kill();
        if !group_stopped {
            // Do not reuse an actor whose descendants could still be active.
            // A container supervisor/restart is required for this rare failure.
            tracing::error!("layout process group cleanup failed; actor remains unavailable");
            std::future::pending::<()>().await;
        }
        let reaped = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        if !matches!(reaped, Ok(Ok(_))) {
            tracing::error!("layout direct-child reap pending; actor remains unavailable");
            // SIGKILL cannot interrupt kernel D-state. Keep scratch/slot until
            // reap rather than spawning another helper after a timeout.
            if self.child.wait().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        if let Some(stderr) = self.stderr.take() {
            stderr.abort();
            let _ = stderr.await;
        }
    }
}

async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    bound: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial frame",
            ));
        }
        let length = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        if length > bound.saturating_sub(output.len()) {
            return Err(std::io::Error::other("frame exceeds byte bound"));
        }
        let complete = bytes[length - 1] == b'\n';
        output.extend_from_slice(&bytes[..length]);
        reader.consume(length);
        if complete {
            return Ok(output);
        }
    }
}

async fn drain_stderr(mut reader: impl AsyncRead + Unpin) -> std::io::Result<()> {
    let mut total = 0_usize;
    let mut chunk = [0_u8; 8192];
    loop {
        let size = reader.read(&mut chunk).await?;
        if size == 0 {
            return Ok(());
        }
        total += size;
        if total > 64 * 1024 {
            return Err(std::io::Error::other("diagnostics exceed byte bound"));
        }
    }
}

fn validate(result: &PageLayout, page: &RgbImage) -> Result<(), ProcessingError> {
    if result.width != page.width()
        || result.height != page.height()
        || result.regions.len() + result.discarded.len() > 300
    {
        return Err(ProcessingError::permanent(
            "layout detector returned invalid page dimensions or region count",
        ));
    }
    // SDK indices are local to each collection: abandoned regions start at
    // zero independently from the regions eligible for formatting.
    for (regions, abandoned) in [(&result.regions, false), (&result.discarded, true)] {
        let mut indices = std::collections::HashSet::new();
        for region in regions {
            if ocr_layout::postprocess::LABELS
                .get(region.class_id as usize)
                .copied()
                != Some(region.label.as_str())
                || region.task != ocr_layout::postprocess::task_for_class(region.class_id as usize)
                || (region.task == ocr_layout::Task::Abandon) != abandoned
            {
                return Err(ProcessingError::permanent(
                    "layout detector returned an invalid class/task mapping",
                ));
            }
            if !region.score.is_finite()
                || !(0.0..=1.0).contains(&region.score)
                || !region.bbox.iter().all(|v| v.is_finite())
                || region.bbox[2] <= region.bbox[0]
                || region.bbox[3] <= region.bbox[1]
                || region.bbox[2] <= 0.0
                || region.bbox[3] <= 0.0
                || region.bbox[0] >= page.width() as f32
                || region.bbox[1] >= page.height() as f32
                || region.bbox_2d.iter().any(|v| !(0..=1000).contains(v))
                || region.bbox_2d[2] <= region.bbox_2d[0]
                || region.bbox_2d[3] <= region.bbox_2d[1]
                || region.reading_order >= 300
                || region.index >= 300
                || !indices.insert(region.index)
                || region.polygon.len() < 3
                || region.polygon.len() > 4096
                || region
                    .polygon
                    .iter()
                    .flatten()
                    .any(|v| !(0..=1000).contains(v))
            {
                return Err(ProcessingError::permanent(
                    "layout detector returned invalid region geometry",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    struct Fixture {
        directory: tempfile::TempDir,
        binary: std::path::PathBuf,
    }

    impl Fixture {
        fn new(mode: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let binary = directory.path().join("fake-layout");
            let starts = quote(&directory.path().join("starts"));
            let calls = quote(&directory.path().join("calls"));
            let once = quote(&directory.path().join("once"));
            let environment = quote(&directory.path().join("environment"));
            let descendant = quote(&directory.path().join("descendant"));
            let script = format!(
                r##"#!/bin/sh
printf '%s\n' "$$" >> {starts}
first=false
if [ ! -e {once} ]; then first=true; : > {once}; fi
if [ "{mode}" = 'environment' ]; then
  /usr/bin/env -u PWD -u SHLVL -u _ > {environment}
fi
printf '%s\n' '{{"type":"ready","version":1,"model_sha256":"{digest}"}}'
while IFS= read -r request; do
  printf '%s\n' 'request' >> {calls}
  if [ "{mode}" = 'hang_descendant_once' ] && [ "$first" = true ]; then
    /bin/sleep 30 </dev/null >/dev/null 2>&1 &
    printf '%s\n' "$!" > {descendant}
    IFS= read -r unused
    wait
    exit 0
  fi
  if [ "{mode}" = 'hang_once' ] && [ "$first" = true ]; then
    IFS= read -r unused
    exit 0
  fi
  if [ "{mode}" = 'malformed_once' ] && [ "$first" = true ]; then
    printf 'not-json\n'
    IFS= read -r unused
    exit 0
  fi
  if [ "{mode}" = 'wrong_id_once' ] && [ "$first" = true ]; then
    printf '%s\n' '{{"type":"layout","id":999,"layout":{{"width":64,"height":96,"regions":[],"discarded":[]}}}}'
    IFS= read -r unused
    exit 0
  fi
  if [ "{mode}" = 'eof_once' ] && [ "$first" = true ]; then
    printf 'partial-frame'
    exit 0
  fi
  if [ "{mode}" = 'flood_once' ] && [ "$first" = true ]; then
    count=0
    while [ "$count" -lt 8193 ]; do
      printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
      count=$((count + 1))
    done
    IFS= read -r unused
    exit 0
  fi
  id=$(printf '%s' "$request" | /usr/bin/sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{{"type":"layout","id":%s,"layout":{{"width":64,"height":96,"regions":[],"discarded":[]}}}}\n' "$id"
done
"##,
                digest = "0".repeat(64)
            );
            std::fs::write(&binary, script).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self { directory, binary }
        }

        fn pool(&self, size: usize, timeout: Duration) -> LayoutPool {
            LayoutPool {
                inner: Arc::new(PoolInner {
                    config: Config {
                        binary: self.binary.clone(),
                        model: "unused-test-model".into(),
                        digest: "0".repeat(64),
                        max_pixels: 1_000_000,
                        timeout,
                        size,
                        runtime_library: None,
                    },
                    runtime: OnceLock::new(),
                    stopped: CancellationToken::new(),
                }),
            }
        }

        fn pids(&self) -> Vec<u32> {
            std::fs::read_to_string(self.directory.path().join("starts"))
                .unwrap_or_default()
                .lines()
                .map(|value| value.parse().unwrap())
                .collect()
        }

        fn calls(&self) -> usize {
            std::fs::read_to_string(self.directory.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .count()
        }

        fn descendant(&self) -> Option<u32> {
            std::fs::read_to_string(self.directory.path().join("descendant"))
                .ok()
                .and_then(|value| value.trim().parse().ok())
        }
    }

    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
    }

    fn page() -> Arc<RgbImage> {
        Arc::new(RgbImage::new(64, 96))
    }

    fn slots() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(2))
    }

    fn alive(pid: u32) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn running(pid: u32) -> bool {
        let output = std::process::Command::new("/bin/ps")
            .env_clear()
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        !state.trim().is_empty() && !state.trim().starts_with('Z')
    }

    fn assert_reaped(pid: u32) {
        let pid = i32::try_from(pid).unwrap();
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

    async fn eventually(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition never became true");
    }

    #[tokio::test]
    async fn warm_requests_reuse_process_and_shutdown_reaps_it() {
        let fixture = Fixture::new("normal");
        let pool = fixture.pool(1, Duration::from_secs(3));
        let (_, first) = pool
            .detect_with_timings(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        let (_, warm) = pool
            .detect_with_timings(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        assert!(first.startup_seconds > 0.0);
        assert_eq!(warm.startup_seconds, 0.0);
        assert_eq!(fixture.pids().len(), 1);
        let pid = fixture.pids()[0];
        assert!(alive(pid));
        pool.shutdown().await;
        assert!(!alive(pid));
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn actual_layout_child_receives_only_clean_environment_and_optional_runtime() {
        for with_runtime in [false, true] {
            let fixture = Fixture::new("environment");
            let mut pool = fixture.pool(1, Duration::from_secs(3));
            let runtime = fixture.directory.path().join("synthetic-runtime-library");
            if with_runtime {
                std::fs::write(&runtime, b"fixture; never loaded by the shell helper").unwrap();
                Arc::get_mut(&mut pool.inner)
                    .unwrap()
                    .config
                    .runtime_library = Some(runtime.clone());
            }
            pool.detect(page(), slots(), CancellationToken::new())
                .await
                .unwrap();
            // A shell may synthesize PWD/SHLVL/_ independently of the worker.
            // The fixture removes only those three before capturing /usr/bin/env;
            // direct command-policy tests separately assert the pre-shell env.
            let snapshot =
                std::fs::read_to_string(fixture.directory.path().join("environment")).unwrap();
            let environment: BTreeMap<_, _> = snapshot
                .lines()
                .map(|line| line.split_once('=').expect("one environment assignment"))
                .collect();
            assert_eq!(environment.len(), if with_runtime { 7 } else { 6 });
            for (key, value) in [
                ("PATH", "/usr/bin:/bin"),
                ("LC_ALL", "C"),
                ("LANG", "C"),
                ("TZ", "UTC"),
                ("HOME", "/nonexistent"),
            ] {
                assert_eq!(environment.get(key), Some(&value));
            }
            let scratch = Path::new(environment["TMPDIR"]);
            assert!(scratch.is_absolute() && scratch.is_dir());
            assert_eq!(
                environment.get("ORT_DYLIB_PATH").copied(),
                with_runtime.then(|| runtime.to_str().unwrap())
            );
            assert!(environment.keys().all(|key| {
                !key.starts_with("DATABASE")
                    && !key.starts_with("REDIS")
                    && !key.starts_with("VLLM")
                    && !key.starts_with("LD_")
                    && !key.starts_with("DYLD_")
                    && !key.ends_with("_FILE")
                    && !key.to_ascii_uppercase().contains("PROXY")
            }));
            pool.shutdown().await;
            assert!(!scratch.exists());
        }
    }

    #[tokio::test]
    async fn cancelled_or_dropped_caller_reaps_group_and_next_call_restarts() {
        for abort in [false, true] {
            let fixture = Fixture::new("hang_descendant_once");
            let pool = fixture.pool(1, Duration::from_secs(30));
            let cancellation = CancellationToken::new();
            let task = {
                let pool = pool.clone();
                let token = cancellation.clone();
                tokio::spawn(async move { pool.detect(page(), slots(), token).await })
            };
            eventually(|| fixture.descendant().is_some()).await;
            let parent = fixture.pids()[0];
            let descendant = fixture.descendant().unwrap();
            assert!(alive(parent) && running(descendant));
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                cancellation.cancel();
                let error = task.await.unwrap().unwrap_err();
                assert!(error.retryable && error.message.contains("cancelled"));
                assert!(!alive(parent), "reply must follow direct-child reaping");
            }
            eventually(|| !alive(parent) && !running(descendant)).await;
            assert_reaped(parent);
            pool.detect(page(), slots(), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(fixture.pids().len(), 2);
            assert_eq!(fixture.calls(), 2);
            pool.shutdown().await;
            assert!(fixture.pids().iter().all(|pid| !alive(*pid)));
        }
    }

    #[tokio::test]
    async fn aborting_shutdown_preserves_outstanding_cleanup_handles() {
        let fixture = Fixture::new("normal");
        let pool = fixture.pool(1, Duration::from_secs(3));
        let runtime = pool.inner.runtime.get_or_init(|| pool.start());
        let (release, waiting) = oneshot::channel::<()>();
        // Model a delayed cleanup task deterministically; separate tests below
        // exercise actual group signalling and direct-child reaping.
        runtime.handles.lock().await.push(tokio::spawn(async move {
            let _ = waiting.await;
        }));
        let first = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.shutdown().await })
        };
        eventually(|| runtime.available.is_closed()).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(runtime.handles.lock().await.len(), 2);
        let mut second = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.shutdown().await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut second)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), second)
            .await
            .unwrap()
            .unwrap();
        assert!(runtime.handles.lock().await.is_empty());
        assert!(fixture.pids().is_empty());
    }

    #[tokio::test]
    async fn fixed_pool_never_launches_more_than_its_bound() {
        let fixture = Fixture::new("normal");
        let pool = fixture.pool(2, Duration::from_secs(3));
        let mut work = Vec::new();
        for _ in 0..12 {
            let pool = pool.clone();
            work.push(tokio::spawn(async move {
                pool.detect(page(), slots(), CancellationToken::new())
                    .await
                    .unwrap()
            }));
        }
        for task in work {
            task.await.unwrap();
        }
        assert_eq!(fixture.pids().len(), 2);
        pool.shutdown().await;
        assert!(fixture.pids().iter().all(|pid| !alive(*pid)));
    }

    #[tokio::test]
    async fn cancellation_reaps_before_reply_and_the_next_request_restarts() {
        let fixture = Fixture::new("hang_once");
        let pool = fixture.pool(1, Duration::from_secs(3));
        let cancellation = CancellationToken::new();
        let task = {
            let pool = pool.clone();
            let token = cancellation.clone();
            tokio::spawn(async move { pool.detect(page(), slots(), token).await })
        };
        eventually(|| fixture.calls() > 0).await;
        let pid = fixture.pids()[0];
        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.retryable && error.message.contains("cancelled"));
        assert!(!alive(pid));
        pool.detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fixture.pids().len(), 2);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn aborting_the_caller_kills_process_and_returns_slot_after_reap() {
        let fixture = Fixture::new("hang_once");
        let pool = fixture.pool(1, Duration::from_secs(3));
        let task = {
            let pool = pool.clone();
            tokio::spawn(
                async move { pool.detect(page(), slots(), CancellationToken::new()).await },
            )
        };
        eventually(|| fixture.calls() > 0).await;
        let pid = fixture.pids()[0];
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        eventually(|| !alive(pid)).await;
        pool.detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fixture.pids().len(), 2);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn queued_cancellation_does_not_leak_a_slot_or_start_a_process() {
        let fixture = Fixture::new("hang_once");
        let pool = fixture.pool(1, Duration::from_secs(3));
        let active_token = CancellationToken::new();
        let active = {
            let pool = pool.clone();
            let token = active_token.clone();
            tokio::spawn(async move { pool.detect(page(), slots(), token).await })
        };
        eventually(|| fixture.calls() > 0).await;
        let queued_token = CancellationToken::new();
        let queued = {
            let pool = pool.clone();
            let token = queued_token.clone();
            tokio::spawn(async move { pool.detect(page(), slots(), token).await })
        };
        tokio::task::yield_now().await;
        queued_token.cancel();
        assert!(
            queued
                .await
                .unwrap()
                .unwrap_err()
                .message
                .contains("cancelled")
        );
        assert_eq!(fixture.pids().len(), 1);
        active_token.cancel();
        active.await.unwrap().unwrap_err();
        pool.detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn deadline_reaps_and_allows_a_fresh_helper() {
        let fixture = Fixture::new("hang_once");
        let pool = fixture.pool(1, Duration::from_millis(200));
        let error = pool
            .detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.retryable && error.message.contains("timed out"));
        assert!(!alive(fixture.pids()[0]));
        pool.detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_oversized_and_wrong_id_outputs_retire_the_process() {
        for mode in ["malformed_once", "flood_once", "wrong_id_once", "eof_once"] {
            let fixture = Fixture::new(mode);
            let pool = fixture.pool(1, Duration::from_secs(3));
            pool.detect(page(), slots(), CancellationToken::new())
                .await
                .unwrap_err();
            assert!(!alive(fixture.pids()[0]), "{mode}");
            pool.detect(page(), slots(), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(fixture.pids().len(), 2);
            pool.shutdown().await;
        }
    }

    #[tokio::test]
    async fn dropping_the_final_pool_owner_reaps_an_idle_helper() {
        let fixture = Fixture::new("normal");
        let pool = fixture.pool(1, Duration::from_secs(3));
        pool.detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap();
        let pid = fixture.pids()[0];
        drop(pool);
        eventually(|| !alive(pid)).await;
    }

    #[tokio::test]
    async fn shutting_down_before_first_use_does_not_start_a_helper() {
        let fixture = Fixture::new("normal");
        let pool = fixture.pool(1, Duration::from_secs(3));
        pool.shutdown().await;
        let error = pool
            .detect(page(), slots(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.message.contains("stopping"));
        assert!(fixture.pids().is_empty());
    }
}
