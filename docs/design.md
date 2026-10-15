# OCR service design and operating decisions

This implementation follows the user's scope: a Docker-first internal OCR service, Rust Axum API, Rust consumer, Redis Streams delivery, PostgreSQL persistence, and an external vLLM inference endpoint. It does not create an API gateway, Kubernetes resources, or cloud infrastructure.

## What changes from the course

| Concern | Course implementation | This version | Reason |
| --- | --- | --- | --- |
| HTTP boundary | Rust producer called a gateway | Internal Axum OCR API | Other services need a direct OCR contract; gateway policy is a separate concern |
| Consumer | Python and GLM-OCR SDK | Rust leased worker and SDK 0.1.4 formatter port | Rust owns scheduling, page/crop preparation, layout policy, recognition calls, formatting, and persistence |
| Queue | Destructive Redis list pops | Redis Streams consumer group backed by a recurring PostgreSQL outbox | Retains Redis-driven delivery while making publication gaps, redelivery, and interrupted work recoverable |
| Input storage | Base64 in Redis memory | Limited binary PostgreSQL column | Avoid shared upload mounts and object/database orphan coordination initially |
| Layout | Local GPU PP-DocLayoutV3 through SDK | Pinned ONNX PP-DocLayoutV3, Rust preprocessing/postprocessing and mask geometry | Preserves the course's detection, class policy, learned order, and crop contract without a Python consumer |
| PDF | PDFium through SDK | Timed Poppler CropBox rendering, one page at a time | Bounds subprocess work and raster memory; rendering is a disclosed fidelity difference |
| Batch collector | Up to 4 documents over 100ms | Bounded documents and concurrent region requests | Native detector processes one page; external vLLM schedules recognition requests |
| Model serving | vLLM/Qwen Python/CUDA runtime | Existing external inference server | Confirmed application boundary; native Rust GPU inference is outside this implementation |
| Deployment | AKS/GKE/KEDA/APIM | Docker Compose | Explicit current deployment scope |

The comparison describes actual checked source, not course performance claims. See the [course audit](course-audit.md) for file and line evidence. Root/week 5 and week 4 are different pipelines; week 4 model-specific bounding-box parsing is deliberately absent.

## Submission and durable state

The caller sends one multipart `file`. The API reads chunks within its file/request deadline and allocation bounds, detects PDF/PNG/JPEG/WebP from magic bytes, hashes the original bytes, and inserts a queued row. A PostgreSQL trigger creates the delivery outbox row in the same transaction. Input bytes, idempotency key, job ID, state, and the durable publication record commit together; decoder validation happens in the worker. The API can accept jobs while Redis is unavailable, subject to the pending-job limit.

An independent dispatcher task in the Rust worker leases bounded outbox batches with `FOR UPDATE SKIP LOCKED`, publishes only `job_id` through Redis `XADD`, and records delivery with an expiring dispatch token. It keeps publishing while inference slots are occupied. A publish that succeeds before its database mark is lost can be repeated safely. Eligible outbox rows recur after `REDIS_REDISPATCH_SECONDS` (default 30); live OCR leases suppress republication. The outbox remains until terminal completion, so it also repairs loss of an entire Redis stream after a previously recorded publish. See [Redis Streams + PostgreSQL](redis-postgres.md) for the failure matrix and migration details.

The public API uses bounded enqueue. Brief admission transactions share a PostgreSQL advisory lock, recheck idempotency after obtaining it, count nonterminal jobs, and reject new jobs above the configured limit. Worker claims and result writes do not take the enqueue lock. Replay deduplication compares content hash plus media type, preserving the original filename. The single API credential defines one shared ownership/idempotency scope. Retention eventually releases the key.

All worker instances sharing the database should use the same Redis stream/group, strategy, attempt, lease, inference, and retention settings. Course results record the detector ID, upstream revision and ONNX SHA-256, model alias, prompt/formatter/profile versions, rendering and recognition-preprocessing metadata, and page/crop bounds. A recognition model alias is not an immutable weights digest; use versioned served names and keep the serving checkpoint revision in deployment records.

## Claim, heartbeat, recovery

Each Rust consumer has a process-specific UUID in one dedicated Redis consumer group. `XREADGROUP` reads only available processing capacity; cursor-based `XAUTOCLAIM` recovers old pending notifications. Before OCR, a conditional PostgreSQL claim by the notified job ID verifies due time, state, attempt budget, and lease expiry. A successful claim increments attempts, assigns a fresh UUID token, and creates a lease from the database clock. Expired final attempts become failed jobs. The short database operation finishes before expensive processing starts. Duplicates cannot claim another attempt while an existing owner has a live lease.

While a document is inspected, rendered, normalized, and transcribed, an independent async heartbeat renews its lease roughly every lease/3 seconds. A failed heartbeat cancels local processing. Renewals, success, and failure updates require processing state, matching token, and an unexpired lease. A new owner supersedes an old token; completion can be persisted once even though inference can execute more than once.

The worker commits a fenced result or failure in PostgreSQL before acknowledging its Redis notification. A Lua command performs `XACK` and `XDEL` atomically. If acknowledgment fails after commit, redelivery observes the durable state and cleans up the notification. Use one dedicated group per stream because acknowledged entries are deleted. Notifications contain no original bytes or OCR results.

Retries return jobs to `queued` with a future due time and reset the outbox in the same transaction; terminal transitions remove the outbox row. Durable due times survive process restarts. Each document fails independently. A permanent error stops immediately; a transient error consumes the finite job attempt budget. Redis commands have bounded deadlines and separate exponential backoff, independent of OCR heartbeats. Redis failure pauses new delivery while PostgreSQL preserves work; there is no direct PostgreSQL consumption fallback. Delivery and inference are at least once, with fenced persistence rather than exactly-once execution. No manual retry/cancel endpoint is implemented; an intentional new-key submission creates a new job.

## Processing and quality boundary

`OCR_STRATEGY=course_layout_vlm` is the default. Input JPEG/PNG/WebP headers are inspected before decoding. Width, height, pixel count, decoder allocations, and concurrent blocking decoders are bounded. EXIF rotation/mirroring is applied before layout detection and crop generation; transparent pixels become white. The detector and crops share the same upright RGB raster, so coordinates refer to the page actually processed.

PDFs require Poppler `pdfinfo` and `pdftoppm`. The worker checks page count/encryption and CropBox dimensions, renders one page at a time with a bounded effective DPI, invokes subprocesses without a shell or user filenames, limits captured output, kills timed-out children, and removes temporary files. Defaults are 200 DPI, a 3,500-pixel page edge, and 40 million pixels. Recognition crops have a separate 2,000-pixel edge bound. Poppler is a native external utility; orchestration remains Rust. The Docker worker also has a memory limit for native rendering and inference work.

Each upright page is passed to the Rust `layout-detect` helper. It verifies the pinned model hash, uses the ONNX Runtime C library, and runs PP-DocLayoutV3 at its fixed 800×800 tensor size. Rust reproduces detector resize/normalization, score filtering, suppression, course class policies, learned V3 query order, and mask-derived polygons. The helper runs as a subprocess so timeout or lease loss can terminate and reap native inference. Its output dimensions, classes, tasks, geometry, counts, and JSON byte size are validated by the worker.

Text, Table, and Formula regions use the course task prompts and cropped, polygon-masked JPEGs. Recognition requests use bounded concurrency; results are restored to learned reading order before the SDK 0.1.4 formatter port maps labels, cleans content, handles headings/lists/formulas, and merges the supported text/formula cases. Default charts/images are skipped for recognition but retained as Markdown position placeholders. Headers, footers, and other abandoned course classes remain in the page's `discarded` audit data. `OCR_RECOGNIZE_IMAGES=true` sends outermost detected chart/image containers to the Text prompt and records the enhanced `course-root-sdk-0.1.4+image-text` profile. The default profile is `course-root-sdk-0.1.4`.

The image override uses `image_ocr_policy.name: "outermost_containers_v1"`. An image/chart region whose normalized bounding box is at least 98% covered by a larger image/chart container keeps its placeholder and receives no separate recognition call. Equal-area boxes choose the earlier detection index to prevent mutual skipping. These pixels remain visible in the outer container crop; Text/Table/Formula regions are unaffected. Each page lists skipped original detection indices, in sorted order, in `image_ocr_skipped_nested_regions`. Their raw content is null and `task_type` remains `skip`, so `coverage.skipped_regions` can be nonzero with image OCR enabled. Disabled image OCR records `course_skip_all`, preserves the course skip policy, and leaves the nested-index list empty. Both policies record `containment_threshold: 0.98`.

The earlier enhanced implementation prompted every image crop separately and produced invented QR/fingerprint narrative from nested regions despite correctly reading critical card fields. That output is a historical failure case. The current hierarchy avoids those separate nested prompts; it does not establish that the outer-container response is accurate. See the [layout report](layout-e2e-report.md) for evidence from each run and check policy metadata when comparing historical results.

`TABLE_CROP_PADDING_PIXELS` optionally adds a white border to Table crops after the unchanged course crop/resize and before JPEG encoding. Its default is 0; the macOS example uses 112 pixels on each side. Detection geometry and Text/Formula crops are unchanged. Results record the exact value in `recognition_preprocessing`, with `padding_color: "white"` and `padding_stage: "after_course_crop_resize_before_jpeg"`. Nonzero padding selects the `+table-padding` profile suffix; enabling image OCR as well selects `course-root-sdk-0.1.4+image-text+table-padding`. The [layout report](layout-e2e-report.md) covers the strict Table hallucination/truncation failure and isolated padding comparison. An improved crop result is not proof of complete-document or general table accuracy.

The vLLM adapter reuses a client, sends base64 JPEG data URLs to the external Qwen3.5-4B server, verifies TLS by default, disables redirects, has connect/request deadlines, and bounds each completion response to 2 MiB. Region prompts use deterministic generation settings and disable thinking. Fixed prompts ask for transcription and treat document instructions as data; model compliance and transcription accuracy still need evaluation. Results are untrusted text and should not be interpreted as application instructions or inserted as unsanitized HTML by callers. A document has a 4 MiB recognized-text budget and 16 MiB serialized-result budget.

Results provide formatted `layout` and page `regions`, pre-format `raw_regions`, original `detections`, `discarded` regions, coverage counts, and warnings. Crop `bbox_2d` and polygon coordinates are quantized to the normalized 0–1000 upright page frame. Detection `bbox` values are raw model boxes in page pixels and can extend outside the page. `page_number` is one-based; SDK Markdown image references use zero-based pages. Formatted region indices can change after cleanup/merges, while `reading_order` preserves detector rank. The page `empty` flag describes generated Markdown, and coverage separately records empty model responses. Neither establishes a blank source page.

The [layout pipeline report](layout-e2e-report.md) records comparison and recognition evidence. Exact retained labels, normalized boxes, polygons, and ordering on comparison pages are a contract check, not strict numerical equivalence of all model tensors or an OCR accuracy guarantee. Float32 ties can change ranks of queries that are not retained. Poppler and the course's PDFium produce different page rasters; Rust crop JPEG encoding/resizing can also differ from SDK libraries. Formatter hyphenation uses SDK 0.1.4's dependency-free fallback, and repetition cleanup has a bounded work budget. Dense tables, small glyphs, handwriting, and image text still need representative quality evaluation. The service rejects missing/null completion content and non-`stop` finish reasons, including token truncation.

The explicit `full_page_vlm` strategy retains the earlier one-request-per-page implementation and is selected by `compose.demo.yaml`. Its [real-model report](real-e2e-report.md) remains a baseline. The optional persisted `vi_rental_contract_v1` profile now adds scoped typed fields, exact source evidence, native/OCR alternatives, normalization and cross-source conflicts. Its machine decision always returns `needs_review`. A separately privileged durable correction/approval API is implemented; a reviewer UI and general extraction remain future work. See the [runtime extraction contract](structured-extraction.md).

## Docker operation

The image uses digest-pinned builder/runtime bases and packages API/worker/detector binaries, demo fixture/client binaries, Poppler, and checksum-pinned ONNX Runtime 1.23.2 libraries for Linux arm64/amd64. Application containers run as a nonroot user with a read-only root filesystem, dropped capabilities, no new privileges, and a writable bounded `/tmp`. The Compose worker memory limit is 2 GiB. Worker concurrency/pixel settings and temporary capacity must be sized together; raising them can cause an out-of-memory kill and recovery through leases.

Run `bash scripts/setup-layout-model.sh` before starting the course worker; setup requires `uv` and Python 3.12. It validates/reuses the exported artifact or installs pinned export dependencies in an isolated environment, downloads the revision-pinned public detector, and exports ONNX. Python is used for export and offline QA only. Setup writes the actual hash and model directory to `.env` while preserving other settings. The default artifact is `artifacts/models/pp-doclayout-v3/model.onnx`; `compose.yaml` mounts its directory read-only at `/models/layout` and supplies the pinned hash. Use `--force` to re-export, `LAYOUT_MODEL_DIR` to change the artifact directory, and `LAYOUT_EXPORT_RUNTIME` to change the export environment.

`compose.yaml` starts the application, PostgreSQL, and digest-pinned Redis 8.2 Alpine, with named database/Redis volumes. Redis enables AOF with `appendfsync everysec`, a 128 MiB `noeviction` cache limit, and a 256 MiB container limit. The recurring PostgreSQL outbox recovers job delivery independently of recent AOF-write loss; database loss still requires backup/restore. Set the example's random Redis password and matching host/container URLs before deployment. Published API/database/Redis ports are loopback-only; Docker services can call `http://api:8080` on the same Compose network.

`compose.demo.yaml` selects the explicit full-page strategy, removes the model mount, and redirects recognition to a Rust canned fixture; delivery still uses Redis and PostgreSQL. Inference stays external for actual OCR. On Apple Silicon, [native vLLM Metal](local-qwen-macos.md) serves Qwen3.5-4B on the host while Linux Docker runs the application/layout/Redis/database.

Keep the model serving instance running for this version. No scale-to-zero machinery is implemented. Increasing workers increases model request pressure; tune concurrency from model latency/throughput rather than copying the course's 512-region setting.

Both Rust executables apply the embedded schema migration on startup; SQLx coordinates migration concurrency. For stricter production permissions, run migrations as a separate release step and use a runtime role with only application privileges; that rollout split is a follow-up. Do not edit applied migration files after deploying them: add a new migration.

Successful course and extraction jobs retain original document bytes in PostgreSQL until pruning, with `source.sha256`, `source.media_type`, `source.original_retained`, and `source.retention_days` in the result. No public download/crop endpoint is implemented. Successful legacy full-page jobs without extraction clear the bytes; failed inputs remain until pruning. Workers prune terminal rows older than `RETENTION_DAYS` (default 7 days) in 1,000-row batches, promptly continuing full batches and checking hourly after draining them. The pending-job limit does not bound retained terminal inputs/results or PostgreSQL WAL/TOAST growth. A high-volume installation needs deliberate capacity and maintenance. A named volume is persistence, not a backup; use database backups and restore drills for important sources/results.

Useful operational queries:

```sql
SELECT status, count(*), min(created_at) AS oldest FROM jobs GROUP BY status;
SELECT count(*) FROM jobs WHERE status = 'processing'
  AND lease_expires_at <= clock_timestamp();
SELECT pg_size_pretty(pg_total_relation_size('jobs'));
SELECT count(*), min(next_dispatch_at) AS oldest_due FROM job_outbox
  WHERE next_dispatch_at <= clock_timestamp();
```

Liveness/readiness are API process/PostgreSQL checks. Readiness does not validate Redis delivery, inference, or worker progress. Worker JSON logs report dispatch/read failures, claims, attempts, processing failures, renewal failures, and fenced commits without credential-bearing Redis URLs. Separate protected Prometheus endpoints expose bounded process/stage measurements; see [observability](observability.md). Database/Redis backlog collectors and alert deployment remain operational work. Track oldest queued age, due outbox age, Redis pending notifications and memory, retries, expired leases, model error/latency, database size, and terminal retention. Integration tests need `TEST_DATABASE_URL` and `TEST_REDIS_URL`; the OOM regression additionally requires a separate disposable Redis instance via `TEST_REDIS_OOM_URL` because it changes that instance's memory configuration.

## Remaining work

1. Evaluate the real deployed model against a small labeled corpus: Vietnamese/English text, tables, receipts/invoices, phone scans, rotation, blank pages, and multi-column PDFs. Track character/word errors, table fidelity, page completeness, latency, and resource use. Protocol mocks do not substitute for this work.
2. Evaluate the implemented scoped extraction against a labeled corpus, broaden supported templates, and integrate the implemented durable correction/approval API into a trusted reviewer application. Preserve evidence and measure exact field accuracy before introducing automatic acceptance.
3. Move larger inputs/results to object storage when database capacity warrants it, retaining durable job transactions and explicitly managing uploads, object ownership, abandoned objects, and expiry.
4. Deploy alerts and independent database/Redis backlog collection; add caller identity/tenant ownership before serving unrelated tenants.

Primary sources and version observations are in [Rust research notes](rust-research-notes.md). Architecture delivery/browser receipts verify the diagram only; application correctness is checked by unit, real PostgreSQL, Poppler, and Docker smoke tests.
