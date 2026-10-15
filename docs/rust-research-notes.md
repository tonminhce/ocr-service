# Rust OCR service research

Research date: 14 October 2026; pinned SDK/native-library audit updated on 15 October 2026. These notes separate documented library/database behavior from application design and implementation evidence. The reference course is the architectural starting point; framework examples do not establish one mandatory production folder structure.

## Recommended service boundary

Build an internal OCR service with two independently runnable Rust processes: an Axum HTTP API that accepts documents and exposes job status/results, and a Rust worker that dispatches/consumes Redis Streams notifications, claims PostgreSQL jobs, renders pages, calls an OCR inference endpoint, and persists results. PostgreSQL stores source bytes, state, results, and the recurring delivery outbox; Redis stores job IDs. Another service calls this API directly. An API gateway adds no necessary OCR behavior and is outside this implementation.

The implementation replaces the Python application consumer, layout preprocessing/postprocessing, region cropping, HTTP orchestration, formatting, validation, and persistence with Rust. Its PP-DocLayoutV3 helper executes an exported graph through ONNX Runtime. Python is used for one-time public-weight export and reference QA, outside the application runtime. Keeping vLLM as the recognition endpoint means the deployment still contains an external model-serving runtime. Calling vLLM from Rust does not turn vLLM or the model into a native Rust inference engine. Replacing that runtime is a separate model compatibility and GPU optimization project.

## Axum structure: evidence and practical choice

Axum provides handlers, typed extractors, routers, and Tower middleware. Its official repository points to examples rather than prescribing a production directory tree. The current repository warns that its `main` branch is working toward Axum 0.9; the released 0.8 branch is the appropriate implementation reference. [Axum repository](https://github.com/tokio-rs/axum).

`State` carries shared configuration and pools. Routers can be composed with a common state type, and `FromRef` supports extracting narrower substates. Put pool/client handles in cloneable application state; avoid a global mutex around the database pool or HTTP client. [Axum State](https://docs.rs/axum/latest/axum/extract/struct.State.html).

The LaunchBadge Axum/SQLx example keeps bootstrap in `main.rs`, composes the HTTP module separately, and uses migrations. Its author explicitly frames the layout as an architectural experiment and personal judgment. Use it for separation of bootstrap, routes, and data access, not as an up-to-date template to copy wholesale. [Project and structure commentary](https://github.com/launchbadge/realworld-axum-sqlx), [bootstrap source](https://raw.githubusercontent.com/launchbadge/realworld-axum-sqlx/main/src/main.rs), [HTTP composition source](https://raw.githubusercontent.com/launchbadge/realworld-axum-sqlx/main/src/http/mod.rs).

Implemented boundaries for this repository:

```text
Cargo.toml                 shared dependency and package settings
crates/
  ocr-core/                shared domain types and job persistence
    src/{domain,store}.rs
    migrations/
    tests/
  ocr-api/                 HTTP contract, API settings, middleware, bootstrap
  ocr-worker/              Redis transport/outbox dispatch, leases, rendering, inference
  ocr-layout/              PP-DocLayoutV3 Rust helper, preprocessing, order, polygons
docs/                      design, API usage, operations, research evidence
```

This is intentionally a modest workspace. The layout helper has a separate process boundary so deadlines and cancellation can terminate native inference. Add another inference or storage crate when there is a second implementation or meaningful reuse. Avoid one crate per tiny module and avoid duplicating the domain model between API and worker. Cargo workspaces share a lockfile and build output and permit common metadata/dependencies; the Cargo Book recommends flat member directories. [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html).

## Redis delivery with a durable PostgreSQL outbox

The implemented architecture uses Redis Streams for delivery and PostgreSQL as the authoritative job store. Submission commits immutable input bytes, the job row, and a trigger-created outbox record in one database transaction. Redis publication happens later in an independent Rust dispatcher task, which remains active when inference slots are full. This closes the database-to-Redis publication gap without assuming a distributed transaction. The API can durably accept work during a Redis outage, subject to its admission limit.

Default document size is 10 MiB (configurable up to a database-enforced 50 MiB hard maximum), with 1,000 queued/processing jobs. Admission serializes brief enqueue transactions with a transaction-scoped advisory lock; matching idempotent retries still replay at full capacity. Pending input alone can approach 9.8 GiB at the default limit, before WAL, indexes, and replication; a 50 MiB file limit raises that to about 48.8 GiB. Successful course jobs and terminal failures retain originals until pruning; successful legacy full-page jobs clear their bytes. The pending-job cap does not bound terminal sources/results. Larger deployments should consider object storage with explicit upload ownership and cleanup while preserving durable job/outbox state.

PostgreSQL explicitly identifies `SKIP LOCKED` as useful for consumers of a queue-like table. The dispatcher uses it to lease bounded outbox batches, publishes `XADD` with only a job ID, then marks the delivery using its dispatch token. A lost response or database mark can produce a duplicate. The outbox recurs after a configured delay, excludes live processing leases, and survives until terminal completion; it can therefore rebuild delivery after the whole Redis stream is lost. [PostgreSQL SELECT locking clause](https://www.postgresql.org/docs/current/sql-select.html).

Each process-specific Redis consumer UUID uses `XREADGROUP` for new notifications and bounded cursor-based `XAUTOCLAIM` for old pending entries. PostgreSQL `claim_by_id` atomically checks the notified job's due time, attempt budget, status, and lease, then increments attempts and assigns a fresh expiry/token. Duplicates cannot start another attempt while a valid owner is processing. Page rendering and inference happen outside database transactions. The worker commits success/failure in PostgreSQL before atomic Lua `XACK` plus `XDEL`; one dedicated group per stream is required because acknowledged entries are deleted. [Redis consumer groups](https://redis.io/docs/latest/commands/xreadgroup/), [pending recovery](https://redis.io/docs/latest/commands/xautoclaim/), [PostgreSQL explicit locking](https://www.postgresql.org/docs/current/explicit-locking.html).

Redis is the only delivery transport; there is no fallback that directly polls PostgreSQL for OCR claims during a Redis outage. Redis command deadlines/backoff are separate from the PostgreSQL OCR heartbeat. Compose uses digest-pinned Redis 8.2 Alpine with AOF `everysec`, `noeviction`, a 128 MiB cache limit, a 256 MiB container limit, and a named volume. The recurring PostgreSQL outbox repairs notification loss independently of Redis persistence; backups remain necessary for durable database sources/results. See [queue design and failure cases](redis-postgres.md).

The following correctness requirements are application design recommendations derived from that behavior:

- Every heartbeat, success update, and failure update checks job ID, current lease token, processing state, and an unexpired lease.
- A worker whose lease expires cannot publish a result, even if its OCR request eventually returns. A replacement worker receives a new token.
- Reclaim expired jobs within the attempt budget. Expired jobs that already exhausted the budget become terminal failures.
- Retry transient network failures, inference overload, and retryable server failures with delayed exponential backoff. Reject unsupported/corrupt inputs as terminal failures.
- An idempotency key maps to one submission, with a fingerprint check. Returning the existing job for a matching retry is safe; reusing the key with different input is a conflict. A single API credential provides one key scope; multi-tenant credentials require tenant-scoped uniqueness and authorization.
- Persist completion once; execution itself remains at least once. A crash after inference but before completion can repeat inference. Do not promise exactly-once model execution.
- Pruning must exclude actively processing jobs and should retain results long enough for callers to collect them.

Measure pending job count/age, due outbox age, Redis pending notifications and memory, reclaim/retry counts, job/page latency, inference errors, and database growth. A successful API readiness response checks PostgreSQL connectivity; it does not establish Redis delivery, consumer progress, or inference health.

## Runtime and shutdown

Tokio's shutdown guidance separates detecting shutdown, notifying tasks, and waiting for them. `CancellationToken` and task tracking support that lifecycle. On SIGTERM, stop Redis reads and outbox dispatch, drain active jobs with their heartbeats/deadlines intact, and let abandoned leases expire after forced shutdown. Dispatcher failures and bounded queue commands do not abort active OCR. [Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown).

Image decoding/resizing/encoding is CPU work. Tokio documents that `spawn_blocking` uses a large default thread limit; bound CPU operations with a semaphore. Started blocking tasks cannot be aborted, so choose bounded images and work that terminates. For PDF rendering through `pdftoppm`, use a separate subprocess timeout and kill/reap children on timeout. [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).

Use separate limits for concurrent documents, page rendering, and inference requests. Increasing HTTP worker concurrency does not increase GPU capacity. Start small, then size from actual page latency and GPU throughput.

## Rust inference client and OCR fidelity

vLLM exposes `/v1/chat/completions` and requires a compatible model/chat template. It supports vision parameters, ignores `user`, and does not support `image_url.detail`. Direct HTTP JSON requests can include vLLM-specific options; defaults may be affected by the model repository's generation configuration. [vLLM OpenAI-compatible server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/).

The multimodal documentation shows image content parts and base64 data URLs. Sending rendered bytes as data URLs avoids making the inference server fetch arbitrary caller-supplied URLs. If URL inputs are introduced, vLLM documents domain restrictions and disabling redirects to limit SSRF exposure. [vLLM multimodal inputs](https://docs.vllm.ai/en/latest/features/multimodal_inputs/).

Use a reused `reqwest::Client`, typed request/response DTOs, a connect timeout, per-request timeout, bounded response bodies, and an explicit retry policy. A small protocol adapter is enough; adopting a large SDK is optional. Reqwest exposes those connection/timeout/pooling configuration points. [Reqwest ClientBuilder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html).

The implemented default `course_layout_vlm` strategy runs the course's PP-DocLayoutV3 region pipeline. `full_page_vlm` remains an explicit alternative. The result records the layout graph/revision/hash, recognition model, prompt/formatter versions, rendering engine, geometry, image policy, and coverage counts. The measured SDK comparison passed the retained-region contract on 93 regions across five pages and all five layout-resize fixtures. The separate strict test for raw query-rank metadata failed on three pages. Shared-image layout parity does not establish independent PDF-renderer, JPEG-crop, recognition-text, or general accuracy parity. [Pinned SDK parity evidence](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/course-layout-parity.md).

The suggested native library `saravananravi08/glm-ocr-rs` was audited at `c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d`. Its isolated default-CPU `cargo check --locked --all-targets` passed, but no real model inference was tested. It implements a different GLM/PP-DocLayout-M pipeline and is not used by this service. The audit includes source-reproduced Unicode/strip edge cases and the missing license-file packaging issue. [Library audit](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/glm-ocr-rs-audit.md).

Assess quality using a representative corpus of invoices, Vietnamese text, tables, scans, rotations, and multi-column documents. The default course profile retains image/chart placeholders without recognizing their text; `OCR_RECOGNIZE_IMAGES=true` explicitly changes that policy and reports an enhanced profile. Reject or flag truncated generations instead of treating them as complete OCR. The optional [rental extraction profile](structured-extraction.md) now supplies a separate schema, provenance, typed fields and conflicts. It requires review; generic extraction and calibrated acceptance still require representative evaluation and workflow implementation.

`TABLE_CROP_PADDING_PIXELS` is a separate optional recognition enhancement: 0 by default, 112 in the macOS example. A white border is added to Table JPEGs after the unchanged course crop/resize; it does not change detection geometry or Text/Formula preprocessing. Results record the amount/color/stage in `recognition_preprocessing` and distinguish strict, image-text, table-padding, and combined profiles. The [layout end-to-end report](layout-e2e-report.md) records the strict Table hallucination/truncation failure and isolated padding comparison. A successful isolated crop is not general OCR accuracy evidence.

## Dependency observations

The primary documentation observed on the research date reported the following current versions. These are reference observations, not a requirement to upgrade everything immediately:

| Component | Observed current docs | Baseline choice |
| --- | --- | --- |
| Axum | 0.8.9 | Released 0.8 line |
| Tokio | 1.53.3 | Compatible 1.x lockfile |
| Reqwest | 0.13.5 | Supported 0.12 baseline with explicit features |
| SQLx | 0.9.0 | Supported 0.8 baseline if avoiding a new major migration |
| Lapin | 4.12.1 | Not used; implemented transport is Redis Streams |

Version evidence: [Axum serve](https://docs.rs/axum/latest/axum/serve/struct.Serve.html), [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), [Reqwest builder](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html), [SQLx](https://docs.rs/sqlx/latest/sqlx/), [Lapin changelog](https://docs.rs/crate/lapin/latest/source/CHANGELOG.md). The local compiler at inspection was Rust 1.98.1. Commit `Cargo.lock` and validate the actual selected versions with `cargo check`, tests, and Clippy; documentation versions are not substitute evidence for a build.

## RabbitMQ as a separate future option

The implemented Redis Streams transport serves the current producer/consumer boundary. A future RabbitMQ migration would need its own operational reason, such as independent fanout or routing requirements. RabbitMQ's reliability guidance requires durable queues/persistent messages, publisher confirms, consumer acknowledgement after durable processing, and idempotent handling of redelivery. [RabbitMQ reliability](https://www.rabbitmq.com/docs/reliability).

Publisher confirms and consumer acknowledgements cover different links; an unroutable publish can still receive a confirm, so routing validation matters. [RabbitMQ confirms](https://www.rabbitmq.com/docs/confirms). If adopting Lapin, use current APIs: the 4.x library supports built-in Tokio integration and automatic recovery; older examples with external Tokio AMQP adapters do not describe the current setup. [Lapin current crate documentation](https://docs.rs/lapin/latest/lapin/). An outbox transaction should connect durable job creation with later publication; a direct database insert followed by publish is insufficient for crash safety.

## Minimum meaningful verification

Verify concurrent claims never share a live lease, idempotent concurrent submissions yield one job, conflicting fingerprints are rejected, stale tokens cannot renew/complete/fail, expired leases recover, attempt exhaustion terminates, and retries respect due time. Check the strategy-specific source retention policy and that pruning excludes active jobs. Delivery checks need duplicate notifications, lost publication marks, full stream loss, result-commit/ACK gaps, Redis outage/timeouts, dispatcher progress while inference is saturated, and clean shutdown. Run integration tests against real PostgreSQL and Redis, with `TEST_DATABASE_URL` and `REDIS_URL` set. Separately check an image and multi-page PDF against a protocol fixture and the actual inference deployment; queue correctness does not establish OCR accuracy.
