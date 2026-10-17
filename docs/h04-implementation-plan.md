# H04 implementation plan — arrival submit/track separation (DR04)

Status: plan reviewed before edits. Targets: `ocr-eval/src/arrival.rs`
(submit/registry lifecycle), `ocr-eval/src/benchmark.rs` (submit/track
seam exposure), tests in `ocr-eval/tests/benchmark.rs`.

## Problem / classification

CONFIRMED MEASUREMENT LIMITATION (DR04). `arrival.rs::run` holds each of
its ≤4 `max_pending_tasks` slots through the WHOLE
`benchmark::execute_document`, which polls to terminal. Long-running
accepted jobs therefore occupy submit slots for their entire OCR lifetime
(illustration: 178.43 s jobs × 4 slots ≈ 80.7 completions/hour of generator
turnover — below the 150/hour planning scenario). The driver cannot
independently offer a high arrival rate while accepted jobs are pending.
This is a GENERATOR defect, not service capacity.

## Current behavior to preserve

- Per-occurrence idempotency keys `bench-{run_id}-{sequence}-{fingerprint}`.
- Total source-byte budget with `total_upload_byte_limit` skips; specific
  skip reasons; no silent deferral/relabel; no auto-retry of POST.
- Rejected vs acceptance-unknown vs not-submitted partitions; unknown
  acceptance stays outstanding.
- `ArrivalReport` schema fields (additive only; `schema_version` bump to
  1.1.0 with new fields — old semantics retained).
- Closed-loop `benchmark` mode unchanged.
- INPUT_SLOTS (4) source buffer bound; hash re-verification at use.

## Design

Split tracking from submission inside the arrival runner only:

1. `benchmark.rs`: expose a submit-only entry (refactor `execute` into
   `submit_document` (upload+POST, returns outcome/job id/progress) and
   `track_document` (status poll loop)). `execute_document` retains its
   exact signature/behavior for closed-loop mode by composing the two —
   byte-identical public behavior, no new public API beyond
   crate-internal functions.
2. `arrival.rs::run`: pending `FuturesUnordered` now holds only
   `submit_document` futures. Upload bytes/buffers release when submit
   completes. Accepted/unknown jobs move into a bounded outstanding
   registry (Vec of identities: sequence, job id, idempotency key,
   accepted_at — no document bytes).
3. Registry drain: bounded poll concurrency (reuse the reqwest client's
   connection pool; poll sequentially per job via a bounded task set —
   `max_pending_tasks` no longer caps the registry; a NEW explicit
   `max_outstanding_jobs` (1..=1000, default 100) caps the registry; when
   full, arrivals record `skip_reason: "outstanding_registry_limit"`
   instead of being silently delayed).
4. Finite drain deadline: after the schedule window, poll the registry
   until terminal or a new bounded `drain_deadline` (default 300 s,
   1..=3600 s); unresolved stay `accepted_outstanding`/`acceptance_unknown`
   exactly as today's semantics.
5. Report: additive fields `max_outstanding_jobs`,
   `maximum_registry_occupancy`, `drain_deadline_seconds`,
   `outstanding_registry_skipped`; `maximum_in_flight` continues to measure
   concurrent submissions. Per-occurrence fields unchanged.

Alternatives rejected: raising `max_pending_tasks` (plan forbids; still
couples upload to OCR lifetime); one unbounded task per accepted job
(plan forbids); a second process/runner platform (YAGNI).

## Invariants

- No new dependencies; no service-side changes; fake-HTTP tests only.
- A skip is recorded as a skip; skipped occurrences never become offered.
- Unknown acceptance remains outstanding; no retry of POST.
- Old report semantics: `documents[].result` for offered occurrences has
  the same shape (result.result carries the submit-phase outcome plus
  terminal status when observed within drain).

## Tests (all against the existing axum fake server)

1. Existing 4 arrival tests remain green (may need `max_outstanding_jobs`
   added to config constructor calls).
2. NEW `fast_submits_accept_more_than_four_lifetime_jobs_under_registry`:
   submit delay ~0, status delay long-but-bounded; 10 arrivals at
   `max_pending_tasks=1`; registry budget ≥8 → all 10 offered+accepted;
   server `peak` concurrency stays ≤ configured submit slots; >4 lifetime
   jobs accepted (the DR04 disproof control).
3. NEW `full_registry_records_specific_skip`: registry cap 1, two arrivals
   before first terminal → second records
   `outstanding_registry_limit` skip, never offered.
4. Existing drain/timeout semantics: `accepted_job_past_client_deadline...`
   stays green (drain deadline interacts with per-document deadline:
   `track_document` keeps the per-document `config.deadline` from submit
   start; unresolved → `timeout` + outstanding exactly as before).

## Budget

Fake HTTP only (axum on 127.0.0.1 ephemeral); ≤30 s per test; registry
≤1000 identities × ~100 B; no DB/model/containers.

## Acceptance / rollback

New controls green; existing arrival/benchmark/scoring/v2 suites green;
workspace gates; additive report schema. Rollback: revert the two source
files; no data impact.

## Research

No unresolved version-sensitive external fact: reqwest/axum semantics are
exercised by existing in-repo tests; Tokio FuturesUnordered behavior is
already load-bearing in this file. Local validation = the new tests.
