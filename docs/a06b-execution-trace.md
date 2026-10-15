# A06b + O01 Execution Trace

Date: 2026-10-15. Status: **implemented and verified in isolated synthetic environments**. No live OCR container, live database, live Redis, model endpoint, credential, or retained user document was changed. This is not a production promotion, penetration test, quality certification, or capacity acceptance.

## Scope and change map

A06b moves PDF inspection/rendering, PDF native-text extraction, and compressed-image decoding out of the worker process into a persistent Rust parser container. A versioned bounded Unix-socket protocol carries document bytes and typed operations; the worker validates every returned RGB frame and native-text record and has no local decode fallback. The existing course render/crop/order/format pipeline and public output schema remain unchanged.

O01 starts a worker-only background collector. It samples PostgreSQL and Redis every 30 seconds, publishes a complete cached snapshot to the existing protected worker exporter, and never performs database/network work during an HTTP request or Prometheus scrape.

Relevant implementation: `crates/ocr-worker/src/parser_protocol.rs`, `parser_client.rs`, `parser_service.rs`, `preparation.rs`, `inference.rs`, `post_processing.rs`, `crates/ocr-core/src/metrics.rs`, `store.rs`, `crates/ocr-worker/src/queue.rs`, and new `observer.rs`.

## A06b boundary and invariants

1. IPC uses `OCRP`, protocol v1, a 20-byte frame header, metadata capped at 64 KiB, request data capped at 64 MiB, and response data capped at 128 MiB. The protocol accepts typed operations only; no caller-supplied filesystem path or executable argument crosses the boundary.
2. A parser container allows at most two active sessions and one decode slot. Per-frame deadlines are bounded. RGB payloads must have exactly checked `width * height * 3` bytes; client-side pixel/edge bounds and native evidence source/page/ID/geometry are revalidated.
3. The parser runs as UID 10002/GID 10001, `network_mode: none`, read-only root, no-new-privileges, all capabilities dropped, init enabled, 2 CPUs, 2 GiB memory, 4 GiB memory+swap, 64 PIDs, and a 512 MiB `/scratch` tmpfs. Docker inspection of the final disposable container confirmed those settings and `healthy` state. Its only persistent mount is the parser socket volume.
4. The socket directory is parser-owned, mode `0710`, group 10001; the socket is `0660`. The worker can traverse/connect but cannot create, remove, or replace the socket. The runtime check printed `socket_dir_not_writable` for the worker identity.
5. Poppler is present only in the parser image; the app image has no `pdfinfo`. The parser image has no worker executable. Parser health is checked before worker startup and each document requires the parser; unavailable helper errors retry/fail through the existing job policy instead of falling back to worker-local parsing.
6. The uploaded source digest is computed worker-side before IPC. The helper response is untrusted; exact RGB shape and native-text provenance are checked again before downstream use. Optional native-text failure remains a warning only where the existing contract permits it; retryable parser failures do not become false OCR success.
7. A06a's bounded Poppler child supervisor remains inside the parser. The ONNX layout helper is still worker-local and is explicitly outside this sandbox boundary. The execution manifest calls container limits `operator_configured_unverified` and does not claim a security attestation.

## A06b test and fault trace

- Parser protocol/client/service unit coverage includes malformed, wrong-version, wrong-direction, truncated and oversized frames; invalid RGB dimensions; forged source/geometry; socket permission/staleness; bounded concurrent sessions; disconnect cleanup; PDF page/RGB/native-text parity; and compressed-image pixel parity.
- The packaged synthetic two-page PDF completed through the isolated production Compose topology. Source SHA-256 was `5d1cf059ad0f473c987717ffac82eefec7699a148be07cf18de85b3cd4ef675c`; output page numbers were `[1,2]`; strategy was `full_page_vlm`; protocol was v1; `fails_closed_when_unavailable=true`.
- The same synthetic two-page job succeeded after parser restart and again after final ARM64 images were rebuilt/recreated. The final packaged output retained the same source digest and page order.
- Scratch exhaustion used a synthetic 512 MiB `/scratch/fill` file. Each client exited 1 after the existing three-attempt policy; the corresponding jobs were failed, not reported successful. The filler was removed from the disposable tmpfs, and a later two-page job succeeded.
- Decoder memory pressure used a generated uniform 6,324 x 6,324 RGBA PNG (39,992,976 pixels, 790,618 compressed bytes). Only the disposable parser cgroup was temporarily reduced from 2 GiB/4 GiB to 128 MiB/128 MiB. Docker emitted three `oom` events per run; the parser restarted three times and returned healthy. The OOM test was run both before and after rebuilding the final image. Limits were restored exactly to 2 GiB/4 GiB before a fresh valid job succeeded on the final image. No live worker/app limits were changed.
- Across the final disposable smoke database: 13 synthetic jobs, 9 succeeded, 4 fault-injection jobs failed, 21 total attempts, and 0 outbox rows. The failures correspond to two scratch-exhaustion runs and two OOM runs; no faulted input was accepted as success.
- Final image IDs: parser `sha256:55636bb0c8882dd5e54b1e8f1d31de62312bf166117a2147d64c01d9f26eaba3`; app `sha256:dbae464e2d546e122240438497a8ae7f13275a583f6464b63e192c5f2af60d3c`; both `linux/arm64`.

The final rebuilt parser image was independently health-checked after OOM, restored to its original limits, and ran a fresh two-page success.

## O01 observer trace

The worker starts one cancellation-aware collector task. Each cycle runs one aggregate PostgreSQL query (3-second deadline) and the existing timed Redis client commands concurrently, with a 5-second total collector deadline and 30-second interval. PostgreSQL reports queued/processing/recent failed counts, eligible/delayed queued counts, oldest queued age, age of the latest processing-row update, due outbox count/age excluding an active dispatch lease, and expired worker leases. Redis reports Stream PEL count and `INFO memory` used bytes.

A snapshot is published only if both sources succeed. Failed collection preserves the last good values, sets `ocr_operational_collector_error=1`, and leaves the last-success Unix timestamp unchanged. Error detail strings, job IDs, file data, credentials, and raw URLs never become labels or metric values. Operational series are enabled only in the worker process; API metrics remain process-only.

Verification:

- Metrics unit test: fixed cardinality, no ID-like labels, disabled-by-default series, last-good values retained after collector failure.
- Disposable PostgreSQL integration test: synthetic queued, delayed, processing, failed, and expired-lease rows produced expected aggregates.
- The PostgreSQL fixture includes both a live worker lease and an expired worker lease. Its exact due-outbox count proves the live lease is excluded and the expired lease remains claimable, matching `claim_outbox` eligibility.
- Disposable Redis integration test: real pending stream entry yielded PEL count 1 and `INFO memory` returned a positive byte count.
- Packaged protected worker scrape returned a fresh timestamp, `collector_error=0`, queued/processing/outbox/PEL/expired-lease values at 0, four recent synthetic failures, and Redis memory about 1.2 MB. The scrape did not trigger SQL/Redis reads.

O01 does not yet expose PostgreSQL pool saturation/storage, Redis eviction rate, host CPU/RSS, or model-server health. No alert thresholds/routing are selected. Last-progress is a coarse age over `jobs.updated_at`, not a guarantee that useful OCR work is advancing.

## Final gates and remaining review

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo test --workspace --locked`: 174 passed, 0 failed, 16 ignored.
- Full explicitly ignored integration matrix on isolated `compose.verify.yaml` services, serialized across binaries/tests: 88 passed, 0 failed. This includes the new PostgreSQL/Redis observer tests. After tightening the due-outbox fixture with an actively leased job, that updated PostgreSQL test passed again by itself. One earlier non-serialized run left two uniquely named synthetic databases after an assertion failure; those exact disposable databases were removed, and the serialized full matrix then passed. No application DB was touched.
- Final parser/app images built for Linux ARM64. Final disposable API/worker/parser were recreated and healthy; protected O01 scrape and two-page packaged OCR passed.
- A06b source/image inspection confirms no Poppler in app runtime, no worker executable in parser runtime, no parser network, no parser secret mounts, and a read-only parser root. This does not establish resistance to kernel vulnerabilities, Docker daemon compromise, host compromise, or all malicious PDF/parser behavior.

Still gated: independent container/security review, a broader egress/mount-sentinel matrix, representative IPC overhead/headroom, layout-helper isolation, production alert thresholds, DB pool/storage/model-health telemetry, independent labeled corpus, accuracy/calibration, measured 1,000+/day capacity, backup/RPO/RTO policy, reviewer identity, release rollback, and live cutover. Do not infer any of these from synthetic tests.

## Cleanup and live boundary

The only fault injection was inside the disposable `rust-ocr-a06b-smoke` project. `/scratch/fill` was removed; the final parser is healthy at its original Compose limits. Private synthetic results/secrets remain under ignored `artifacts/a06b-verification/` for local evidence and are excluded from the commit. The disposable Compose project is stopped without deleting volumes after final evidence capture. The four existing `rust-ocr-*` live container IDs are checked unchanged before commit. No push is performed.
