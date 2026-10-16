# E02 inference request ownership and encoding

Date: 2026-10-16. Status: implemented and verified locally with synthetic fixtures. Scope: reduce temporary encoding copies and release the request JSON value before waiting for model HTTP. No model, concurrency, output, checkpoint, schema or live-service configuration changes.

## Finding and implementation

The [E02 audit](e02-memory-audit.md) identified overlapping JPEG, base64, data-URL, JSON value and serialized request buffers. `send_transcription` previously constructed a separate base64 string, formatted it into a data URL, inserted a clone through `json!`, and retained the JSON value across the HTTP await.

The pinned [reqwest 0.12.28 implementation](https://github.com/seanmonstar/reqwest/blob/v0.12.28/src/async_impl/request.rs) serializes `.json(&body)` eagerly into an owned request body. The [serde_json macro source](https://github.com/serde-rs/json/blob/v1.0.154/src/macros.rs) converts interpolated expressions by reference, which can clone a supplied string. These behaviors were checked before choosing the ownership change.

The client now preallocates the exact data-URL capacity from checked base64 length plus prefix length, appends STANDARD base64 directly, and moves that string into the prepared JSON slot. It then constructs the request builder and explicitly drops the JSON value before calling `.send().await`. Request-builder serialization failures still reach the existing fixed connection/error mapping; no transport behavior is changed to a separate build/execute path.

Shared R admission and telemetry still surround construction, send, bounded response reading and completion parsing. Checkpoint keys/lookup/write order, model identity, task/system prompts, full-page sampling omissions, regional sampling overrides, sensitive default Authorization, response limits, timeouts and cancellation ownership remain unchanged. There are no new dependencies, threads, helpers or settings.

## Impact and independent review

Pre-edit upstream impact for `send_transcription` reported LOW: one direct `transcribe_uncached` caller, three impacted symbols and no indexed execution processes. Actual full-page/regional call paths were inspected because Rust dispatch coverage is incomplete. New parity-test symbols were initially UNKNOWN/unindexed. Raw impact evidence is private under ignored `artifacts/e02-request-memory-2026-10-16/`.

An independent source review found no blocking issue. Combined change detection retains the shared CRITICAL warning across the continuing A06b/B02/E02/E03 patch; its full result is recorded in the session continuation evidence rather than relabeled LOW from this one function's impact.

## Verification

| Check | Result |
|---|---|
| Worker inference-contract target | 20 passed; one Poppler case ignored by the ordinary command |
| Worker layout-pipeline target | 14 passed |
| Final ordinary workspace suite including the later evaluator timing increment | 307 passed; 90 ignored |
| Explicit serial ignored suite on disposable services | 90 passed; zero ignored |
| Strict workspace/all-targets Clippy | Passed |
| Format, whitespace, no-new-comments and block-comment guards | Passed; 128/105 source files checked with zero violations |

The new public-client test submits bounded synthetic byte inputs of lengths 0, 1, 2, 3, 257 and 65,536 to a loopback fake server. It compares the exact data URL with the previous STANDARD encoding expression, covering all padding cases, and checks content type, Authorization, sampling omissions and unchanged returned text. Existing regional request/sampling, cancellation/shared-R, checkpoint and raw-layout tests passed. Test counts overlap the workspace/explicit totals and are not additional tests to sum.

The explicit suite ran only against the already-running `rust-ocr-verify` PostgreSQL/Redis instances on loopback ports 55434/56381/56382. Read-only preflight/postflight checked disposable administrator identity/privileges, no extra databases and no backup-role dependencies. No Compose service or volume was recreated/deleted. All sources and data fixtures were synthetic; no real inference endpoint was used.

## Bounded allocation probe

The private `request-allocation-probe.rs` reproduces old/new request-construction primitives and full-page fields/prompts with a synthetic model alias. It does not execute the actual async recognition method. It constructs no network connection, never calls `.send`, and measures neither latency nor OCR throughput. Prebuilt 4 KiB/256 KiB payloads and client setup are outside tracking; each method runs six times. Exact method, URL, headers and serialized body equality are asserted before measurement, and every measured construction releases all tracked allocations after cleanup.

| Synthetic input bytes | Identical HTTP body bytes | Original retained requested bytes after `.json()` | Updated retained requested bytes | Construction peak, both methods |
|---|---:|---:|---:|---:|
| 4,096 | 6,212 | 23,590 | 12,934 | 23,590 |
| 262,144 | 350,276 | 1,055,782 | 701,062 | 1,055,782 |

The verified reduction is retained ownership before HTTP waiting and lower aggregate requested allocation bytes during construction. Peak construction allocation did not decrease in these fixtures. Small allocation counts increased while reallocations fell; this is not a zero-allocation request builder or a claimed CPU speedup. Requested/live byte accounting excludes fixture/client allocation, allocator metadata and transient realloc internals. It is not process RSS or cgroup memory. Full raw reports and source hashes remain private.

## Remaining gates and rollback

JPEG/page/crop ownership outside request construction remains, including per-job region futures before shared R admission. Real worker/cgroup memory, allocator A/B, representative quality and actual arrival/burst/soak capacity remain gated in the [production execution plan](production-execution-plan.md). The host still has no new measured headroom proof for increasing real-model W/R. A smaller retained request object alone does not resolve whole-host pressure.

Rollback is a source/image revert without data or schema migration and restores the previous temporary copies/lifetimes. This candidate has not been deployed or published.
