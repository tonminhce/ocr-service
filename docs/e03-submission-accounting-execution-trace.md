# E03 — Submission uncertainty and finite Poisson scheduling

Execution date: 2026-10-16. Scope: offline bounded arrival/closed-loop clients, synthetic HTTP/RNG controls and this trace. No API/storage contract, durable queue, inference, runtime concurrency, model, migration or live service changes.

## Status

**IMPLEMENTED / VERIFIED LOCAL SYNTHETIC.** Both failures were reproduced with the existing local evaluator before editing. The focused evaluator suite passed, and the combined ordinary workspace run passed 305 tests with 90 ignored; the later timing increment's final suite passed 307. The explicit serial ignored suite passed all 90 on disposable services. Strict workspace/all-targets Clippy, workspace format, whitespace and both comment guards passed. No binary built before this patch is described as verification of these edits.

## Findings and implementation

| Finding | Reproduction before editing | Change and controls |
|---|---|---|
| Lost enqueue response or HTTP503 could be reported as definite rejection and no outstanding server work | A local fake server read a complete synthetic POST/idempotency key and closed without sending a response. Existing CLI returned success with offered1, accepted0, rejected1, outstanding0, `submit_transport_failed`, and no job ID. `ocr-api/src/error.rs` and `operations-contract.md` explicitly say HTTP503 `enqueue_timeout` may follow a durable commit. | Preserve submission progress before waiting for HTTP and expose separate accepted/rejected/unknown/local-preflight outcomes. Retain the exact idempotency key in the private per-document result. Never retry automatically. Raw TCP fixtures consume the complete bounded request before disconnecting or delaying; tests distinguish429,503,408, malformed202, dropped response, submit deadline, local source mutation, successful completion and accepted-job polling timeout. |
| A valid Poisson seed could panic instead of returning bounded load evidence | Seed7650297886450228676 produces `u64::MAX` as its first random value. The old floating denominator rounded to2^53, giving U=1, log(0), infinite duration and CLI exit101 before HTTP. | Use52 random bits at binary midpoint values. Exactly representable U is between2^-53 and1−2^-53 inclusive, hence strictly inside(0,1). Tests cover both extremes, finite intervals at minimum permitted rate, a maximum-duration finite schedule, tiny-rate rejection, and the original CLI seed. |

The existing HTTP rejection test now treats429 as a rejection and503 as acceptance unknown. The existing accepted-job deadline test retains accepted/outstanding semantics and additionally verifies the separate accepted-outstanding count. Their prior fixtures remain synthetic.

## Submission and serialization contract

Existing report schema identifiers, document status/error strings, `accepted` boolean, success/failure counts, and closed-loop `submitted` meaning are preserved. Fields are additive; consumers that reject unknown fields must adopt them before using these reports. The corrected `rejected` and `outstanding` definitions are intentional observable behavior changes.

Each offered document now includes:

| `submission_outcome` | Meaning |
|---|---|
| `not_submitted` | Local source/hash/media/permit work failed or timed out before the HTTP submission was attempted. No request key exists and no server work from this attempt requires reconciliation. |
| `rejected` | A non202 HTTP client error other than408 was observed. This includes the API's definite429 queue refusal. |
| `acceptance_unknown` | Submission was attempted but no definitive response was observed, or the response was408,5xx, or another unexpected non202 status. The server may have committed work. |
| `accepted` | HTTP202 headers were observed. An empty, malformed, oversized or lost response body cannot undo that fact; a missing job ID remains unresolved. |

`idempotency_key` is null until the HTTP attempt begins, then stores exactly the key sent in that attempt. It contains no bearer credential or OCR text; it is private reconciliation metadata, alongside existing job/document IDs. A key can support an approved same-key API reconciliation or a bounded database lookup. This runner performs neither operation automatically and does not issue a fresh-key replacement.

For the arrival report:

```text
scheduled = offered + client_skipped_or_late + upload_budget_skipped
offered = accepted + rejected + acceptance_unknown + not_submitted
submission_attempted = offered - not_submitted = accepted + rejected + acceptance_unknown
offered = succeeded + failed + timed_out
completed = succeeded + observed terminal server failures
accepted_outstanding = accepted - completed
outstanding = accepted_outstanding + acceptance_unknown
```

`offered` continues to count runner tasks admitted for source/upload/poll work; a local preflight failure is therefore offered but not submitted to the server. Its legacy `offered_per_second = offered / scheduled_window_seconds` remains unchanged. The new `submission_attempts_per_second = submission_attempted / scheduled_window_seconds` normalizes attempted-request count by the scheduled window. Source preparation can delay actual POST attempts into client drain, so this is not a measurement of POST start timestamps or arrivals strictly inside that window. Neither rate proves server receipt after transport failure. The exact attempted count can be lower than admitted task count, and a local source failure contributes zero attempted requests. `failed`/`timed_out` describe the client result, whereas the submission partition describes what is known about server acceptance. They are different dimensions, and neither unknown acceptance nor local failure is called a definite server rejection.

The closed-loop report adds the same rejection/local/unknown/completed/outstanding counters plus `submission_attempted`. Its existing `submitted` is observed accepted count, so `documents.len() = submitted + rejected + acceptance_unknown + not_submitted` and `submission_attempted = documents.len() - not_submitted = submitted + rejected + acceptance_unknown`. Existing `failed = documents.len() - succeeded` still includes timeout/client failures. `outstanding` is a count of unresolved possible server work, not proof that exactly that many jobs are still processing. Both known accepted and acceptance-unknown submissions must be reconciled and server work drained before another capacity configuration starts.

No response bodies or private server diagnostic values enter new report fields. Error codes stay fixed or HTTP numeric codes; benchmark/arrival HTTP body limits, deadlines, total job/byte caps and concurrency bounds remain in force.

## Poisson bounds

`ArrivalConfig::new` already rejects nonfinite rates and rates outside0.001..100, including1e-300. With the new strict variates, the longest interval at the minimum permitted rate is `53*ln(2)/0.001`, approximately36736.8seconds. This is finite and representable as `Duration`; schedule offsets beyond the finite configured window stop scheduling before conversion to a runtime deadline. The constructor caps the run window at86400seconds and scheduled jobs at10000. No new configuration or dependency is added.

The corrected floating mapping changes seeded Poisson offsets slightly; determinism is preserved for the same seed and executable revision. Compare/replay evidence must retain the evaluator revision instead of assuming exact numeric compatibility across a bug fix. Public post-construction mutation of config fields remains the existing library API and is outside the validated constructor contract.

## Pre-edit impact analysis

Every modified existing function/struct/test was analyzed upstream before editing. All results were **LOW**. Private JSON results are under `artifacts/e03-submission-accounting-2026-10-16/`.

| Symbol | Direct scope and indexed processes |
|---|---|
| `benchmark::execute` | One direct `execute_document` caller;3 impacted symbols through both run modes;13 listed runner flow traces |
| `benchmark::execute_document` | Two direct run callers;2 impacted symbols;13 listed runner flow traces |
| `benchmark::run`, `arrival::run` | No indexed callers; actual CLI call paths were inspected |
| `SeededRandom::unit_interval` | Direct `next_interval`, then schedule/run/tests;5 impacted symbols;4 listed arrival flow traces |
| `Progress` | No indexed callers; actual use within execute/execute_document inspected |
| `BenchmarkDocument` | Direct execute_document construction, both run modes upstream;3 impacted symbols |
| `BenchmarkReport` | Direct closed-loop run construction;1 impacted symbol,9 listed flow traces |
| `ArrivalReport` | Direct arrival run construction;1 impacted symbol,4 listed flow traces |
| Existing HTTP rejection and accepted-job deadline tests | No indexed callers/processes |

New enum/test/helper symbols have no prior implementation to analyze. No symbol rename or index rebuild occurs in this increment.

The attempted-request rate follow-on rechecked both run functions and report structs before editing; all remain LOW. GitNexus returns UNKNOWN/not-found for the three newly added tests because the index predates them. Manual inspection confirms their scope is test-only assertions around the existing synthetic helpers and runners; this limitation is retained in the private impact JSON instead of inferred as zero production risk.

## Verification record

The coordinator ran the focused evaluator suite, final serial ordinary workspace tests, explicit ignored tests on the verified disposable PostgreSQL/Redis cluster, strict workspace/all-targets Clippy, format and whitespace checks. No-new-comments and block-comment guards checked 128/105 source files with zero violations. Combined GitNexus compare/all scope retains its CRITICAL warning and is recorded in the session continuation evidence. Logs are private under `artifacts/e03-submission-accounting-2026-10-16/` and `artifacts/production-verification-2026-10-16/`.

An independent reviewer confirmed the submission partitions, conservative unknown handling, accepted-before-body semantics, exact reconciliation key, strict midpoint range and constructor-rate bounds. That review also corrected the normalized-rate description so it does not imply measured POST timestamps. Fake servers use loopback TCP with bounded request reads, synthetic tokens/fixtures and finite fixture deadlines. This increment makes no real model calls, service/container changes, secret reads or data deletion; the separate existing integration fixtures mutate only their disposable test databases/Redis configuration.

## Limits and rollback

The tests establish reporting and scheduler mechanics only. They do not prove real OCR throughput, target-arrival acceptance, service availability or production capacity. The subsequent [timing increment](e03-submission-timing-execution-trace.md) measures client POST handoff timing separately; normalized request counts must not be promoted to measured arrival timing. Unknown outcomes remain unknown until separately reconciled; the report deliberately does not infer a safe restart from missing202/job ID. Rollback is a source/binary revert without storage migration, but restores the demonstrated false-rejection/panic defects.
