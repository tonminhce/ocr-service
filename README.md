# Rust OCR service

An internal OCR service that another service can call directly. The Axum API and consumer worker are Rust. Redis Streams distributes job notifications; PostgreSQL stores uploads, job state, results and a transactional outbox. The worker calls a separately hosted, vision-capable vLLM server. Docker Compose is the deployment target.

The default `course_layout_vlm` strategy implements the course layout pipeline in Rust: render an upright page, detect regions with a revision-pinned PP-DocLayoutV3 ONNX model, recover learned reading order and mask polygons, recognize Text/Table/Formula crops through Qwen3.5-4B on external vLLM, then apply the GLM-OCR SDK 0.1.4 formatter port to produce Markdown and layout JSON. The deployed consumer and detector use Rust, the ONNX Runtime C library, and Poppler. Python is used only for model export and development comparison tools.

## Start actual OCR with Docker

For a fresh setup, create the local environment file:

```sh
cp .env.example .env
```

Set `OCR_API_TOKEN` in `.env` to a random service token with at least 16 printable characters and `REDIS_PASSWORD` to a random hex password. Set `COMPOSE_VLLM_BASE_URL` to a reachable inference endpoint including `/v1`, `VLLM_MODEL` to its exact served name, and `VLLM_API_KEY` if required. Docker Desktop can reach a host server at `http://host.docker.internal:8000/v1`.

Prepare the layout model, then start the application:

```sh
bash scripts/setup-layout-model.sh
docker compose up --build -d --wait
```

The setup script requires [uv](https://docs.astral.sh/uv/getting-started/installation/) and Python 3.12. It validates and reuses an existing model and manifest, or exports the pinned public model using an isolated environment with pinned dependencies. The default export environment is `~/.local/share/ocr-service/layout-export-v1`; override it with `LAYOUT_EXPORT_RUNTIME`. Use `bash scripts/setup-layout-model.sh --force` to rebuild. The model lives at `artifacts/models/pp-doclayout-v3/model.onnx`; `LAYOUT_MODEL_DIR` changes that location. Setup updates only the layout hash and model directory in `.env`, preserving credentials. An export can produce a different hash from the examples, so use the value written by setup. The worker verifies `LAYOUT_MODEL_SHA256` and the Compose mount is read-only at `/models/layout/model.onnx`.

vLLM is external to this Compose stack. The worker sends base64 crop images through the OpenAI-compatible Chat Completions API. Confirm the served name through `/v1/models` and use the model's supported vision chat template. [vLLM server documentation](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/).

On Apple Silicon, use the macOS profile and native vLLM Metal for Qwen3.5-4B. For a fresh setup:

```sh
cp .env.macos.example .env
# Replace the API, model and Redis credential placeholders, then:
bash scripts/setup-layout-model.sh
./scripts/install-vllm-metal-macos.sh
# Terminal 1:
./scripts/serve-vllm-metal-macos.sh
# Terminal 2:
docker compose up --build -d --wait
```

Preserve an existing `.env` containing your settings. The Linux Docker containers run the API, worker, layout detector, and PostgreSQL; the host Mac runs the Metal model server. See [Qwen3.5-4B on macOS](docs/local-qwen-macos.md) for host prerequisites and model budgets.

The API listens at `http://localhost:8080`. PostgreSQL and Redis have named persistent volumes; Redis uses AOF with a one-second fsync policy. Published API/database/Redis ports bind to loopback. Stop the application while preserving data with:

```sh
docker compose down
```

[Hướng dẫn chạy trên máy hiện tại và gọi OCR hợp đồng](docs/run-production-local.md).

For a new deployment with separate DB identities and file-mounted secrets, use
the standalone [A03 operations guide](docs/a03-operations.md) and
`compose.production.yaml`. API/worker binaries check the exact schema at startup;
the one-shot migrator owns DDL. The local/demo Compose file explicitly runs
`schema migrate-dev` using its existing development identity. See the
[A03 execution trace](docs/a03-execution-trace.md) for restricted-role Docker
verification. [A04 hold controls](docs/retention-operations.md) and their
[execution trace](docs/a04-execution-trace.md) cover bounded previews,
independent holds and source preservation. New workers have no automatic pruning
loop. [A05 backup operations](docs/backup-operations.md) and the
[detailed restore trace](docs/a05-execution-trace.md) cover encrypted logical
backups, offline verification and fresh-volume recovery with empty Redis.
the [A06a trace](docs/a06a-execution-trace.md) verifies native subprocess
environment/command policy, bounded process-group cleanup and scratch lifetimes
on Mac and Linux ARM64. The [A06b/O01 trace](docs/a06b-execution-trace.md)
records isolated parser IPC/container fault testing and the worker's bounded
cached PostgreSQL/Redis observer. Neither trace is a production cutover or
capacity/accuracy certification; see the [sanitized verification record](docs/research/a06b-o01-verification-2026-10-15.json). Policy, destructive apply, live adoption,
off-host backup and production restore activation remain gated.

## CPU protocol demo

The demo explicitly selects the legacy `full_page_vlm` strategy and removes the layout-model mount. It starts a Rust mock endpoint that returns **canned text**, validating rendering, queueing, request format, and retrieval without a layout model or GPU. It does not demonstrate OCR accuracy. After stopping the actual OCR stack:

```sh
docker compose -f compose.yaml -f compose.demo.yaml up --build -d --wait
docker compose -f compose.yaml -f compose.demo.yaml exec -T api make-fixtures /tmp/ocr-fixtures
docker compose -f compose.yaml -f compose.demo.yaml exec -T -e EXPECTED_PAGES=2 api service-client /tmp/ocr-fixtures/fixture.pdf

set -a
. ./.env
set +a
./scripts/smoke.sh

docker compose -f compose.yaml -f compose.demo.yaml down
```

## Call it from another service

The [layout pipeline report](docs/layout-e2e-report.md) records detector comparisons and real-model evidence. The earlier [full-page report](docs/real-e2e-report.md) remains a baseline: its synthetic-image hallucination is evidence about that earlier strategy, not a layout-pipeline result.

```sh
curl --fail-with-body http://localhost:8080/v1/ocr/jobs \
  -H "Authorization: Bearer $OCR_API_TOKEN" \
  -H 'Idempotency-Key: invoice-2026-00042' \
  -F 'file=@/absolute/path/invoice.pdf'
```

The response is `202 Accepted`, with a job `id`, current `status`, and a `Location` header. Poll the returned ID:

```sh
curl --fail-with-body http://localhost:8080/v1/ocr/jobs/JOB_UUID/status \
  -H "Authorization: Bearer $OCR_API_TOKEN"
```

After success, fetch `/v1/ocr/jobs/JOB_UUID/business` for compact structured fields,
or `/v1/ocr/jobs/JOB_UUID/artifacts/layout` and `/artifacts/markdown` for the original
OCR outputs. `/artifacts/source` returns the retained original upload. The original
`GET /v1/ocr/jobs/JOB_UUID` remains compatible and includes the full result;
polling `/status` avoids repeatedly reading and transferring that large JSON.
See the [business/artifact/review API](docs/business-review.md).

States are `queued`, `processing`, `succeeded`, and `failed`. The default successful result has `strategy: "course_layout_vlm"`, pinned layout-model metadata, formatter/profile versions, ordered `pages`, combined `markdown`, and `layout` arrays. Each page includes formatted `regions`, original `detections`, `discarded` course-policy regions, `raw_regions` before formatting, coverage counts, and warnings. `bbox_2d` and polygons use normalized 0–1000 coordinates on the upright rendered page; page width/height are pixels. Public page numbers start at 1; Markdown image references start at 0.

By default, `OCR_RECOGNIZE_IMAGES=false` preserves the course policy: detected charts/images become positional Markdown placeholders and receive no recognition call. Setting it to `true` sends outermost detected image containers to the Text prompt and records the enhanced `course-root-sdk-0.1.4+image-text` profile with `image_ocr_override: true`. Nested image regions covered by a larger image are kept as placeholders and skipped for separate recognition. The result declares `image_ocr_policy.name: "outermost_containers_v1"` and the original skipped indices per page. This avoids prompting the model separately with portraits, QR codes or fingerprints already visible inside a full-card crop. It does not establish image-text accuracy. Headers, footers, and other abandoned course classes remain in `discarded`.

`TABLE_CROP_PADDING_PIXELS=0` keeps the strict course crop. The macOS example explicitly uses 112: this adds a white margin to table recognition images after course cropping/resizing, before JPEG encoding. It mitigated repeated invented rows on one very thin header in the local Qwen backend; it does not establish general table accuracy. The result records this extension in `layout_profile` and `recognition_preprocessing`; measured page-space boxes/polygons stay unchanged. See the [layout report](docs/layout-e2e-report.md).

The `empty` flag means that the generated page Markdown is empty; it does not prove the source is blank. Coverage distinguishes skipped/abandoned regions and empty model responses. Failed jobs expose a sanitized processing error. Public results contain no original file bytes, credentials, or raw upstream error bodies.

Reuse the same `Idempotency-Key` when retrying a submission after a network failure. Matching bytes, media type and extraction profile return the original job; a different fingerprint with the same key returns `409`. Keys remain valid until their terminal job is pruned. Omitting a key creates a new job on every submission. Filenames are display metadata and do not select the decoder.

The shared bearer credential authorizes access to all jobs. This is a trusted internal service contract, without tenant isolation. If multiple unrelated tenants use the service, add tenant-scoped credentials, ownership checks, and tenant-scoped idempotency before exposing it to them.

[OpenAPI contract](docs/openapi.yaml) · [Rust caller example](crates/ocr-api/examples/service_client.rs)

## Extracting business fields

Submit `extraction_profile=vi_rental_contract_v1` to run the Rust extraction stage after OCR. The profile maps explicit Vietnamese rental-contract anchors to party names, identifiers, dates, addresses, payment accounts, rent and deposit. It links ID-card observations only through an exact unambiguous identifier, keeps conflicting observations, and returns source blocks, hashes, exact UTF-8 quotes and whole-block geometry. Omitting the profile preserves OCR-only behavior.

```sh
curl --fail-with-body http://localhost:8080/v1/ocr/jobs \
  -H "Authorization: Bearer $OCR_API_TOKEN" \
  -H "Idempotency-Key: rental-fields-001" \
  -F 'extraction_profile=vi_rental_contract_v1' \
  -F 'file=@/absolute/path/contract.pdf'
```

Poll the returned job URL as usual. `result.layout` and `result.markdown` retain raw OCR. `result.post_processing` contains parallel OCR/native PDF evidence, quality alerts, text-layer alternatives and a reviewer Markdown projection. `result.extraction.parties[].fields` contains typed fields and unresolved conflicts; `result.extraction.fields` contains rent/deposit/property fields. Dates use ISO format, IDs remain strings, and money uses decimal strings with currency/period.

**Every machine extraction returns `decision=needs_review`.** The explicit-anchor rules cover a scoped template family, not arbitrary documents. Native text screening is not pixel verification. Detection scores are not OCR/field confidence. Literal `#`, missing dates and disagreements are preserved; no automatic correction or business acceptance occurs. Unknown templates, ambiguous identity links and processing budgets produce review issues. A separately privileged, revisioned [review API](docs/business-review.md) records corrections/approval without changing raw evidence; no reviewer UI is included. `succeeded` means execution completed.

The [implementation plan](docs/production-implementation-plan.md) tracks the current
delivery. [Protected metrics](docs/observability.md), a [warm detector pool](docs/warm-layout.md),
[region checkpoints and recovery](docs/checkpoints-recovery.md), and the Rust
[evaluation/benchmark CLI](docs/evaluation.md) provide measurement and operational
controls. They do not establish extraction accuracy on an unlabeled corpus.

The [implemented extraction contract and limits](docs/structured-extraction.md) and [runtime JSON Schema](schemas/rental-extraction-v1.schema.json) describe this profile. The broader [field-extraction research](docs/production-field-extraction.md), older proposed envelope and interactive design diagram include future work beyond this implementation.

## Folder structure

```text
crates/
  ocr-core/
    src/domain.rs          job and lease types
    src/store.rs           durable scheduling and persistence
    migrations/            PostgreSQL schema
  ocr-extraction/
    src/                   scoped typed KIE, source evidence and conflict rules
    tests/                 synthetic role, normalization, linkage and budget checks
    tests/                 concurrent queue and lease tests
  ocr-api/
    src/main.rs            bootstrap and shutdown
    src/config.rs          validated API environment
    src/lib.rs             router and shared state
    src/auth.rs            service authentication
    src/error.rs           JSON error contract
    src/routes/jobs.rs     multipart submission and polling
    examples/              service-to-service client
  ocr-worker/
    src/main.rs            bootstrap and shutdown
    src/settings.rs        validated worker environment
    src/worker.rs          claims, heartbeats, retries, draining
    src/queue.rs           bounded Redis Streams transport
    src/preparation.rs     worker-side parser IPC and page preparation
    src/parser_client.rs   bounded Unix IPC client and response validation
    src/parser_service.rs  isolated PDF/image parsing service
    src/observer.rs        cached PostgreSQL/Redis operational snapshots
    src/inference.rs       reusable vLLM protocol adapter
    src/layout_runner.rs   bounded native detector subprocess
    src/formatting.rs      SDK 0.1.4 formatter port
    examples/              Rust demo server and file fixtures
  ocr-layout/
    src/model.rs           pinned ONNX inference
    src/preprocessing.rs   detector resize/normalization
    src/postprocess.rs     filtering, class policy, learned order
    src/polygon.rs         mask geometry
  ocr-eval/
    src/                   bounded corpus scorer and authenticated API load runner
tools/                     one-time Python export and offline parity QA
docs/                      reference audit, research, decisions, diagram
Dockerfile                 builds all Rust executables
compose.yaml               local API, worker, Redis and PostgreSQL
compose.demo.yaml          optional CPU-only inference fixture
```

Axum does not prescribe a production directory tree. This workspace separates HTTP, durable job semantics, and processing so the API and worker can deploy and scale independently while sharing one domain model. Typed state owns cloneable database/client handles; no global mutex serializes normal database or HTTP work. [Axum State](https://docs.rs/axum/0.8.9/axum/extract/struct.State.html), [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html).

## Reliability and limits

PostgreSQL stores uploaded bytes, the queued job and its outbox in one transaction. A Rust dispatcher publishes job IDs to Redis Streams independently of inference slots. Consumers receive through a consumer group, conditionally claim each ID in PostgreSQL, then perform inference outside the transaction. Renewable SQL leases recover interrupted work; tokens fence heartbeats and terminal writes. The worker commits the database result before `XACK`/`XDEL`. Recurring outbox publication repairs Redis loss and database-to-queue gaps; duplicate delivery is expected. Model execution is at least once. See [Redis + PostgreSQL failure behavior](docs/redis-postgres.md).

The API defaults to 10 MiB files, 8 concurrent uploads, and 1,000 queued/processing jobs. Matching idempotent replays still work when the queue is full; new submissions receive `429`. Byte storage is intentionally simple: 1,000 maximum-size inputs alone can occupy about 9.8 GiB before database overhead. Successful course and extraction jobs retain the original uploaded bytes. Results record `source.sha256`, `media_type`, `original_retained`, and `retention_days`; the authenticated source artifact endpoint downloads retained bytes. Successful legacy full-page jobs without extraction clear the bytes unless an active hold requires preservation. Failed inputs and results remain stored. No terminal cleanup is currently enabled; `retention_days` is compatibility metadata. Larger workloads should use object storage with a deliberate upload/cleanup contract.

Workers default to 2 concurrent documents, 20 PDF pages, and 40 million input pixels. Course pages use Poppler CropBox rendering at up to 200 DPI and a 3,500-pixel longest edge; recognition crops use a separate 2,000-pixel edge bound. Layout detection has a 60-second deadline and a bounded warm helper pool. Region recognition defaults to 4 in-flight calls **across the worker process**, with separate document/CPU limits. The macOS example reduces document and region concurrency to 1, matching its single-sequence model server. Multiple worker replicas multiply this bound; choose their combined admission limit against measured model-server capacity. Lowering resolution can reduce OCR fidelity; tune it with representative documents. Corrupt/encrypted PDFs and unsupported formats fail permanently. Network failures, `429`, and upstream `5xx` retry up to 3 total attempts, with 2/4/.../60-second delayed backoff. Missing completion content and truncated generations fail rather than returning incomplete results. Optional lease-fenced region checkpoints reuse successful recognition within a retried job.

The 600-second total job deadline bounds the whole document, even if individual requests have time remaining. New worker binaries no longer delete terminal jobs automatically. A04 adds a bounded retention preview and maintenance-only hold/release operations, with DB guards against deletion and loss of held source. `RETENTION_DAYS` remains a parsed compatibility setting and does not authorize deletion; destructive apply stays disabled. Monitor stored sources/results until explicit policy and restore gates pass. SIGTERM stops new claims and drains active jobs within the worker deadline; a forced shutdown leaves leases recoverable. Set the Compose grace period to cover any increase to the job deadline.

`/health/live` checks the API process; `/health/ready` checks database reachability. Readiness does not guarantee a healthy model server or active consumer. Logs include job IDs and structured events. Protected process metrics and the worker's 30-second cached queue/outbox/lease/Redis observer are implemented; alert thresholds, PostgreSQL pool/storage, model health, representative labeled evaluation, production load targets, database backup activation and tenant isolation remain deployment work. See [observability](docs/observability.md) and [operations and design](docs/design.md). Matching retained detector regions and formatter fixtures does not establish general OCR accuracy. Poppler differs from the course's PDFium renderer, Rust crop JPEG/resizing can differ from SDK libraries, and float32 ties can change ranks of model queries that are not retained.

## Verify and develop

The Docker image uses digest-pinned builder/runtime bases and includes Poppler plus checksum-pinned ONNX Runtime 1.23.2 libraries for Linux arm64/amd64. Native development needs Rust 1.89+, Poppler (`pdfinfo`, `pdftoppm`, `pdftotext`) in fixed installation directories, and a matching native ONNX Runtime library referenced by an absolute regular-file `ORT_DYLIB_PATH`. Poppler search directories are `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin` on Mac, and `/usr/bin`, `/bin`, `/usr/local/bin` on Linux; inherited PATH is ignored. Use an absolute `LAYOUT_BINARY` for a custom native build, or `layout-detect` beside the worker/in a fixed installation directory. Child environment is scrubbed; LD/DYLD overrides and proxy variables are not forwarded. See [A06a compatibility and limits](docs/a06a-execution-trace.md).

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Entirely separate disposable services; no application .env or volumes.
docker compose --env-file /dev/null -f compose.verify.yaml up -d --wait
TEST_REDIS_URL=redis://127.0.0.1:56381/0 TEST_REDIS_OOM_URL=redis://127.0.0.1:56382/0 \
  TEST_DATABASE_URL=postgres://ocr_test_admin:isolated-tests-only@127.0.0.1:55434/ocr_test \
  TEST_ADMIN_DATABASE_URL=postgres://ocr_test_admin:isolated-tests-only@127.0.0.1:55434/ocr_test \
  cargo test --workspace --locked -- --ignored --test-threads=1
docker compose --env-file /dev/null -f compose.verify.yaml down
```

Ignored tests exercise real PostgreSQL/Redis, HTTP integration, restricted-role administration and Poppler rendering. Administration tests create cluster roles and databases; both database URLs must refer to the disposable cluster. The separate Redis instance is required for the OOM regression. The commands above execute all ignored tests explicitly. CI runs both suites. Docker smoke checks use the packaged API/worker binaries.

Focused operational checks are separate from the Rust counts:

```sh
python3 -m unittest discover -s scripts -p test_profile_worker_memory.py
# Uses only an already-present pinned Redis image and disposable network-none containers:
python3 scripts/test_redis_secret_validation.py
```

For native course runs, export `.env`, build `cargo build --locked --bin layout-detect`, set `LAYOUT_BINARY` to the built executable and `ORT_DYLIB_PATH` to your native library. On a fresh local development DB, explicitly run `cargo run --locked -p ocr-core --bin ocr-admin -- schema migrate-dev` before starting `cargo run -p ocr-api` and `cargo run -p ocr-worker` in separate terminals. The examples use a repository-relative `LAYOUT_MODEL_PATH`; update it if setup used a different directory. A host worker needs `VLLM_BASE_URL` to point to a host-accessible inference endpoint. The demo mock has no host port.

## Research

- [Next production roadmap](docs/production-next-roadmap.md): researched plan for Mac first and 1,000+ documents/day, quality/model benchmarks, capacity gates, review and operational gaps; planned work, not implemented features.
- [Detailed execution plan](docs/production-execution-plan.md): work packages, dependencies and release gates; [M1 execution review](docs/m1-execution-review.md) tracks implemented API bounds, enqueue deadlines, v2 evaluation and inference telemetry with actual verification evidence.
- [A03 execution trace](docs/a03-execution-trace.md): one-shot migrator, restricted DB identities, file secrets and packaged Docker permission tests. [A04 execution trace](docs/a04-execution-trace.md) verifies hold/release, bounded metadata previews and source guards. [E02 memory audit](docs/e02-memory-audit.md) identifies the retained helper and documents the verified bounded observer. [A05 execution trace](docs/a05-execution-trace.md) records 227 ordinary +86 explicit integration tests, two full restores and Redis-empty worker recovery. Live cutover, retention policy/apply, off-host/PITR/RPO/RTO/deletion reconciliation and capacity gates remain open.
- [Extraction production readiness and performance](docs/extraction-production-readiness.md): earlier confidence/JSON audit, concurrency, offline diagnostic and primary-source research; its opening update links the subsequently implemented controls.
- [Production field extraction](docs/production-field-extraction.md): Vietnamese design summary, real model failures, source-grounded fields, Rust changes, and evaluation criteria.
- [Reference course audit](docs/course-audit.md): what each course version actually does and what must change.
- [Primary-source Rust research](docs/rust-research-notes.md): Axum structure, PostgreSQL scheduling, Tokio, vLLM, and broker tradeoffs.
- [Design and operating decisions](docs/design.md): boundaries, failure behavior, storage and quality roadmap.
- [Validation evidence](docs/validation.md): checks executed and their practical limits.
- [Suggested glm-ocr-rs audit](docs/glm-ocr-rs-audit.md): build check, V3 differences and verified failure cases.
- [Course layout parity](docs/course-layout-parity.md): pinned source, shared-image comparisons and numerical limits.
- [Redis + PostgreSQL](docs/redis-postgres.md): queue/database responsibilities, outbox, ACK ordering and recovery.
- [Interactive architecture diagram](docs/architecture.html): eleven pipeline components, deployment boundaries, data paths and exploration controls. Generated with the Archify skill and validated separately from application tests.

