# Reference course audit

Audited on 14 October 2026; SDK and Rust-port findings updated on 15 October 2026. Reference checkout: `/Users/tonminh-mac/Documents/GitHub/production-ocr-course`. The checkout was read only. It has no `.codegraph/` directory. Source locations below are relative to that checkout; executable source takes precedence over README claims. The course installs `glmocr[selfhosted,layout]==0.1.4`. Its extracted wheel and official 0.1.4 source at `d947dc2dca3d04ee1d48efce3be02b708cbe26c2` have now also been inspected; detector, pipeline, and layout-policy files match by SHA-256. Detailed pins, measurements, and fidelity limits appear in [course-layout-parity.md](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/course-layout-parity.md).

## Which implementation to use as a reference

| Reference | Interface and processing | Meaning for this service |
| --- | --- | --- |
| Root `client_rt_producer`, `client_rt_consumer`, `server` | Async multipart submission and status polling; Redis list/hash; Python GLM-OCR orchestration; local PP-DocLayoutV3; remote Qwen3.5-4B vLLM | Closest application topology. Port the service/worker separation, then replace the unreliable queue behavior. |
| `week4_rust_gateway_deployment/gateway` | Synchronous JSON/base64 `/process` and SSE `/process-stream`; Poppler PDF rasterization; direct Baidu Unlimited-OCR requests; model-specific grounding parser | Separate OCR pipeline, not a Rust equivalent of the GLM-OCR worker. Selected utilities are useful, but prompts, output parsing, batching, and page handling must be reconsidered. |
| `week5_async_architecture_deployment` | Same async producer; Qwen3.5-2B; GLM layout worker; adds a worker heartbeat; FIFO collector | Another snapshot of the async architecture. Heartbeat implementation does not remain fresh during a long parse. |
| `week6_apim_mcp_deployment` | Qwen3.5-2B async pipeline plus APIM/MCP deployment and polling wrapper; collector uses LPOP after the initial BRPOP | External gateway/MCP work is outside the requested service scope. Secondary collection can process newer jobs before older ones. |

Evidence: root `client_rt_producer/src/main.rs:66`, `client_rt_consumer/config.yaml:58`; week 4 `gateway/src/main.rs:65`, `gateway/src/config.rs:25`; week 5 `client_rt_consumer/config.yaml:58`, `client_rt_consumer/worker.py:103`; week 6 `client_rt_consumer/worker.py:126`, `mcp_server/server.py:50`, `mcp_server/server.py:83`.

## Reference course's asynchronous data flow

1. Axum receives a multipart field named `file`, buffers its complete bytes, and base64-encodes them (`client_rt_producer/src/main.rs:114`, `:122`, `:128`). The whole request has a 10 MiB body limit (`:77`).
2. The producer writes status, filename, extension, and base64 payload into `task:<UUID>` with HSET, then independently LPUSHes the UUID into `ocr_tasks` (`:138`, `:151`). HTTP 202 returns the task ID (`:155`).
3. The consumer BRPOPs the first ID, collects more with RPOP up to four documents over a 100 ms window, and waits for the complete batch before claiming more (`client_rt_consumer/worker.py:17`, `:19`, `:105`, `:122`, `:134`). LPUSH/RPOP gives FIFO ordering in root/week 5.
4. Each task is marked processing, its base64 input decoded, and its bytes written under `/dev/shm` using the stored extension (`worker.py:39`, `:42`, `:44`, `:46`).
5. `GlmOcr(..., enable_layout=True)` is initialized once and the entire pipeline is invoked with a list of local file paths through `asyncio.to_thread(ocr_engine.parse, temp_paths)` (`worker.py:23`, `:60`).
6. The worker stores SDK Markdown plus JSON layout in the task hash, clears input on success, and removes temporary files (`worker.py:71`, `:75`, `:79`, `:82`, `:91`). Other services retrieve status/result through `/status/:task_id` (`client_rt_producer/src/main.rs:167`).

There is no separate object store or application database in this implementation. Redis contains the queue, uploaded document, job state, and result. The model-weights PVC is for downloaded inference/layout weights, not uploaded documents (`k8s/gke/apps/deployment-vlm.yml:63`, `k8s/gke/apps/deployment-api.yml:69`).

## What replacing the Python consumer actually requires

The consumer is short because it delegates processing to `glmocr[selfhosted,layout]==0.1.4` (`client_rt_consumer/Dockerfile:17`). The repository config establishes these intended stages and policies:

- PDF/image loading and PDF rasterization at 200 DPI; no configured PDF page limit (`client_rt_consumer/config.yaml:102`, `:127`, `:128`).
- PP-DocLayoutV3 safetensors, explicit CUDA execution, layout batch size four, NMS, and class-specific overlap rules (`config.yaml:169`, `:182`, `:194`, `:206`, `:213`). The 25-class V3 model supplies learned order and mask geometry; replacing it with PP-DocLayout-M or coordinate sorting changes the pipeline.
- Text/table/formula prompts and recognition through `ocr-vlm-service:8000/v1/chat/completions` (`config.yaml:51`, `:63`, `:120`).
- Configured recognition concurrency of 512 and a connection pool of 1,024, with retries for selected transient HTTP statuses (`config.yaml:85`, `:89`, `:93`, `:97`). These are tuning values, not measured requirements for a new service.
- Markdown/JSON formatting and merge rules (`config.yaml:132`, `:136`).
- Charts/images are skipped; headers, footers, numbers, footnotes, and other labels are discarded according to configuration (`config.yaml:263`, `:266`). Those are product policies worth revisiting if callers expect faithful transcription.

The YAML's `use_polygon: false` at `config.yaml:203` does not control the active SDK 0.1.4 parse path: the extra field is accepted but never read, and the streaming pipeline passes polygons to every region crop. A faithful replacement follows source execution rather than the YAML comment. Evidence: [SDK layout config](https://github.com/zai-org/GLM-OCR/blob/d947dc2dca3d04ee1d48efce3be02b708cbe26c2/glmocr/config.py#L186), [active crop path](https://github.com/zai-org/GLM-OCR/blob/d947dc2dca3d04ee1d48efce3be02b708cbe26c2/glmocr/pipeline/pipeline.py#L630).

A Rust worker can own submission, job claims, retries, rasterization, bounded HTTP inference, ordered page assembly, and result persistence without a Python application process. A direct full-page VLM implementation does not reproduce the course's layout regions, crop behavior, learned order, or formatting merely by using the same remote Qwen model.

This repository now implements the course region pipeline in Rust with its own PP-DocLayoutV3 ONNX adapter. Comparison against the pinned SDK passed the retained-region contract on 93 regions across five shared page images, with exact labels, tasks, output sequence, normalized boxes, and polygons. Five layout-resize fixtures matched bytes exactly. The separate strict test for raw 300-query rank metadata failed on three pages because of equal-vote unretained queries; the report remains explicitly failed overall. PDFium-versus-Poppler rendering and recognition-crop JPEG pixels are also remaining differences. These results establish the measured contract, not complete numerical or recognition-text identity; see [parity evidence](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/course-layout-parity.md).

The optional `OCR_RECOGNIZE_IMAGES=true` profile recognizes text in image/chart crops and declares that override in results. The default course profile skips them. `TABLE_CROP_PADDING_PIXELS` is another explicit enhancement: default 0, with 112 in the macOS example. It adds a white border only after the course Table crop/resize and before JPEG encoding; results record its value/stage and a `+table-padding` profile suffix. Neither enhancement changes detector geometry. The [layout end-to-end report](layout-e2e-report.md) distinguishes the strict Table hallucination/truncation failure from isolated padding results and complete-document runs. The suggested [glm-ocr-rs engine audit](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/glm-ocr-rs-audit.md) explains why that PP-DocLayout-M engine was not used as a drop-in replacement.

The model server itself remains Python vLLM (`server/Dockerfile:1`, `server/entrypoint.sh:34`). “Rust API and Rust consumer” and “all components including model inference implemented in Rust” are different delivery scopes. The latter requires separately validating a Rust runtime's model/operator support.

The course's `/dev/shm` handoff avoids persistent disk I/O, but it is not an end-to-end zero-copy transport. Upload buffering, base64 allocation, Redis transfer, decoding, and file writes happen explicitly. Worker and vLLM are separate GPU deployments (`k8s/gke/apps/deployment-api.yml:58`, `k8s/gke/apps/deployment-vlm.yml:18`), so a local RAM file does not share image memory with the remote model server.

## Reliability gaps to address

| Gap | Concrete evidence and consequence | Required replacement behavior |
| --- | --- | --- |
| Destructive dequeue | `worker.py:105`, `:122`: BRPOP/RPOP removes an ID before processing. Crashes can strand queued/processing jobs indefinitely. | Recoverable Redis Streams notifications, durable PostgreSQL leases/heartbeats, recurring outbox repair, and ownership-checked completion. |
| Submission is not atomic | Producer HSET at `main.rs:138` and LPUSH at `:151` are separate operations. Failure after HSET creates a stored job absent from the queue. | Commit input/job/outbox together, publish asynchronously, and acknowledge after fenced result commit. |
| Redis durability absent from manifest | `k8s/gke/apps/redis-deployment.yml:24` defines a Redis container without a data volume or explicit persistence configuration. | Persist committed job/input/result state and document backup/restore assumptions. |
| No job retry or idempotency | UUID is newly generated at producer `main.rs:107`; worker `worker.py:86` immediately records failure. SDK HTTP retries are only request retries. | Caller-scoped idempotency with content/options conflict checks; bounded job attempts; transient versus permanent failure classification. |
| Batch failure coupling | `worker.py:30`, `:89`: one exception fails already-prepared batch members. Later popped IDs may receive no updated state. | Isolate job outcomes; every claim remains recoverable until its terminal transition commits. |
| Missing results silently strand jobs | `worker.py:67` breaks if SDK returns fewer results than inputs. | Validate result count or produce/retry an explicit error for every affected job. |
| Unsafe filename-derived temporary path | Producer accepts extension text at `main.rs:119`; worker incorporates it at `worker.py:44`. | Use generated temporary paths and detected MIME/type; do not derive filesystem paths from caller text. |
| No retention policy | Only successful jobs clear input (`worker.py:82`); producer/worker define no task expiry. | Explicit retention/cleanup and capacity controls for uploaded data and results. |
| Health does not check dependencies | Producer health returns unconditional 200 (`main.rs:213`). | Separate process liveness from database/inference readiness. |
| Heartbeat becomes stale during healthy work | Week 5/6 `worker.py:105` updates a shared heartbeat with ten-second TTL before potentially long processing (`week5 worker.py:140`). | Independent per-worker lease renewal/readiness reporting. |
| Collector ordering differs by snapshot | Week 6 `worker.py:126` uses LPOP; producer still LPUSHes. | Define fair oldest-ready ordering and test it. |
| Request buffering and huge decoded inputs | Producer buffers and base64-encodes input (`main.rs:122`, `:128`); SDK allows unlimited PDF pages (`config.yaml:128`). | Enforce upload bytes, decoded pixels, PDF page count, total work, and concurrency limits. |
| Scale-to-zero inference wake-up is unproven | `k8s/gke/apps/keda-scaler.yml:36` permits zero inference replicas; its scaler watches vLLM waiting-request metrics at `:49`. | Keep initial inference capacity warm or provide a tested wake-up signal that exists without a serving pod. |

Redis calls in the Python loop are synchronous (`worker.py:13`, `:105`). The worker also has no explicit shutdown/lease protocol. No automated tests were found in the inspected producer, consumer, or week 4 gateway source. Treat the course as an instructional reference, not evidence that failure cases are already covered.

## Week 4 utilities and bugs

Week 4 recognizes magic bytes and uses Poppler for PDF inspection/rasterization (`gateway/src/pdf.rs:28`, `:87`, `:139`). Its HTTP client shares a pool, uses timeouts, and its server installs graceful shutdown (`gateway/src/main.rs:53`, `:85`). These are useful patterns.

Do not transplant these behaviors without correction:

- The model defaults to `baidu/Unlimited-OCR`, uses the prompt `<image>document parsing.`, and extracts model-specific `<|det|>` and `<|ref|>` output (`gateway/src/config.rs:25`, `gateway/src/processor.rs:659`, `:152`, `:154`). These are not the Qwen region prompts used by the async pipeline.
- Synchronous subprocesses execute directly inside the async OCR pipeline (`processor.rs:282`, `pdf.rs:87`, `:139`), with no per-process timeout. Use bounded work and timed subprocesses.
- It rasterizes and retains every PDF page before dispatch (`pdf.rs:111`, `:174`), then base64-encodes all batches (`processor.rs:301`). Prefer bounded page processing.
- For a multi-page batch it assigns the entire batch's identical Markdown and boxes to every page (`processor.rs:344`, `:349`, `:351`). Preserve true page-specific results instead.
- Streaming code emits a final complete event after some connection/stream error paths (`processor.rs:549`, `:633`). Completion must reflect whether every required page succeeded.

## Scope for an internal OCR service

Keep the Axum interface that other services call. It is the OCR service's own API; no separate API gateway is needed. Keep an independently scalable Rust worker, a shared processing library, typed contracts/errors/configuration, bounded resource use, structured logs, and graceful shutdown. Deploy internally using private networking and the caller authentication appropriate to the environment.

Do not port APIM/GCP gateway setup, enterprise ingress policies, or the week 6 MCP wrapper unless they become an explicit requirement. The root GKE API manifest uses an internal LoadBalancer (`k8s/gke/networking/service.yml:7`, `:15`); ClusterIP is sufficient for callers in the same cluster.

The service now uses Redis Streams with PostgreSQL persistence. Submission stores original bytes, job/idempotency state, and a trigger-maintained outbox in one PostgreSQL transaction. An independent Rust dispatcher leases outbox rows with `FOR UPDATE SKIP LOCKED` and publishes only job IDs. Consumer-group `XREADGROUP` and `XAUTOCLAIM` deliver notifications; a conditional PostgreSQL claim by job ID controls eligibility, attempts, and expiring ownership. Heartbeats and terminal writes require the same unexpired token. After the result/failure commits, atomic Lua `XACK` plus `XDEL` cleans up the notification.

The outbox recurs until the job becomes terminal, suppressing republication while its processing lease is live. It repairs the database-to-Redis publication gap and complete stream loss, including previously recorded deliveries. Redis outages pause new processing without discarding durable jobs; there is no automatic direct-PostgreSQL consumption fallback. A crash after inference can still repeat model execution: this is at-least-once delivery/inference with fenced persistence. Successful course jobs retain original sources until terminal pruning; successful legacy full-page jobs clear them. Large document storage, backups, and cleanup remain capacity decisions. See [Redis + PostgreSQL operation](redis-postgres.md) and [service design](design.md) for the implemented boundary.
