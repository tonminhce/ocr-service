# Round-6 implementation plan — parser IPC overhead measurement (A06b residual)

Status: plan reviewed before execution. Target: one new ignored
measurement test in `ocr-worker` parser_service tests (synthetic
fixture; no production-code change).

## Question (DR12-adjacent, A06b "Remaining")

What is the per-operation Unix-IPC overhead of the isolated parser
(render_rgb/page_info round trips) relative to the underlying Poppler
render/decode work, on the canonical ARM64 host? This informs whether
parser IPC is a capacity bottleneck worth structural change (it should
NOT be, given persistent sessions) — a measurement, not an optimization.

## Design

New `#[ignore]` test `parser_ipc_round_trip_overhead_probe` in
`parser_service.rs` tests (the in-process service = the real production
IPC path: same socket, protocol, admission, Poppler subprocess):

1. Start the real parser service (TestServer pattern) with the existing
   two-page synthetic fixture; open one session.
2. Warm-up: 2 full page_info+render_rgb cycles (Poppler caches, first
   touch).
3. Measured: 10 cycles of page_info(1) + render_rgb(1, FullPage) +
   render_rgb(2, CourseLayout) — record per-operation and per-cycle
   wall times via Instant, plus page pixel dimensions.
4. Report a strict single-line JSON to stderr: p50/p95 per operation
   kind, cycle total, session pages, fixture identity, host scope
   disclaimer. No assertion on absolute values (measurement, not gate);
   only structural asserts (all ops succeeded, dimensions match the
   fixture).
5. Budget: ≤60 s test wall time; fixture ≤64 KiB; one service process;
   no DB/model/network.

## Acceptance

Receipt recorded in the trace with numbers + scope (ARM64 Mac, in-process
service, synthetic fixture). NOT a production capacity claim; no
threshold promoted. If p95 per-render overhead is within the same order
as historical whole-page Poppler timings, IPC is confirmed non-dominant
(recorded as evidence, not a change mandate).

## Rollback

Delete the test; nothing else changes.
