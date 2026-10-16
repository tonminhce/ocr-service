# E03 — Actual monotonic POST attempt timing

Execution date: 2026-10-16. This increment adds timing evidence to the bounded offline load clients after [submission uncertainty hardening](e03-submission-accounting-execution-trace.md). It changes no API, durable queue, inference model, runtime concurrency, migration or live service.

## Status

**IMPLEMENTED / VERIFIED LOCAL SYNTHETIC.** Pre-edit GitNexus impacts are recorded. The timing increment's focused evaluator suite and final ordinary workspace suite passed; the final workspace count is 307 passed with 90 ignored. Strict workspace/all-targets Clippy, format, whitespace and both source comment guards passed. The earlier 90 explicit tests exercised unchanged API/core/worker integration paths; they are not counted as additional timing tests. Independent source review found no blocking issue.

## Finding and change

Task admission and completed result counts did not establish when an HTTP submission actually began. Lazy client polling, source reading/hashing, multipart preparation or client drain could move the POST attempt beyond its intended arrival or measurement window. Dividing all eventual attempts by the scheduled window could therefore overstate the load offered within that window.

Both run modes already establish a monotonic run origin. `Occurrence` now carries that internal origin to the existing execution path. After file/hash/media/multipart preparation and request construction, immediately before handing the request to `RequestBuilder::send`, the client records `submission_started_offset`. The per-result JSON adds optional `submission_started_offset_ms`; it is null when local preflight never reached an HTTP attempt. Existing `submit_latency_ms` still includes source/preflight through observed202 headers, so it is not silently relabelled as network-only latency.

The timestamp describes a **client HTTP handoff**, including attempts that subsequently fail before transmission. It does not establish socket write completion, server receipt, durable acceptance or server processing start. Transport loss/503/deadline uncertainty and exact private idempotency-key reconciliation remain as previously hardened.

## Measurement contract

Existing schemas, task counts, acceptance partition, normalized rates and the shared origin remain unchanged. New fields are additive; consumers with strict unknown-field rejection must adopt them. Closed-loop results expose the same per-result offset, without pretending they use an arrival window.

Arrival reports add:

| Field | Definition |
|---|---|
| `submission_starts_in_window` | Actual client POST handoffs with exact monotonic offset in `[0, scheduled_window)` |
| `submission_starts_after_window` | Actual handoffs with offset `>= scheduled_window`, including requests first started while draining client work |
| `submission_starts_in_window_per_second` | In-window starts divided by the unchanged scheduled-window seconds |
| `submission_scheduling_lag_samples` | Number of measured POST handoffs; local failures and client/budget skips contribute no sample |
| `submission_scheduling_lag_p50_ms`, `submission_scheduling_lag_p95_ms` | Nearest-rank quantiles of `max(actual handoff offset − planned occurrence offset, 0)` |

The policy compares internal `Duration` values, retaining nanosecond boundaries before any conversion to JSON milliseconds. The exact end of the window is exclusive: a start exactly at the end is after-window. The internal duration is omitted from JSON; its milliseconds projection is the public value. The small private aggregation helper keeps the boundary policy testable without sleep-based scheduling assumptions.

```text
submission_attempted = submission_starts_in_window + submission_starts_after_window
submission_scheduling_lag_samples = submission_attempted
submission_starts_in_window_per_second = submission_starts_in_window / scheduled_window_seconds
```

`offered_per_second` remains admitted-task count normalized by the scheduled window. `submission_attempts_per_second` remains all eventual client POST attempts normalized by that same window, including attempts beginning after it. Operators use **`submission_starts_in_window_per_second`** when describing load actually handed to the HTTP client inside the arrival window; they inspect the after-window count and scheduling-lag samples alongside it. None of these rates prove service-side received/accepted throughput. Late task admission and source preparation remain visible through the distinction between existing task scheduler lag and the new POST scheduling lag.

## Deterministic controls

- An internal execution test passes a genuine monotonic origin ten seconds in the past, uses a synthetic PDF prefix and a loopback listener that closes, and requires the measured offset to fall between that origin's known elapsed lower/upper bounds. No clock configuration or test sleep is introduced.
- The timing-policy test supplies exact durations at zero, one nanosecond before the window end, the exact end, and one nanosecond after it; only the first two count inside. Scheduling lag is calculated from the actual POST start, and an empty set creates no invented samples.
- Existing raw TCP429/503/408/malformed202/disconnect controls now require a per-result start, timing/count partition identities, one POST-lag sample and the exact in-window rate denominator. They avoid fixed wall-time thresholds.
- The changed-source preflight control requires null start time, zero in/after-window counts/rate, no POST-lag samples and null quantiles. Existing local-failure accounting is preserved.
- The closed-loop lost-response control requires a start offset even though acceptance remains unknown. Existing byte/timeout/recovery/finite-Poisson controls remain regression requirements.

These are synthetic client mechanics tests, not OCR capacity evidence. A reported late start is evidence about this client, not proof of model queueing or a production SLO violation.

## Pre-edit impact analysis

Private results are under `artifacts/e03-submission-timing-2026-10-16/`. All indexed modified symbols returned **LOW**: `execute` has one direct `execute_document` caller and three impacted symbols through both run modes (thirteen listed flow traces); `execute_document`/`Occurrence` have both run modes as direct callers; `BenchmarkDocument` feeds execute_document then both run modes; `ArrivalReport` has its arrival run constructor and four listed flows; `Progress`/both run functions have no indexed upstream callers. Actual CLI/execution references were inspected because the index does not cover every Rust relationship.

The three modified E03 tests are absent from the older index and return **UNKNOWN/not-found**. Manual scope is their synthetic test-only runner/fake-server usage and added assertions; this is recorded rather than presented as an indexed zero-risk result. New timing helper/tests have no prior implementation to analyze. No index refresh, rename, external notification, model call, Docker mutation or secret read occurs.

## Verification and rollback

The coordinator ran targeted evaluator tests, the final ordinary workspace suite and strict workspace/all-targets Clippy. Formatting, whitespace and both source comment guards passed; the guards checked 128/105 maintained source files with zero violations. Tests cover the shared origin, exact timing boundary, missing attempts and synthetic HTTP loss/deadline/acceptance controls described above. Private logs are under `artifacts/e03-submission-timing-2026-10-16/` and `artifacts/production-verification-2026-10-16/`. Aggregate GitNexus change detection retains its CRITICAL warning in the continuation evidence; this increment adds no new unbounded work or concurrency. Rollback is a source/binary revert with no storage or settings migration; it would remove actual POST timing evidence while retaining the prior outcome accounting if reverted independently.
