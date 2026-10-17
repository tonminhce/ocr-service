# H03 implementation plan — outbox expiry during unchanged-row lock (DR03)

Status: plan reviewed before edits. Target: reproduction test first; fix only
if reproduced. `ocr-core` store tests (ignored suite) + possible
`mark_dispatched` fix.

## Problem / classification

DR03 (REPRODUCE FIRST). `mark_dispatched` (`store.rs:477`) checks
`dispatch_lease_expires_at > clock_timestamp()` inside the same UPDATE that
waits for the row lock. PostgreSQL READ COMMITTED re-evaluates the WHERE via
EvalPlanQual only when the blocker *modified* the tuple
([PG 18 §13.2.1](https://www.postgresql.org/docs/18/transaction-iso.html),
accessed 2026-10-18; installed disposable fixture is postgres:18, psql 18.6).
A blocker that holds `FOR UPDATE` without modifying the row and commits
never triggers re-evaluation, so a dispatch lease that expires during the
wait can still authorize the mark → stale dispatch bookkeeping/postponed
replay. The job-write path already uses lock-then-fresh-statement
(`lock_job_write` + fresh predicate), and `unchanged_locked_row_rechecks_expiry`
(`tests/store.rs:1577`) proves that pattern for renew/complete/fail. The
outbox path lacks the equivalent regression.

Hypothesis: with an unchanged-row lock held across expiry,
`mark_dispatched` returns true (defect). Disproof: it returns false and
leaves token/deadline/next-dispatch unchanged.

## Identity

- Branch `codex/rust-ocr-service`, H02 commit `8280fd3`. rustc/cargo 1.98.1.
- Disposable fixture: fresh owner-labelled container/network namespace
  `h03-outbox-<uuid>` (postgres:18, 512 MiB, pids 64, no host ports, no
  volume). TEST_DATABASE_URL targets it only. 13 preexisting live containers
  and all volumes untouched.
- GitNexus `mark_dispatched`: LOW, 1 direct caller (`worker.rs:265`
  dispatch loop), 1 example. Manual review: dispatch_outbox flow — XADD
  before mark retained.

## Test design (barriers, no timing sleeps)

New ignored test `mark_dispatched_rechecks_expiry_after_unchanged_outbox_lock_wait`:
1. Submit job; claim_outbox(1, lease) → token.
2. Shrink `dispatch_lease_expires_at` to ~900ms via direct UPDATE.
3. Blocker tx: `SELECT ... FOR UPDATE` on the outbox row (unchanged).
4. Spawn mark_dispatched(job, token, 30); poll `pg_blocking_pids` until it
   waits; assert lease still live at that moment.
5. Poll until the lease is expired (database clock).
6. Release blocker (rollback).
7. Assert mark returns **false** and token/deadline/next_dispatch_at are
   unchanged; then a fresh claim_outbox can still claim and mark.

Expected baseline: step 7's first assertion FAILS (mark returns true) →
defect reproduced → proceed to fix. If it passes, record disproof and stop.

## Fix (only if reproduced)

Mirror the job-write pattern minimally inside `mark_dispatched`: take the
outbox row lock in a bounded transaction (`FOR UPDATE`), then run the
token/clock predicate UPDATE as a fresh subsequent statement in that
transaction. No schema change, no new migration, no global dispatch mutex,
no broker. Preserve XADD-before-mark ordering and retained replay records.
Separate SQL lock/statement timeouts from caller-total budget: the test
harness pattern uses `set_config('lock_timeout','2s',true)` etc. — follow
`lock_job_write`'s exact budget style.

## Budget

- Fixture: ≤512 MiB, 64 PIDs, no host ports; removed after verification
  (owner-checked, explicit names).
- Tests: ≤10 s each; barriers via pg_blocking_pids polling (5 ms interval,
  700 ms/2 s caps) matching the existing harness.
- No writes to any live/original database.

## Acceptance / rollback

Baseline-red/candidate-green receipt; existing outbox/Redis-loss/lease
regressions (ignored suite) still pass with restricted fixture credentials.
Rollback: revert store.rs diff. Any schema proposal → separate migration
review (none anticipated).

## Research

- PostgreSQL 18.6 §13.2.1 (transaction-iso): EPQ re-check only on modified
  tuple; unchanged-row lock wait does not refresh the qual. Accessed
  2026-10-18. Applicability: installed fixture postgres:18 — matches.
  Local validation: the reproduction test itself.
