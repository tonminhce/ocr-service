# H04 execution trace — arrival submit/track separation (DR04)

Date: 2026-10-18. Packet H04. Scope: `ocr-eval` arrival runner lifecycle
split + `benchmark.rs` submit/track seam + fake-HTTP regressions + CLI knobs
+ docs.

## Status

- Implementation: PASS.
- Verification: PASS (59 ocr-eval tests incl. 2 new; workspace 324/0/91).
- Measurement: PASS at generator level (the new control accepts 10 lifetime
  jobs at `max_pending_tasks=1`; registry occupancy >4; server submit
  concurrency peak 1). This proves the GENERATOR, not OCR capacity.
- Adoption: N/A (eval tooling; no service change).

## Problem (confirmed measurement limitation, DR04)

`arrival.rs::run` held each ≤4 pending slot through the whole
`execute_document` (upload + poll-to-terminal), so long OCR jobs starved
the arrival schedule (illustrative 4×178.43 s ≈ 80.7 jobs/h generator
turnover vs 150/h planning scenario). Little's-law registry sizing still
applies; no capacity claim is made here.

## Change

1. `benchmark.rs`: `execute` split into `submit` (upload+POST, returns job
   id) and `track_document` (status poll loop). New crate-internal
   `submit_document`/`submit_document_deadlined` and `track_document`;
   `execute_document` composes both with byte-identical closed-loop
   behavior. `Progress`/`Status` made `pub(crate)` with `pub(crate)`
   fields; `Progress`, `BenchmarkConfig`, `Dataset` gained `Clone`
   (metadata-only; no document bytes).
2. `arrival.rs`: pending set now holds ONLY submit tasks
   (`tokio::spawn` + `FuturesUnordered`). Accepted/unknown jobs move to a
   bounded `Vec<RegistryEntry>` registry (job id, progress — no bytes) of
   at most `max_outstanding_jobs` (1..=1000, default 100); when full, the
   occurrence records the explicit `outstanding_registry_limit` skip (the
   job may be server-accepted → must be reconciled like other outstanding
   work). After the schedule, registry entries are polled to terminal
   concurrently, each under the REMAINING per-document deadline
   (`config.deadline` minus submit latency), with the finite
   `drain_deadline` (1..=3600 s, default 300 s) as backstop; per-document
   deadline expiry yields `timeout`/`document_deadline_exceeded` +
   outstanding, matching prior semantics. Report schema bumped to
   `ocr-arrival-benchmark/1.1.0` with additive fields:
   `max_outstanding_jobs`, `maximum_registry_occupancy`,
   `outstanding_registry_skipped`, `drain_deadline_seconds`.
3. `main.rs`: optional `--max-outstanding-jobs` (default 100) and
   `--drain-deadline-seconds` (default 300) flags.
4. `docs/evaluation.md`: registry lifecycle, skip semantics and
   reconciliation duty documented.

Preserved: idempotency key format; total-upload-byte budget skips; specific
skip reasons (never silently delayed/relabel); no POST auto-retry;
rejected/unknown/not-submitted partitions; unknown acceptance outstanding;
closed-loop `benchmark` mode unchanged; INPUT_SLOTS 4-buffer bound and
hash re-verification at use.

## Tests

New fake-HTTP regressions (axum, 127.0.0.1 ephemeral):

- `arrival_registry_accepts_more_than_four_lifetime_jobs_with_fast_submits`:
  10 arrivals, `max_pending_tasks=1`, registry 10 → all offered+accepted,
  10 server submissions, submit peak 1, registry occupancy >4 — the DR04
  disproof control.
- `full_outstanding_registry_records_specific_skip`: registry 1, fast
  arrivals → ≥1 explicit `outstanding_registry_limit` skip; skipped count
  equals report counter; skipped occurrences may be accepted and remain
  outstanding.

Existing suites all green, including
`accepted_job_past_client_deadline_is_reported_outstanding` and
`submission_deadline_after_consumption_remains_unknown_and_unreconciled`
(after restoring per-document deadline coverage over BOTH phases).

## Failed attempts (preserved)

- First draft moved whole `execute_document` into spawns without a
  per-document deadline on the tracking phase → status polls could outlive
  the document deadline (2 test failures); fixed by remaining-deadline
  tracking + drain backstop.
- Submit-phase timeout initially built a fresh `Progress`, losing the
  idempotency key recorded before POST (test caught: key must survive a
  submit-deadline expiry); fixed via `submit_document_deadlined` wrapping
  `submit` so pre-timeout progress (key, AcceptanceUnknown) survives.
- Type-escape errors (E0521) fixed with `Arc`/`Clone`; clippy
  complex-type/let-return fixes; a required-flag CLI regression (new flags
  must default, not break the CLI smoke test) fixed with optional values.

## Verification receipts

- `cargo test --frozen -j 1 -p ocr-eval` → 8 unit + 11 benchmark + 13
  scoring + 27 v2, 0 failed.
- `cargo test --frozen -j 1 --workspace` → 324 passed / 0 failed / 91
  ignored (322 + 2 new).
- `cargo clippy --frozen -j 1 --workspace --all-targets -- -D warnings`
  clean; `cargo fmt --all -- --check` clean; both comment guards 0
  violations.

## Budget

Fake HTTP only; ≤10 s per test (observed 5 s suite); registry ≤1000 ×
small identities; no DB/model/containers; no live service touched.

## Rollback

Revert `arrival.rs`, `benchmark.rs`, `main.rs`, test and docs diffs; no
data/schema impact (report schema is versioned additively; old reports
remain 1.0.0-readable).

## Remaining

Generator capability proven; real capacity remains gated on H14 inputs
(workload profile, headroom window, corpus) and H08/H09 readiness. No E04
conclusion may be drawn from fake-HTTP results.
