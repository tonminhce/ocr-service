# E03 Bounded Arrival Runner Trace

Date: 2026-10-16. Status: **runner implemented and verified with fake HTTP only; no real OCR capacity run**. The existing closed-loop benchmark remains available and unchanged in scheduling behavior.

## Implementation

- Added `ocr-eval arrival` with fixed-rate and seeded-Poisson schedules. Schedules are deterministic for a seed, finite by duration/max-job caps, and emit occurrence sequence IDs.
- Limits: rate 0.001–100 documents/second, duration 1–86,400 seconds, 1–10,000 scheduled occurrences, 1–4 GiB total source bytes, and 1–4 pending tasks. Pending tasks cannot exceed the configured client count. Each individual source remains bounded by the existing 32 MiB source limit and is hash-checked immediately before upload.
- Every occurrence uses an idempotency key containing run UUID, occurrence sequence, and source fingerprint. Repeating a corpus source therefore does not replay a prior occurrence. Repeated occurrences are load samples only, not independent quality samples.
- At an arrival tick, a full pending-task cap creates an explicit skip rather than silently delaying the offered schedule. Source-byte-budget skips are tracked separately.
- Reports contain scheduled/offered/accepted/rejected/completed/succeeded/failed/timed-out/outstanding counts; budget/cap skips; offered rate over the scheduled window; scheduler lag; schedule/client-drain/total time; maximum in-flight tasks; and submission, accepted-to-terminal, and successful end-to-end latency p50/p95 with sample counts. Client-drain time does not prove timed-out server work has ended.
- A timed-out accepted job retains its UUID and contributes to `outstanding`; jobs continue server-side and must be drained before another run. Tokens and source file paths are not emitted in the report.

## Verification

`cargo fmt --all -- --check` passed. `cargo clippy --locked -p ocr-eval --all-targets -- -D warnings` passed. `cargo test --locked -p ocr-eval` passed **44 tests, 0 failures**: 4 library tests, 4 fake-HTTP benchmark tests, 11 v1 scorer tests, and 25 v2 scorer tests.

Fake-HTTP coverage verifies:

- Fixed arrivals continue during a deliberately slow submission endpoint; skipped arrivals are visible and observed in-flight work never exceeds the configured cap.
- Occurrence idempotency keys are unique even when multiple scheduled occurrences reuse a small corpus.
- Total source-byte budget stops further uploads and reports byte skips.
- HTTP 429 and 503 remain rejected outcomes instead of disappearing from accounting.
- An accepted job whose status remains slow past the client deadline is reported timed out and outstanding, with its job UUID retained.
- Existing closed-loop bounded concurrency/compact status behavior still passes.

No model endpoint, live OCR API, worker setting, Redis, or PostgreSQL service was used by these tests.

## Remaining gates

The arrival runner is infrastructure for measurement, not capacity evidence. A real run still requires an approved workload profile/corpus, a disposable evaluation deployment, adequate host memory headroom, and a drain/stop plan. The 2026-10-16 read-only snapshot showed only 1.34 GiB host swap free while the live worker used 1.72 GiB of its 2 GiB cgroup, so no real arrival load was started. Independent labeled data, representative arrival rate, tail-latency target, and capacity acceptance remain open.
