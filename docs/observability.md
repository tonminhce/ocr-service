# Pipeline observability

The API and Rust worker export Prometheus text format on **separate, opt-in listeners**. Process observations use fixed-size atomics and enum dimensions; worker durable/runtime observations are short-held cached values. Scrapes never query PostgreSQL or Redis, log document contents, or allocate a label map for each region. Each process has its own registry. Counters reset when that process restarts, so dashboards must use `rate`/`increase` and preserve Prometheus target labels.

## Configuration

| Variable | Default | Behavior |
| --- | --- | --- |
| `API_METRICS_BIND_ADDR` | unset | API process metrics socket, e.g. `127.0.0.1:9091` |
| `WORKER_METRICS_BIND_ADDR` | unset | Worker process metrics socket, e.g. `127.0.0.1:9092` |
| `METRICS_BEARER_TOKEN` / `METRICS_BEARER_TOKEN_FILE` | unset | Separate scrape credential; 32–4096 visible ASCII characters; production mounts a secret file |

An unset/empty bind variable disables that exporter. An address must be an IP socket address. Loopback listeners can be used without a token; any non-loopback address, including `0.0.0.0` inside Docker, requires the metrics token. Invalid configurations fail startup without printing credentials. `GET /metrics` accepts `Authorization: Bearer <token>` when configured; unknown paths return 404. The application API port does not gain an unauthenticated metrics route.

For Docker, bind the exporter to `0.0.0.0` inside its container, mount a generated token through `METRICS_BEARER_TOKEN_FILE`, and map only to host loopback when local inspection is needed. Do not configure both direct and file forms. Give a Prometheus scraper network access explicitly. Store the same token in Prometheus's `authorization.credentials_file`; do not put it into a tracked YAML file. Bearer authentication provides access control; use a private network or TLS proxy if scrapes cross an untrusted network. Exporters stop with their process and cap graceful scrape draining at five seconds.

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
| `ocr_operational_oldest_queued_seconds` | none | Age of the oldest queued job since submission, including delayed retries |
| `ocr_operational_oldest_eligible_seconds` | none | How long the longest-waiting eligible queued job has been claimable (`now - available_at`); delayed retries are excluded |
| `ocr_operational_seconds_since_last_lease_update` | none | Age of the newest `updated_at` among processing jobs. Lease heartbeats update it, so it shows worker liveness, not OCR progress |
| `ocr_operational_due_outbox_jobs` / `ocr_operational_oldest_due_outbox_seconds` | none | Dispatcher-claimable outbox count/age: queued-and-eligible or processing-with-expired-worker-lease; active dispatch leases are excluded |
| `ocr_operational_expired_leases` | none | Processing jobs whose PostgreSQL lease has expired |
| `ocr_operational_database_bytes` | none | `pg_database_size` of the application database (tables, indexes, TOAST and bloat) |
| `ocr_operational_redis_pending_entries` / `ocr_operational_redis_memory_bytes` | none | Redis Stream PEL size and Redis-reported used memory |
| `ocr_operational_redis_maxmemory_bytes` | none | Configured Redis maxmemory from the same INFO response; zero means unlimited, not safe capacity |
| `ocr_operational_collector_last_success_timestamp_seconds` | none | Unix timestamp of the last complete PostgreSQL + Redis snapshot; zero means none yet |
| `ocr_operational_collector_error` | none | `1` when the most recent snapshot failed; cached last-good values remain visible |
| `ocr_runtime_last_sample_timestamp_seconds` | none | Last worker runtime sample time, independent of successful PostgreSQL/Redis collection; zero before first sample |
| `ocr_database_pool_connections` | `state` | Worker-local SQLx size/idle/max occupancy; neither waiter count nor acquisition latency |
| `ocr_dependency_up` / `ocr_dependency_last_check_timestamp_seconds` / `ocr_dependency_probe_duration_seconds` | `dependency` | Bounded parser/model_catalog probe outcome, completion time and wall duration; failed probes still advance their timestamp |
| `ocr_parser_sessions` | `state` | Shared parser active/max sessions from a successful health reply; absent on failed health, aggregate replicas with max, not sum |

Operational series are enabled only by the worker collector. It samples immediately at startup and then every 30 seconds. PostgreSQL uses one aggregate query in a read-only transaction with `SET LOCAL statement_timeout = '3s'`, so the server cancels a slow query instead of only the client abandoning it; a 4-second client deadline also covers pool acquisition. PostgreSQL and Redis collection together has a 5-second deadline. Redis calls use the configured Redis command timeout. Used memory and configured maxmemory come from one existing `INFO memory` response, without an additional request. A partial sample is never published: failure preserves the previous values, updates the collector-error gauge, and leaves the last-success timestamp unchanged. Restart resets this process-local cache. These are observations, not durable queue state.

Runtime observation runs alongside the durable snapshot. Parser Unix-socket health and model catalog checks run concurrently, each with a two-second bound; SQLx pool sampling reads local occupancy without a database query. The model client rejects redirects/proxies, bounds the response to 64KiB and verifies the configured model ID in `/v1/models`. These checks continue during a database/Redis outage. Catalog success is not generation readiness, loaded-weight attestation or model capacity. The API exporter remains process-only. Host CPU/RSS/swap/free disk, container memory pressure, Redis eviction rates and inference-engine internals are not exported or inferred from these observations.

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

The O01 worker gauges detect a backlog even when **no job is claimed**. Sustained `oldest_eligible_seconds` with no increase in completed attempts means claimable work is not being picked up (workers down, parser gate closed, or every slot stuck). It is a better stall signal than oldest-queued age, which also counts retries deliberately delayed by backoff. A nonzero `ocr_operational_collector_error` or an old `ocr_operational_collector_last_success_timestamp_seconds` means the snapshot itself is unavailable; do not interpret retained values as fresh.

Example starting expressions (thresholds and windows require deployment-specific calibration):

```promql
ocr_operational_oldest_eligible_seconds
ocr_operational_due_outbox_jobs
ocr_operational_expired_leases
deriv(ocr_operational_database_bytes[6h])
increase(ocr_events_total{event="parser_unavailable"}[5m])
time() - ocr_operational_collector_last_success_timestamp_seconds
ocr_operational_collector_error
```

Also monitor PostgreSQL acquisition/query latency, Redis evictions, host CPU/RSS/swap/free disk, container memory pressure, successful backups and the inference backend's own metrics. Local pool occupancy and database-size gauges do not supply those measurements. Native Metal and GPU vLLM backends must be benchmarked independently; available metrics can differ.

## Opt-in monitoring candidate

[compose.monitoring.yaml](../compose.monitoring.yaml) adds digest-pinned, resource-bounded Prometheus and Alertmanager under the `monitoring` profile; it is not enabled by normal application startup. The [alert runbook](alert-runbook.md) covers activation on an approved existing project with matching images and `--no-build`, safe inspection, all 13 alerts, cleanup and trust boundaries. The local-only receiver has no external destination or human notification. UIs/APIs bind host loopback but remain unauthenticated inside the trusted Compose default network.

Candidate rules filter each reporting instance by successful scrape and positive timestamps no older than 90 seconds, allowing at most five seconds future clock skew. Shared durable gauges aggregate with max after this filtering; pool occupancy retains instance identity. Redis memory alerts compare used memory to its actual positive configured maxmemory, not the larger container limit. An unlimited zero limit disables that ratio alert; it is not healthy-capacity evidence. Static service-name targets currently support one API and one worker; production replica discovery must provide distinct instance labels.

Validate with `bash scripts/check-monitoring.sh`, which uses pinned tools and a temporary synthetic token without live activation. [monitoring-smoke.py](../scripts/monitoring-smoke.py) creates a unique disposable synthetic stack from existing images, checks metrics authentication, pauses dependencies/stops services, observes Prometheus/Alertmanager firing and recovery, and checks preserved job rows plus fresh post-recovery processing. Both local drills passed; the strengthened repeat passed all 18 checks, including resource/secret-mount boundaries, unchanged complete original job rows and runtime progression during PostgreSQL outage. See the [execution trace](o01-alerting-execution-trace.md). This is orchestration evidence, not real-model throughput, workload sizing or human notification delivery. Its private reports/logs stay under ignored artifacts.

For release inventories that include these services, pass `--monitoring` to [release-manifest.py](../scripts/release-manifest.py); the inventory/lock then includes Prometheus and Alertmanager identity. See [release operations](release-operations.md). Host/swap/free-disk/backup exporters, independent monitoring watchdogs, notification/rule-error policies, threshold calibration and production deployment acceptance remain gates.

## Verification and sources

Tests construct isolated registries and cover exact histogram boundaries/sums, simultaneous four-thread observations, fixed route classification, cancellation guards, fail-closed listener configuration, bearer authentication, graceful endpoint shutdown, fixed-cardinality operational rendering and last-good snapshot retention. Disposable PostgreSQL/Redis integration tests exercise the real queue/lease/outbox aggregates, pending-entry count and used/configured memory query. Probe tests cover bounded response/time, authentication/redirect behavior, independent failed probes and invalidated parser capacity. The policy fixtures contain 39 cases, 124 alert assertions and three PromQL assertions, including exact hold boundaries, missing metrics, counter resets and stale/fresh replicas. No test relies on shared global counter values.

The design follows Prometheus guidance on [bounded instrumentation](https://prometheus.io/docs/practices/instrumentation/), [metric and label naming](https://prometheus.io/docs/practices/naming/) and [text exposition](https://prometheus.io/docs/instrumenting/exposition_formats/). [Histogram quantiles](https://prometheus.io/docs/practices/histograms/) are approximations determined by bucket boundaries; they are not calibrated OCR confidence. vLLM's [engine metrics](https://docs.vllm.ai/en/latest/usage/metrics/) provide model-server measurements separately from Rust client timings.
