# Pipeline observability

The API and Rust worker export Prometheus text format on **separate, opt-in listeners**. Process observations use fixed-size atomics and enum dimensions; the worker's O01 operational snapshot is a short-held cached value. Scrapes never query PostgreSQL or Redis, log document contents, or allocate a label map for each region. Each process has its own registry. Counters reset when that process restarts, so dashboards must use `rate`/`increase` and preserve Prometheus target labels.

## Configuration

| Variable | Default | Behavior |
| --- | --- | --- |
| `API_METRICS_BIND_ADDR` | unset | API process metrics socket, e.g. `127.0.0.1:9091` |
| `WORKER_METRICS_BIND_ADDR` | unset | Worker process metrics socket, e.g. `127.0.0.1:9092` |
| `METRICS_BEARER_TOKEN` | unset | Separate scrape credential; 32–4096 visible ASCII characters |

An unset/empty bind variable disables that exporter. An address must be an IP socket address. Loopback listeners can be used without a token; any non-loopback address, including `0.0.0.0` inside Docker, requires the metrics token. Invalid configurations fail startup without printing credentials. `GET /metrics` accepts `Authorization: Bearer <token>` when configured; unknown paths return 404. The application API port does not gain an unauthenticated metrics route.

For Docker, bind the exporter to `0.0.0.0` inside its container, pass a generated token through the environment, and map only to host loopback when local inspection is needed. Give a Prometheus scraper network access explicitly. Store the same token in Prometheus's `authorization.credentials_file`; do not put it into a tracked YAML file. Bearer authentication provides access control; use a private network or TLS proxy if scrapes cross an untrusted network. Exporters stop with their process and cap graceful scrape draining at five seconds.

Example Prometheus scrape configuration (host processes):

```yaml
scrape_configs:
  - job_name: ocr-api
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:9091"]
    authorization:
      type: Bearer
      credentials_file: /run/secrets/ocr_metrics_token
  - job_name: ocr-worker
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:9092"]
    authorization:
      type: Bearer
      credentials_file: /run/secrets/ocr_metrics_token
```

Targets running inside a separate Prometheus container need the actual Docker service names and internal ports, instead of its own `127.0.0.1`.

## Exported measurements

| Metric | Dimensions | Meaning |
| --- | --- | --- |
| `ocr_process_uptime_seconds` | none | Monotonic uptime of this process |
| `ocr_stage_duration_seconds` | `stage` | Histogram of stage wall times, including error/cancellation paths |
| `ocr_active` | `activity` | Work in progress in this process: job, model, layout, postprocess |
| `ocr_job_attempts_total` | `outcome` | Succeeded, failed, retry scheduled, lost lease, failed/stale commit, aborted attempt |
| `ocr_events_total` | `event` | Fixed transport, retry, checkpoint and recovery events |
| `ocr_http_requests_total` | `route`, `status` | Bounded route group and response status class |
| `ocr_http_request_duration_seconds` | `route` | API handler time through response headers |
| `ocr_model_finishes_total` | `reason` | Parsed server finish reasons; unknown values become `other` |
| `ocr_model_usage_total` | `state` | Server token usage `present`, `missing`, or `invalid` |
| `ocr_model_tokens_total` | `kind` | Genuine reported prompt/completion/total tokens, independently summed |
| `ocr_model_token_observations_total` | `kind` | Denominator for each reported token component |
| `ocr_operational_jobs` | `state` | Worker snapshot of queued, processing and failed-in-last-24-hours PostgreSQL jobs |
| `ocr_operational_eligible_jobs` / `ocr_operational_delayed_jobs` | none | Queued jobs due now versus waiting for `available_at` |
| `ocr_operational_oldest_queued_seconds` | none | Age of the oldest queued job |
| `ocr_operational_seconds_since_last_progress` | none | Age of the most recent `updated_at` among processing jobs |
| `ocr_operational_due_outbox_jobs` / `ocr_operational_oldest_due_outbox_seconds` | none | Dispatcher-claimable outbox count/age: queued-and-eligible or processing-with-expired-worker-lease; active dispatch leases are excluded |
| `ocr_operational_expired_leases` | none | Processing jobs whose PostgreSQL lease has expired |
| `ocr_operational_redis_pending_entries` / `ocr_operational_redis_memory_bytes` | none | Redis Stream PEL size and Redis-reported used memory |
| `ocr_operational_collector_last_success_timestamp_seconds` | none | Unix timestamp of the last complete PostgreSQL + Redis snapshot; zero means none yet |
| `ocr_operational_collector_error` | none | `1` when the most recent snapshot failed; cached last-good values remain visible |

Operational series are enabled only by the worker collector. It samples immediately at startup and then every 30 seconds. PostgreSQL uses one aggregate query with a 3-second deadline; PostgreSQL and Redis collection together has a 5-second deadline. Redis calls use the configured Redis command timeout. A partial sample is never published: failure preserves the previous values, updates the collector-error gauge, and leaves the last-success timestamp unchanged. Restart resets this process-local cache. These are observations, not durable queue state.

The current snapshot intentionally excludes PostgreSQL pool saturation/storage, host CPU/RSS, Redis eviction rates, and inference-server health. The API exporter remains process-only. Query cardinality is fixed; labels contain only the three fixed job states. No threshold is asserted by the implementation.

Stages are `queue_wait`, `document`, `inspect`, `render`, `layout`, `layout_queue`, `layout_startup`, `layout_encode`, `layout_inference`, `crop`, `model_queue`, `model`, `format`, `postprocess`, `native_text`, `extraction`, `validation`, and `persistence`. Instrumented stages have counts greater than zero; stages unused by a strategy remain zero. Histograms cover 1 ms through 600 s plus `+Inf`; times are seconds in exposition. Nested stage measurements overlap: adding all stage sums does not yield document duration. Stage timers measure wall time and may include waiting for capacity; `model_queue` specifically separates waiting for a local model permit from an HTTP model attempt. A cancelled or failed timer remains a timing observation, not a success count. Layout substage observations are emitted for successful detector replies; overall `layout` also covers failed/cancelled attempts. Warm requests have zero startup duration.

`model_failed` counts completed upstream inference attempts returning an error;
cancelled/dropped HTTP futures are not completed errors. `recovery_attempted`
counts extra crop observations. No counter claims an observation improved truth;
that requires independently labeled evaluation.

`parser_unavailable` counts failed isolated-parser health probes. The worker
probes before each claim batch (a healthy answer is reused for one second). While
probes fail it stops claiming, backs off up to 30 seconds, and leaves
notifications in Redis, so a parser outage delays queued jobs instead of spending
their attempts. Jobs already running when the parser fails still lose that
attempt. A sustained non-zero rate means no new work is being claimed.

`model_attempt_started` begins after acquiring the shared per-process R permit;
`model_cancelled` counts dropped started attempts. Work cancelled while waiting
for R has not started an inference attempt. These counters describe local HTTP
attempts: dropping a Rust future releases its local permit, but does not attest
that the remote model stopped its GPU work. Tokens are recorded only from parsed
server replies, including a truncated reply that still consumed tokens. Negative,
fractional, string, overflowing or inconsistent token usage is invalid and is
never estimated. Partial usage remains partial. A reported zero has an observation;
an unavailable count has none. Cache hits do not replay finish/token measurements.

Successful job results add bounded `execution.inference_telemetry` summaries of
attempt outcomes, checkpoints, JPEG dimensions/pixels/bytes, base64 payload bytes,
finish reasons and usage. Input/task arrays have fixed sizes; crop images and
transcriptions are not copied into telemetry. Dimensions come from the actual
JPEG header; they are not necessarily the post-resize tensor dimensions inside
the server. The normalized crop bytes are input bytes, not total HTTP wire bytes.
Counters/sums saturate at u64 limits. Each leased attempt has its own recorder;
`for_job` clones still share R. With INFO logging enabled, the worker logs one bounded attempt trace after
ordinary processing completion/failure/timeout; raw OCR text is never logged.
Abrupt process exit can prevent that per-job log;
metrics are process-local, and successful persisted summaries cover that attempt,
not earlier attempts. Existing raw Markdown/layout remain unchanged.

`execution.identity` records actual client prompt/preprocessing hashes, requested
profile/model alias, detector hash and worker package version. `INFERENCE_REVISION`
is still an operator declaration. Optional `INFERENCE_DEPLOYMENT_JSON` accepts up
to 8192 bytes and only these fields (all optional):

```json
{
  "runtime": "vllm-metal",
  "runtime_version": "0.31.0",
  "weights_revision": "pinned-revision",
  "quantization": "4bit",
  "max_sequences": 1,
  "max_pixels": 1572864
}
```

Additional supported fields are `weights_sha256`, `tokenizer_revision`,
`image_digest` and `max_model_len`. Unknown fields, unsafe/unbounded identifiers,
invalid SHA-256 and out-of-range numeric limits fail startup without echoing the
input. Empty/unset means unavailable. The declaration is always labelled
`operator_declared_unverified`; `verified_server_configuration` remains null.
Neither `/v1/models` nor this JSON verifies weights or scheduler/pixel settings.
Use separately captured, access-controlled startup records for those checks.
Never place credentials in the declaration. Changing it invalidates checkpoints;
leaving it unset preserves established checkpoint keys.

Non-streaming completions do not expose TTFT; `ttft_seconds` remains null. No token
count, finish reason, layout score or timing is a calibrated field confidence.

`queue_wait` measures submission to the **first successful SQL claim**, using two PostgreSQL timestamps. It excludes retries and does not measure current waiting jobs or model-server queueing. `document` includes the claimed attempt through the fenced storage update. One job can have several attempts. A successful inference whose commit fails is counted as `commit_failed`, rather than a durable success. RAII guards release active gauges on task cancellation/panic and mark an unfinished attempt `aborted`. Process termination can still discard in-memory observations; PostgreSQL remains authoritative.

HTTP route groups are submit, job, status, business, artifact, review, health, other. Unknown routes collapse to other. Status classes are `1xx`…`5xx`. HTTP duration excludes time spent transmitting/consuming response bodies, and cancelled requests without a completed response are not counted. Metrics scraper traffic uses the separate listener and does not inflate API request counters.

No metric includes job ID, UUID, filename, source text, extracted field/value, reviewer identity, token, raw URL, request ID, or arbitrary error text. Identifiers required for per-job diagnosis remain in access-controlled job data and structured logs. Do not add them as Prometheus labels.

## Useful queries and operating decisions

Processing throughput, in durably completed attempts per second:

```promql
sum(rate(ocr_job_attempts_total{outcome="succeeded"}[5m]))
```

Document p95 across workers:

```promql
histogram_quantile(0.95,
  sum by (le) (rate(ocr_stage_duration_seconds_bucket{stage="document"}[10m])))
```

Compare model waiting with model execution before raising concurrency:

```promql
sum by (stage) (rate(ocr_stage_duration_seconds_sum{stage=~"model_queue|model"}[5m]))
/
sum by (stage) (rate(ocr_stage_duration_seconds_count{stage=~"model_queue|model"}[5m]))
```

Checkpoint hit fraction (zero traffic gives no meaningful ratio):

```promql
sum(rate(ocr_events_total{event="region_cache_hit"}[10m]))
/
sum(rate(ocr_events_total{event=~"region_cache_hit|region_cache_miss"}[10m]))
```

Alert on missing scrape targets, sustained lease renewal/commit failures, retries, model errors, Redis read/outbox/ack errors, and growing latency. Set thresholds after a representative load run; an arbitrary p95 target has no evidence yet. Benchmark document and region concurrency separately with a fixed corpus, tracking throughput, p50/p95, failure rate, resident memory, and model utilization. A higher throughput accompanied by more errors or worse required tail latency is not a successful optimization.

The O01 worker gauges detect a backlog even when **no job is claimed**. For example, sustained oldest-queued age with eligible work and no increase in completed attempts distinguishes processing blockage from intake. A nonzero `ocr_operational_collector_error` or an old `ocr_operational_collector_last_success_timestamp_seconds` means the snapshot itself is unavailable; do not interpret retained values as fresh.

Example starting expressions (thresholds and windows require deployment-specific calibration):

```promql
ocr_operational_oldest_queued_seconds
ocr_operational_due_outbox_jobs
ocr_operational_expired_leases
time() - ocr_operational_collector_last_success_timestamp_seconds
ocr_operational_collector_error
```

Also monitor PostgreSQL pool/query latency and storage, Redis evictions, host CPU/RSS, and the inference backend's own metrics. These are not exported by O01 and are not fabricated from application timings. Native Metal and GPU vLLM backends must be benchmarked independently; available metrics can differ.

## Verification and sources

Tests construct isolated registries and cover exact histogram boundaries/sums, simultaneous four-thread observations, fixed route classification, cancellation guards, fail-closed listener configuration, bearer authentication, graceful endpoint shutdown, fixed-cardinality operational rendering and last-good snapshot retention. Disposable PostgreSQL/Redis integration tests exercise the real queue/lease/outbox aggregates, pending-entry count and memory query. No test relies on shared global counter values.

The design follows Prometheus guidance on [bounded instrumentation](https://prometheus.io/docs/practices/instrumentation/), [metric and label naming](https://prometheus.io/docs/practices/naming/) and [text exposition](https://prometheus.io/docs/instrumenting/exposition_formats/). [Histogram quantiles](https://prometheus.io/docs/practices/histograms/) are approximations determined by bucket boundaries; they are not calibrated OCR confidence. vLLM's [engine metrics](https://docs.vllm.ai/en/latest/usage/metrics/) provide model-server measurements separately from Rust client timings.
