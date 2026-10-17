# H03 execution trace — outbox expiry during unchanged-row lock (DR03)

Date: 2026-10-18. Packet H03. Scope: `mark_dispatched` lock-then-fresh-
statement repair + ignored regression on a disposable PostgreSQL.

## Status

- Implementation: PASS.
- Verification: PASS (new regression + full ignored store suite on a fresh
  disposable fixture; ordinary workspace gates).
- Measurement: N/A (correctness).
- Adoption: N/A (worker store path; no live cutover).

## Baseline identity

Branch `codex/rust-ocr-service`, H02 commit `8280fd3` + this packet.
rustc/cargo 1.98.1. GitNexus `mark_dispatched` upstream: LOW, 1 direct
caller (`worker.rs:265` dispatch loop), 1 example; manual review of the
dispatch flow retained XADD-before-mark and replay records.

## Research (primary source)

[PostgreSQL 18 §13.2.1](https://www.postgresql.org/docs/18/transaction-iso.html)
(accessed 2026-10-18): in READ COMMITTED, an UPDATE whose target row was
locked by a concurrent transaction waits, and re-evaluates the WHERE only
against the *updated version* of the row (EvalPlanQual) — and only when the
blocker *modified* it. An unchanged-row lock (`SELECT ... FOR UPDATE`, no
tuple change, then release) never re-evaluates the predicate, so a volatile
`clock_timestamp()` qual evaluated at scan time stays stale after the wait.
Installed fixture: postgres:18 (psql 18.6) — version-matched.

## Reproduction (baseline RED → candidate GREEN)

New ignored test `mark_dispatched_rechecks_expiry_after_unchanged_outbox_lock_wait`
(`ocr-core/tests/store.rs`): submit → claim_outbox → shrink the dispatch
lease to ~900 ms → blocker transaction takes `FOR UPDATE` on the outbox row
without modifying it → spawn `mark_dispatched` → poll `pg_blocking_pids`
until it waits (assert lease still live) → poll until the lease is expired
→ release the blocker → assert.

- Baseline observed: mark returned **true** — "a dispatch lease that
  expired during an unchanged row-lock wait must never authorize the mark"
  FAILED. Deterministic, barrier-based (pg_blocking_pids polling; no timing
  sleeps).
- Candidate: the same test PASSES; token/deadline/next_dispatch unchanged
  after the rejected late mark; a fresh claim+mark cycle still succeeds.

## Change (minimal)

`mark_dispatched` now mirrors the existing `lock_job_write` pattern inside a
bounded transaction: `set_config('lock_timeout','2s')` +
`set_config('statement_timeout','5s')` (same budgets as the job-write path),
`SELECT ... FOR UPDATE` on the outbox row, then the token/clock predicate
UPDATE as a **fresh subsequent statement** in the same transaction. The
fresh statement re-evaluates `clock_timestamp()` after the lock wait, so an
expired lease can never authorize the mark; a missing row rolls back and
returns false. No schema change, no migration, no global dispatch mutex, no
broker change; XADD-before-mark and retained replay records preserved.

## Verification receipts

- `TEST_DATABASE_URL=postgres://h03:h03@127.0.0.1:55432/h03 cargo test
  --frozen -j 1 -p ocr-core --test store -- --ignored --test-threads=1` →
  **34 passed / 0 failed** (all preexisting outbox/replay/fencing/lease/
  retention regressions + 1 new).
- `cargo test --frozen -j 1 --workspace` → 322 passed / 0 failed / 91
  ignored (the 91st ignored is this new test; ignored ≠ passed).
- `cargo clippy --frozen -j 1 --workspace --all-targets -- -D warnings`
  clean; `cargo fmt --all -- --check` clean; both comment guards 0
  violations.

## Fixture hygiene

Fresh owner-labelled disposable namespace `h03-outbox-5A387BAF`
(postgres:18, 512 MiB / 64 PIDs, loopback-only published port 55432, no
volume) created solely for this packet; credential is a synthetic
single-use password. First instance (no host port) was replaced before any
test ran; both instances removed by exact name after verification.
Postflight: zero `h03-outbox-*` containers/networks remain; all 13
original live containers intact; no volumes deleted or created.

## Rollback

Revert the `store.rs` diff; test is additive (ignored without
TEST_DATABASE_URL).

## Remaining

None for H03. Note: blackhole/absolute-completion-bound transport tests
were NOT part of this packet's demonstrated defect and remain unclaimed.
H04 (arrival driver separation) next.
