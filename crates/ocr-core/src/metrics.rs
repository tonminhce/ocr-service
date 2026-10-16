//! Bounded observations and an opt-in Prometheus endpoint. All dimensions are
//! enums; document IDs, text, filenames, credentials and raw paths cannot become
//! labels. Tests can construct independent registries without global recorders.
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::{
    fmt::Write as _,
    future::{Future, IntoFuture},
    net::SocketAddr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

macro_rules! dimension {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const COUNT: usize = Self::ALL.len();
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}
dimension!(Stage {
    QueueWait => "queue_wait", Document => "document", Inspect => "inspect",
    Render => "render", Layout => "layout", LayoutQueue => "layout_queue",
    LayoutStartup => "layout_startup", LayoutEncode => "layout_encode",
    LayoutInference => "layout_inference", Crop => "crop", ModelQueue => "model_queue",
    Model => "model", Format => "format", Postprocess => "postprocess",
    NativeText => "native_text", Extraction => "extraction", Validation => "validation",
    Persistence => "persistence",
});
dimension!(Activity { Job => "job", Model => "model", Layout => "layout", Postprocess => "postprocess" });
dimension!(JobOutcome {
    Succeeded => "succeeded", Failed => "failed", RetryScheduled => "retry_scheduled",
    LeaseLost => "lease_lost", CommitFailed => "commit_failed", CommitStale => "commit_stale", Aborted => "aborted",
});
dimension!(Event {
    JobRetry => "job_retry", JobDeadline => "job_deadline", LeaseRenewalFailed => "lease_renewal_failed",
    RedisReadFailed => "redis_read_failed", NotificationMalformed => "notification_malformed",
    NotificationPublished => "notification_published", AcknowledgementFailed => "acknowledgement_failed",
    OutboxFailed => "outbox_failed", TaskPanicked => "task_panicked", RegionCacheHit => "region_cache_hit",
    RegionCacheMiss => "region_cache_miss", RecoveryAttempted => "recovery_attempted",
    ModelFailed => "model_failed",
    ModelAttemptStarted => "model_attempt_started", ModelCancelled => "model_cancelled",
    ParserUnavailable => "parser_unavailable",
});
dimension!(ModelFinish {
    Stop => "stop", Length => "length", ContentFilter => "content_filter",
    ToolCalls => "tool_calls", FunctionCall => "function_call", Other => "other", Missing => "missing",
});
dimension!(ModelUsage { Present => "present", Missing => "missing", Invalid => "invalid" });
dimension!(TokenKind { Prompt => "prompt", Completion => "completion", Total => "total" });
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperationalSnapshot {
    pub queued_jobs: u64,
    pub processing_jobs: u64,
    pub failed_jobs_24h: u64,
    pub eligible_jobs: u64,
    pub delayed_jobs: u64,
    pub oldest_queued_seconds: u64,
    pub seconds_since_last_progress: u64,
    pub due_outbox_jobs: u64,
    pub oldest_due_outbox_seconds: u64,
    pub expired_leases: u64,
    pub redis_pending_entries: u64,
    pub redis_memory_bytes: u64,
}
dimension!(HttpRoute {
    Submit => "submit", Job => "job", Status => "status", Business => "business",
    Artifact => "artifact", Review => "review", Health => "health", Other => "other",
});
impl HttpRoute {
    /// Only known router templates become labels. Unknown templates collapse to other.
    pub fn from_matched_path(path: Option<&str>) -> Self {
        match path {
            Some("/v1/ocr/jobs") => Self::Submit,
            Some("/v1/ocr/jobs/{id}") => Self::Job,
            Some("/v1/ocr/jobs/{id}/status") => Self::Status,
            Some("/v1/ocr/jobs/{id}/business") => Self::Business,
            Some("/v1/ocr/jobs/{id}/artifacts/{kind}") => Self::Artifact,
            Some("/v1/ocr/jobs/{id}/review") => Self::Review,
            Some("/health/live" | "/health/ready") => Self::Health,
            _ => Self::Other,
        }
    }
}
dimension!(HttpStatus { Informational => "1xx", Success => "2xx", Redirection => "3xx", ClientError => "4xx", ServerError => "5xx" });
impl HttpStatus {
    fn from_status(status: StatusCode) -> Self {
        match status.as_u16() / 100 {
            1 => Self::Informational,
            2 => Self::Success,
            3 => Self::Redirection,
            4 => Self::ClientError,
            _ => Self::ServerError,
        }
    }
}

const BUCKETS_US: [u64; 17] = [
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    30_000_000,
    60_000_000,
    120_000_000,
    300_000_000,
    600_000_000,
];
struct Histogram {
    buckets: [AtomicU64; BUCKETS_US.len() + 1],
    sum_us: AtomicU64,
}
impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
        }
    }
    fn observe(&self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        let bucket = BUCKETS_US.partition_point(|limit| *limit < micros);
        saturating_add(&self.sum_us, micros);
        saturating_add(&self.buckets[bucket], 1);
    }
    fn render(&self, output: &mut String, name: &str, labels: &str) {
        let mut count = 0_u64;
        for (index, bucket) in self.buckets.iter().enumerate() {
            count = count.saturating_add(bucket.load(Ordering::Relaxed));
            if let Some(limit) = BUCKETS_US.get(index) {
                let seconds = *limit as f64 / 1_000_000.0;
                let _ = writeln!(output, "{name}_bucket{{{labels},le=\"{seconds}\"}} {count}");
            } else {
                let _ = writeln!(output, "{name}_bucket{{{labels},le=\"+Inf\"}} {count}");
            }
        }
        let sum = self.sum_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let _ = writeln!(output, "{name}_sum{{{labels}}} {sum}");
        let _ = writeln!(output, "{name}_count{{{labels}}} {count}");
    }
}
fn saturating_add(value: &AtomicU64, amount: u64) {
    let mut old = value.load(Ordering::Relaxed);
    loop {
        match value.compare_exchange_weak(
            old,
            old.saturating_add(amount),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => old = actual,
        }
    }
}

pub struct Metrics {
    started: Instant,
    stages: [Histogram; Stage::COUNT],
    activities: [AtomicI64; Activity::COUNT],
    job_outcomes: [AtomicU64; JobOutcome::COUNT],
    events: [AtomicU64; Event::COUNT],
    http_durations: [Histogram; HttpRoute::COUNT],
    http_requests: [[AtomicU64; HttpStatus::COUNT]; HttpRoute::COUNT],
    model_finishes: [AtomicU64; ModelFinish::COUNT],
    model_usage: [AtomicU64; ModelUsage::COUNT],
    model_tokens: [AtomicU64; TokenKind::COUNT],
    model_token_observations: [AtomicU64; TokenKind::COUNT],
    operational: Mutex<OperationalMetrics>,
}

#[derive(Default)]
struct OperationalMetrics {
    snapshot: OperationalSnapshot,
    last_success_timestamp_seconds: u64,
    collector_error: bool,
    enabled: bool,
}
impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
impl Metrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            stages: std::array::from_fn(|_| Histogram::new()),
            activities: std::array::from_fn(|_| AtomicI64::new(0)),
            job_outcomes: std::array::from_fn(|_| AtomicU64::new(0)),
            events: std::array::from_fn(|_| AtomicU64::new(0)),
            http_durations: std::array::from_fn(|_| Histogram::new()),
            http_requests: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            model_finishes: std::array::from_fn(|_| AtomicU64::new(0)),
            model_usage: std::array::from_fn(|_| AtomicU64::new(0)),
            model_tokens: std::array::from_fn(|_| AtomicU64::new(0)),
            model_token_observations: std::array::from_fn(|_| AtomicU64::new(0)),
            operational: Mutex::new(OperationalMetrics::default()),
        }
    }
    pub fn observe(&self, stage: Stage, elapsed: Duration) {
        self.stages[stage as usize].observe(elapsed);
    }
    /// Includes cancellation/error paths. Nested spans are intentionally not additive.
    pub fn timer(&self, stage: Stage) -> StageTimer<'_> {
        StageTimer {
            metrics: self,
            stage,
            started: Instant::now(),
        }
    }
    pub fn active(&self, activity: Activity) -> ActivityGuard<'_> {
        self.activities[activity as usize].fetch_add(1, Ordering::Relaxed);
        ActivityGuard {
            metrics: self,
            activity,
        }
    }
    pub fn event(&self, event: Event) {
        saturating_add(&self.events[event as usize], 1);
    }
    /// Count only parsed server metadata, never estimates or cache replays.
    /// `None` has no token observation and therefore cannot mean zero tokens.
    pub fn model_metadata(&self, finish: ModelFinish, usage: ModelUsage, tokens: [Option<u64>; 3]) {
        saturating_add(&self.model_finishes[finish as usize], 1);
        saturating_add(&self.model_usage[usage as usize], 1);
        if usage == ModelUsage::Present {
            for (index, value) in tokens.into_iter().enumerate() {
                if let Some(value) = value {
                    saturating_add(&self.model_tokens[index], value);
                    saturating_add(&self.model_token_observations[index], 1);
                }
            }
        }
    }
    /// Dropping without finish records aborted, including panic/task cancellation.
    pub fn job_attempt(&self) -> JobAttempt<'_> {
        JobAttempt {
            metrics: self,
            _active: self.active(Activity::Job),
            _timer: self.timer(Stage::Document),
            finished: false,
        }
    }
    pub fn http_request(&self, route: HttpRoute, status: StatusCode, elapsed: Duration) {
        self.http_durations[route as usize].observe(elapsed);
        saturating_add(
            &self.http_requests[route as usize][HttpStatus::from_status(status) as usize],
            1,
        );
    }
    pub fn enable_operational_collector(&self) {
        self.operational
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = true;
    }
    pub fn operational_snapshot(&self, snapshot: OperationalSnapshot) {
        let mut operational = self
            .operational
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operational.snapshot = snapshot;
        operational.last_success_timestamp_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        operational.collector_error = false;
    }
    pub fn operational_collector_failed(&self) {
        self.operational
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .collector_error = true;
    }
    /// No database, network or model calls. All series exist even before first use.
    pub fn render(&self) -> String {
        let mut output = String::with_capacity(48_000);
        output.push_str("# HELP ocr_process_uptime_seconds Monotonic process uptime.\n# TYPE ocr_process_uptime_seconds gauge\n");
        let _ = writeln!(
            output,
            "ocr_process_uptime_seconds {}",
            self.started.elapsed().as_secs_f64()
        );
        output.push_str("# HELP ocr_stage_duration_seconds Stage wall time, including failed or cancelled stages.\n# TYPE ocr_stage_duration_seconds histogram\n");
        for stage in Stage::ALL {
            self.stages[*stage as usize].render(
                &mut output,
                "ocr_stage_duration_seconds",
                &format!("stage=\"{}\"", stage.as_str()),
            );
        }
        output.push_str("# HELP ocr_active Work currently in progress in this process.\n# TYPE ocr_active gauge\n");
        for activity in Activity::ALL {
            let _ = writeln!(
                output,
                "ocr_active{{activity=\"{}\"}} {}",
                activity.as_str(),
                self.activities[*activity as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_job_attempts_total Completed job attempts by durable outcome or interruption.\n# TYPE ocr_job_attempts_total counter\n");
        for outcome in JobOutcome::ALL {
            let _ = writeln!(
                output,
                "ocr_job_attempts_total{{outcome=\"{}\"}} {}",
                outcome.as_str(),
                self.job_outcomes[*outcome as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_events_total Fixed pipeline and transport events.\n# TYPE ocr_events_total counter\n");
        for event in Event::ALL {
            let _ = writeln!(
                output,
                "ocr_events_total{{event=\"{}\"}} {}",
                event.as_str(),
                self.events[*event as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_model_finishes_total Parsed server finish reasons; arbitrary values collapse to other.\n# TYPE ocr_model_finishes_total counter\n");
        for finish in ModelFinish::ALL {
            let _ = writeln!(
                output,
                "ocr_model_finishes_total{{reason=\"{}\"}} {}",
                finish.as_str(),
                self.model_finishes[*finish as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_model_usage_total Parsed server usage availability, including invalid usage.\n# TYPE ocr_model_usage_total counter\n");
        for usage in ModelUsage::ALL {
            let _ = writeln!(
                output,
                "ocr_model_usage_total{{state=\"{}\"}} {}",
                usage.as_str(),
                self.model_usage[*usage as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_model_tokens_total Genuine server-reported tokens only; absence is not zero.\n# TYPE ocr_model_tokens_total counter\n# HELP ocr_model_token_observations_total Responses actually reporting each token kind.\n# TYPE ocr_model_token_observations_total counter\n");
        for kind in TokenKind::ALL {
            let _ = writeln!(
                output,
                "ocr_model_tokens_total{{kind=\"{}\"}} {}",
                kind.as_str(),
                self.model_tokens[*kind as usize].load(Ordering::Relaxed)
            );
            let _ = writeln!(
                output,
                "ocr_model_token_observations_total{{kind=\"{}\"}} {}",
                kind.as_str(),
                self.model_token_observations[*kind as usize].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP ocr_http_request_duration_seconds HTTP time through response headers, excluding response body streaming.\n# TYPE ocr_http_request_duration_seconds histogram\n");
        for route in HttpRoute::ALL {
            self.http_durations[*route as usize].render(
                &mut output,
                "ocr_http_request_duration_seconds",
                &format!("route=\"{}\"", route.as_str()),
            );
        }
        output.push_str("# HELP ocr_http_requests_total HTTP responses by fixed route group and status class.\n# TYPE ocr_http_requests_total counter\n");
        for route in HttpRoute::ALL {
            for status in HttpStatus::ALL {
                let _ = writeln!(
                    output,
                    "ocr_http_requests_total{{route=\"{}\",status=\"{}\"}} {}",
                    route.as_str(),
                    status.as_str(),
                    self.http_requests[*route as usize][*status as usize].load(Ordering::Relaxed)
                );
            }
        }
        let operational = self
            .operational
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if operational.enabled {
            let snapshot = operational.snapshot;
            output.push_str("# HELP ocr_operational_jobs Durable PostgreSQL job counts from the last background snapshot.\n# TYPE ocr_operational_jobs gauge\n");
            for (state, count) in [
                ("queued", snapshot.queued_jobs),
                ("processing", snapshot.processing_jobs),
                ("failed_24h", snapshot.failed_jobs_24h),
            ] {
                let _ = writeln!(output, "ocr_operational_jobs{{state=\"{state}\"}} {count}");
            }
            output.push_str("# HELP ocr_operational_eligible_jobs Queued jobs currently eligible for processing.\n# TYPE ocr_operational_eligible_jobs gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_eligible_jobs {}",
                snapshot.eligible_jobs
            );
            output.push_str("# HELP ocr_operational_delayed_jobs Queued jobs waiting for their next eligible time.\n# TYPE ocr_operational_delayed_jobs gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_delayed_jobs {}",
                snapshot.delayed_jobs
            );
            output.push_str("# HELP ocr_operational_oldest_queued_seconds Age of the oldest queued job.\n# TYPE ocr_operational_oldest_queued_seconds gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_oldest_queued_seconds {}",
                snapshot.oldest_queued_seconds
            );
            output.push_str("# HELP ocr_operational_seconds_since_last_progress Age of the most recent active-job update.\n# TYPE ocr_operational_seconds_since_last_progress gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_seconds_since_last_progress {}",
                snapshot.seconds_since_last_progress
            );
            output.push_str("# HELP ocr_operational_due_outbox_jobs Durable outbox records currently due for dispatch.\n# TYPE ocr_operational_due_outbox_jobs gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_due_outbox_jobs {}",
                snapshot.due_outbox_jobs
            );
            output.push_str("# HELP ocr_operational_oldest_due_outbox_seconds Age of the oldest due outbox record.\n# TYPE ocr_operational_oldest_due_outbox_seconds gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_oldest_due_outbox_seconds {}",
                snapshot.oldest_due_outbox_seconds
            );
            output.push_str("# HELP ocr_operational_expired_leases PostgreSQL worker leases past their expiry time.\n# TYPE ocr_operational_expired_leases gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_expired_leases {}",
                snapshot.expired_leases
            );
            output.push_str("# HELP ocr_operational_redis_pending_entries Redis Stream pending-entry count.\n# TYPE ocr_operational_redis_pending_entries gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_redis_pending_entries {}",
                snapshot.redis_pending_entries
            );
            output.push_str("# HELP ocr_operational_redis_memory_bytes Redis-reported used memory.\n# TYPE ocr_operational_redis_memory_bytes gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_redis_memory_bytes {}",
                snapshot.redis_memory_bytes
            );
            output.push_str("# HELP ocr_operational_collector_last_success_timestamp_seconds Unix time of the last complete background snapshot.\n# TYPE ocr_operational_collector_last_success_timestamp_seconds gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_collector_last_success_timestamp_seconds {}",
                operational.last_success_timestamp_seconds
            );
            output.push_str("# HELP ocr_operational_collector_error Whether the latest background snapshot failed.\n# TYPE ocr_operational_collector_error gauge\n");
            let _ = writeln!(
                output,
                "ocr_operational_collector_error {}",
                u8::from(operational.collector_error)
            );
        }
        output
    }
}
pub fn process_metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}
pub struct StageTimer<'a> {
    metrics: &'a Metrics,
    stage: Stage,
    started: Instant,
}
impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        self.metrics.observe(self.stage, self.started.elapsed());
    }
}
pub struct ActivityGuard<'a> {
    metrics: &'a Metrics,
    activity: Activity,
}
impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.metrics.activities[self.activity as usize].fetch_sub(1, Ordering::Relaxed);
    }
}
pub struct JobAttempt<'a> {
    metrics: &'a Metrics,
    _active: ActivityGuard<'a>,
    _timer: StageTimer<'a>,
    finished: bool,
}
impl JobAttempt<'_> {
    pub fn finish(mut self, outcome: JobOutcome) {
        saturating_add(&self.metrics.job_outcomes[outcome as usize], 1);
        self.finished = true;
    }
}
impl Drop for JobAttempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            saturating_add(&self.metrics.job_outcomes[JobOutcome::Aborted as usize], 1);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsConfigError {
    #[error("metrics bind address must be a valid IP socket address")]
    Address,
    #[error("metrics bearer token must contain 32..4096 visible ASCII characters")]
    Token,
    #[error(transparent)]
    Secret(#[from] crate::secrets::SecretError),
    #[error("non-loopback metrics listeners require METRICS_BEARER_TOKEN")]
    PublicWithoutToken,
    #[error("metrics listener could not bind: {0}")]
    Bind(#[from] std::io::Error),
}
/// No Debug implementation: credentials must not enter logs or error messages.
pub struct MetricsEndpoint {
    listener: tokio::net::TcpListener,
    router: Router,
}
#[derive(Clone)]
struct EndpointState {
    metrics: &'static Metrics,
    token: Option<String>,
}
impl MetricsEndpoint {
    /// Disabled unless named bind variable is set. Unauthenticated listeners are
    /// permitted only on loopback; Docker network binds require a bearer token.
    pub async fn from_env(bind_variable: &str) -> Result<Option<Self>, MetricsConfigError> {
        let token = crate::secrets::optional("METRICS_BEARER_TOKEN")?;
        let address = match std::env::var(bind_variable) {
            Ok(value) if !value.trim().is_empty() => value,
            Ok(_) | Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(MetricsConfigError::Address),
        };
        let address = validate_endpoint_config(&address, token.as_deref())?;
        let listener = tokio::net::TcpListener::bind(address).await?;
        Ok(Some(Self::new(listener, process_metrics(), token)))
    }
    fn new(
        listener: tokio::net::TcpListener,
        metrics: &'static Metrics,
        token: Option<String>,
    ) -> Self {
        Self {
            listener,
            router: Router::new()
                .route("/metrics", get(scrape))
                .with_state(EndpointState { metrics, token }),
        }
    }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let (stopped, mut stopping) = tokio::sync::oneshot::channel();
        let server = axum::serve(self.listener, self.router)
            .with_graceful_shutdown(async move {
                shutdown.await;
                let _ = stopped.send(());
            })
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result,
            _ = &mut stopping => {
                // A slow scrape/client must not keep API or worker shutdown
                // alive indefinitely. Dropping the server closes connections.
                match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
                    Ok(result) => result,
                    Err(_) => Ok(()),
                }
            },
        }
    }
}
fn validate_endpoint_config(
    address: &str,
    token: Option<&str>,
) -> Result<SocketAddr, MetricsConfigError> {
    let address = address
        .parse::<SocketAddr>()
        .map_err(|_| MetricsConfigError::Address)?;
    if let Some(token) = token {
        if !(32..=4096).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(MetricsConfigError::Token);
        }
    } else if !address.ip().is_loopback() {
        return Err(MetricsConfigError::PublicWithoutToken);
    }
    Ok(address)
}
async fn scrape(State(state): State<EndpointState>, headers: HeaderMap) -> Response {
    if let Some(expected) = state.token.as_deref() {
        let provided = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "Unauthorized\n",
            )
                .into_response();
        }
    }
    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        state.metrics.render(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn genuine_partial_usage_has_its_own_denominator_and_saturating_sum() {
        let metrics = Metrics::new();
        metrics.model_metadata(
            ModelFinish::Stop,
            ModelUsage::Present,
            [Some(u64::MAX), None, None],
        );
        metrics.model_metadata(
            ModelFinish::Length,
            ModelUsage::Present,
            [Some(3), Some(0), None],
        );
        metrics.model_metadata(ModelFinish::Other, ModelUsage::Invalid, [Some(99); 3]);
        metrics.model_metadata(ModelFinish::Missing, ModelUsage::Missing, [None; 3]);
        let output = metrics.render();
        assert!(output.contains(&format!(
            "ocr_model_tokens_total{{kind=\"prompt\"}} {}\n",
            u64::MAX
        )));
        assert!(output.contains("ocr_model_token_observations_total{kind=\"prompt\"} 2\n"));
        assert!(output.contains("ocr_model_token_observations_total{kind=\"completion\"} 1\n"));
        assert!(output.contains("ocr_model_token_observations_total{kind=\"total\"} 0\n"));
        assert!(output.contains("ocr_model_finishes_total{reason=\"other\"} 1\n"));
        assert!(output.contains("ocr_model_usage_total{state=\"missing\"} 1\n"));
        assert!(output.contains("ocr_model_usage_total{state=\"invalid\"} 1\n"));
    }
    #[test]
    fn operational_metrics_are_cached_fixed_cardinality_and_retain_last_good_snapshot() {
        let metrics = Metrics::new();
        assert!(!metrics.render().contains("ocr_operational_jobs"));
        metrics.enable_operational_collector();
        metrics.operational_snapshot(OperationalSnapshot {
            queued_jobs: 7,
            processing_jobs: 2,
            failed_jobs_24h: 1,
            eligible_jobs: 5,
            delayed_jobs: 2,
            oldest_queued_seconds: 19,
            seconds_since_last_progress: 3,
            due_outbox_jobs: 4,
            oldest_due_outbox_seconds: 8,
            expired_leases: 1,
            redis_pending_entries: 6,
            redis_memory_bytes: 4096,
        });
        metrics.operational_collector_failed();
        let output = metrics.render();
        assert!(output.contains("ocr_operational_jobs{state=\"queued\"} 7\n"));
        assert!(output.contains("ocr_operational_eligible_jobs 5\n"));
        assert!(output.contains("ocr_operational_redis_pending_entries 6\n"));
        assert!(output.contains("ocr_operational_redis_memory_bytes 4096\n"));
        assert!(output.contains("ocr_operational_collector_error 1\n"));
        assert!(output.contains("ocr_operational_collector_last_success_timestamp_seconds "));
        assert!(!output.contains("job_id"));
    }
    #[test]
    fn histogram_cumulative_buckets_and_sum_are_exact() {
        let metrics = Metrics::new();
        metrics.observe(Stage::Crop, Duration::from_millis(5));
        metrics.observe(Stage::Crop, Duration::from_millis(6));
        metrics.observe(Stage::Crop, Duration::from_secs(700));
        let output = metrics.render();
        assert!(
            output.contains("ocr_stage_duration_seconds_bucket{stage=\"crop\",le=\"0.005\"} 1\n")
        );
        assert!(
            output.contains("ocr_stage_duration_seconds_bucket{stage=\"crop\",le=\"0.01\"} 2\n")
        );
        assert!(
            output.contains("ocr_stage_duration_seconds_bucket{stage=\"crop\",le=\"+Inf\"} 3\n")
        );
        assert!(output.contains("ocr_stage_duration_seconds_sum{stage=\"crop\"} 700.011\n"));
        assert!(output.contains("ocr_stage_duration_seconds_count{stage=\"crop\"} 3\n"));
        assert!(output.len() < 60_000);
        assert!(!output.contains("NaN"));
    }
    #[test]
    fn guards_release_inflight_and_record_interruption() {
        let metrics = Metrics::new();
        {
            let _attempt = metrics.job_attempt();
            assert!(
                metrics
                    .render()
                    .contains("ocr_active{activity=\"job\"} 1\n")
            );
        }
        metrics.job_attempt().finish(JobOutcome::Succeeded);
        let output = metrics.render();
        assert!(output.contains("ocr_active{activity=\"job\"} 0\n"));
        assert!(output.contains("ocr_job_attempts_total{outcome=\"aborted\"} 1\n"));
        assert!(output.contains("ocr_job_attempts_total{outcome=\"succeeded\"} 1\n"));
        assert!(output.contains("ocr_stage_duration_seconds_count{stage=\"document\"} 2\n"));
    }
    #[test]
    fn parallel_updates_are_isolated_and_lossless() {
        let metrics = std::sync::Arc::new(Metrics::new());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let metrics = metrics.clone();
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        let _active = metrics.active(Activity::Model);
                        metrics.observe(Stage::Model, Duration::from_millis(1));
                        metrics.event(Event::RegionCacheHit);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let output = metrics.render();
        assert!(output.contains("ocr_active{activity=\"model\"} 0\n"));
        assert!(output.contains("ocr_events_total{event=\"region_cache_hit\"} 4000\n"));
        assert!(output.contains("ocr_stage_duration_seconds_count{stage=\"model\"} 4000\n"));
        assert!(output.contains("ocr_stage_duration_seconds_sum{stage=\"model\"} 4\n"));
        assert!(
            Metrics::new()
                .render()
                .contains("ocr_events_total{event=\"region_cache_hit\"} 0\n")
        );
    }
    #[test]
    fn http_labels_are_bounded_route_groups() {
        let metrics = Metrics::new();
        metrics.http_request(
            HttpRoute::from_matched_path(Some("/v1/ocr/jobs/{id}/business")),
            StatusCode::OK,
            Duration::from_millis(2),
        );
        metrics.http_request(
            HttpRoute::from_matched_path(Some("/someone-private-document")),
            StatusCode::NOT_FOUND,
            Duration::from_millis(1),
        );
        let output = metrics.render();
        assert!(output.contains("ocr_http_requests_total{route=\"business\",status=\"2xx\"} 1\n"));
        assert!(output.contains("ocr_http_requests_total{route=\"other\",status=\"4xx\"} 1\n"));
        assert!(!output.contains("someone-private"));
    }
    #[test]
    fn endpoint_config_fails_closed_without_token_leak() {
        assert!(validate_endpoint_config("127.0.0.1:9000", None).is_ok());
        assert!(validate_endpoint_config("[::1]:9000", None).is_ok());
        assert!(matches!(
            validate_endpoint_config("0.0.0.0:9000", None),
            Err(MetricsConfigError::PublicWithoutToken)
        ));
        assert!(matches!(
            validate_endpoint_config("0.0.0.0:9000", Some("private-secret")),
            Err(MetricsConfigError::Token)
        ));
        assert!(validate_endpoint_config("0.0.0.0:9000", Some(&"a".repeat(32))).is_ok());
        assert!(
            !MetricsConfigError::Token
                .to_string()
                .contains("private-secret")
        );
    }
    #[tokio::test]
    async fn scrape_enforces_exact_bearer_and_text_exposition() {
        let state = EndpointState {
            metrics: process_metrics(),
            token: Some("a".repeat(32)),
        };
        assert_eq!(
            scrape(State(state.clone()), HeaderMap::new())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", "a".repeat(32)).parse().unwrap(),
        );
        let accepted = scrape(State(state), headers).await;
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(accepted.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("# TYPE ocr_stage_duration_seconds histogram")
        );
    }
    #[tokio::test]
    async fn exporter_drains_on_shutdown() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = MetricsEndpoint::new(listener, process_metrics(), None);
        assert!(endpoint.local_addr().unwrap().ip().is_loopback());
        tokio::time::timeout(Duration::from_secs(1), endpoint.serve(async {}))
            .await
            .unwrap()
            .unwrap();
    }
}
