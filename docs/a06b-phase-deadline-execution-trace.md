# A06b parser request phases and total health deadline

Date: 2026-10-16. Status: implemented and verified locally. Scope: local parser IPC hardening and synthetic fixtures. This follows the control-response allocation correction in the [O01 trace](o01-alerting-execution-trace.md). It does not authorize or establish production adoption, real OCR quality, throughput or capacity.

## Findings and bounded implementation

The health client previously gave the connection and exchange separate copies of the caller's budget. The observer imposed its own total timeout, but startup and worker admission called the shared health function directly. The health function now wraps connection, exchange and capacity validation in one timeout using the supplied budget. Existing connection and exchange guards remain, and expiry returns the existing fixed, retryable operation-timeout message. Dropping the timed-out future drops its socket; the change adds no detached task, thread, dependency or configuration.

Parser request decoding previously applied the 64 MiB upload allowance to the initial handshake and every established-session request. A frame valid as an `Upload` could therefore allocate a body before its operation was rejected for the wrong session phase. The request reader now takes an explicit data bound: zero during the initial handshake, 64 MiB only for the admitted upload, and zero after the document is opened. The existing frame decoder checks this bound from the header before allocating or reading metadata or data. Valid upload, control and rendering contracts remain unchanged; protocol version and persistent schema are unchanged.

The earlier control-response bound and its regression fixtures in `parser_client.rs` are preserved. Binary RGB/native responses still use the document-session response reader. Worker, model, layout, decode and session concurrency limits are unchanged.

## Pre-edit impact and review

GitNexus query/context and upstream impact were run before editing existing symbols. Raw reports are retained under ignored `artifacts/a06b-phase-deadline-2026-10-16/`.

| Symbol | Graph result | Reviewed scope |
|---|---|---|
| `health_check` | CRITICAL; seven direct callers, nine impacted symbols, four affected process roots | Startup, worker admission, dependency observer and service tests; warning reported before editing |
| `read_request` | LOW; two direct callers, two affected roots | Initial handshake, admitted upload and established document session |
| `handle_connection` | LOW; zero indexed direct callers | Actual `serve` task closure reviewed; the graph omits this caller |
| `serve_document` | LOW; one direct caller, one affected root | Admitted connection and document operations |

New test symbols were not yet indexed and return UNKNOWN; their test-only scope was reviewed manually. No index refresh was performed during concurrent work. Graph impact is navigation evidence and does not prove complete Rust dispatch coverage.

## Verification record

| Check | Current result |
|---|---|
| `rustfmt --edition 2024 --check` for the two changed parser files | Passed |
| `git diff --check` | Passed |
| New health deadline/recovery fixture | Passed in serial worker/workspace suites |
| New request-phase header/recovery fixture | Passed in serial worker/workspace suites |
| Existing parser client/protocol/service and observer regressions | Passed; initial worker suite 96 passed/1 Redis integration ignored; final ordinary workspace 297 passed/90 explicit integration ignored |
| Explicit synthetic Poppler ordering/page-limit pipeline | 1 passed on the combined source changes |
| No-new-comments and retained block-comment guards | Passed; 128/105 source files, zero violations |
| Strict workspace/all-targets Clippy | Passed |
| GitNexus change detection | Combined scope reviewed with CRITICAL warning; see continuation evidence |

The request-phase fixture uses the real local parser service with one session and a five-second frame timeout. In both initial and established phases it supplies only a header advertising one byte, 64 MiB or `u64::MAX`, keeps its client socket open, and requires server closure within one second. No metadata or large payload is sent. Established-phase cases first complete a real tiny synthetic PNG upload. Each case checks zero active sessions through a fresh healthy probe; a final new upload and RGB render checks recovery. The short rejection deadline distinguishes header rejection from the service's longer frame timeout. Existing actual-upload bounds remain covered by protocol tests.

The health fixture accepts and reads a probe, withholds its response, verifies that timeout closes the client socket, and serves the next healthy probe successfully. It exercises expiry, retry classification and recovery. It does not deterministically delay both Unix connection establishment and exchange in one test: portable backlog behavior and timer scheduling would make that fixture unreliable without extra test machinery. The shared total budget is established by the single outer timeout enclosing both operations; a dynamic regression separating the old and new two-phase duration remains a test limitation. Tokio timeouts remain cooperative and are not a real-time scheduling guarantee.

No Cargo build or test was started by the implementation subtask, to avoid overlap with the parent task's resource-sensitive verification. The parent ran the serial worker/pipeline and final workspace checks; private logs are in `artifacts/e02-json-budget-2026-10-16/` and `artifacts/production-hardening-2026-10-16/`. Counts overlap and must not be summed. No live service, volume, credentials, model process, remote branch or registry was changed.

## Remaining gates and rollback

The [production execution plan](production-execution-plan.md) still governs broader malformed-frame/egress/mount testing, independent parser boundary review, representative IPC/headroom measurements, hosted release verification and production adoption. These local fixtures do not close those gates.

Rollback is a source/image revert within protocol version 2. No database migration or stored-data transformation is involved. Rolling back these bounds restores the previous larger invalid-frame allocation exposure and split health timeout; it must not restore an unsandboxed document parser.
