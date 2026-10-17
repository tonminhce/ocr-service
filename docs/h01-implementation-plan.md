# H01 implementation plan — parser retained-session cancellation (DR01)

Status: plan reviewed before source edits, per harness §4 and ledger §3.
Target symbols: `ParserSession::request` (+ new regression tests only).

## Problem / hypothesis

`crates/ocr-worker/src/parser_client.rs:239` `request()` borrows the socket
from `Mutex<Option<UnixStream>>` and clears it via `stream.take()` only in the
processed-error branch (`:271`). The `tokio::select!` at `:260` drops the
in-flight exchange future on cancellation/timeout *without* taking the socket
out of the session. Protocol responses (`parser_protocol.rs`) carry no
request/page correlation. Hypothesis (REPRODUCE FIRST, DR01): after caller
abort of page1's exchange while the session is retained, a later release of
response1 leaves it queued on the socket; the next request (page2) can then
read the stale response1 and misinterpret it as its own. Disproof: a
deterministic synthetic barrier test showing the aborted session's next
request cannot observe the stale frame (fail-closed instead).

## Identity

- Branch `codex/rust-ocr-service`, HEAD `2708164` (H00 docs). Application
  source baseline `1f5c6c4`. rustc/cargo 1.98.1.
- GitNexus `request` upstream: LOW, 3 direct (page_info/render_rgb/native_text),
  2 mapped processes. Manual review adds real production callers
  `preparation.rs::prepare_page`/`prepare_rgb_page` via `Arc<ParserSession>`,
  and ~12 test callers in `parser_service.rs`. Graph limitation recorded;
  zero mapped upstream callers elsewhere is not treated as proof.

## Invariants

- Typed error contract unchanged: permanent vs transient vs cancelled
  messages stay byte-identical for existing tests.
- `health_check`/`admit`/`exchange` paths untouched (control frames already
  fixed; CRITICAL `read_frame`/`exchange` shared codec NOT edited).
- Protocol frames unchanged — no new request IDs/version bump unless the
  smaller repair cannot satisfy tests (per plan H01 §5).
- Valid sequential reuse of a session must keep working; normal job
  cancellation (worker drops the session) unchanged.

## Baseline / control

Existing tests in `parser_client.rs` cover malformed/forged responses and
out-of-bounds dimensions, but none abort an in-flight exchange and reuse the
retained session. Required failing control before any fix: a synthetic
Unix-socket server that (1) accepts Open/Upload, (2) reads request render1,
(3) withholds response1 behind a oneshot barrier, (4) caller aborts the
render1 future (direct drop, not token), (5) server releases a *valid*
response1, (6) client issues render2 — baseline is expected to accept
response1's frame as render2's answer (wrong-dimension rejection or, worse,
dimension-matching acceptance). Assert fail-closed never returns stale data.

## Minimal change (only if reproduced)

Take the socket out of the `Option` for the duration of the exchange
(`stream.take()` before, restore only after a complete processed exchange on
both Ok and Err paths, mirroring the review's preference). Dropping the
future mid-exchange then leaves the session with no socket → next request
fails closed as `parser_unavailable` (transient), the existing typed error.
Alternatives rejected: per-request correlation IDs (protocol change, YAGNI),
shared-codec edits (CRITICAL scope), never reusing sessions (breaks callers).

## Budget

- Tests: synthetic Unix sockets in tempdirs only; no DB/model/network/containers.
- Time: ≤10s per test; whole H01 run ≤5 min native, `-j 1`.
- Bytes: ≤1 MiB fixtures (tiny RGB frames). PIDs: none added.
- Stop conditions: any flakiness in barrier determinism → investigate, no
  retry-loops; any protocol guard violation → halt packet.

## Tests

1. `aborted_render_leaves_session_fail_closed_against_stale_response` (the
   control above; must fail on baseline).
2. Partial-header abort then reuse (header written, body withheld).
3. Cancellation via token while waiting on the socket mutex → reuse fails
   closed, no stale read.
4. Complete sequential render1→render2 on one session still succeeds
   (regression guard).
5. Existing parser_client suite green (typed messages unchanged).

## Acceptance / rollback

- Baseline-red / candidate-green receipt for test 1; tests 2–5 green.
- `cargo test --frozen -j 1 -p ocr-worker --lib parser_client`, then
  `-p ocr-worker --lib` full, then workspace gates for the commit.
- Rollback: revert `parser_client.rs` diff; no migration/config impact.

## Research

Tokio `select!` drops the losing future at the await point — cancellation
safety of `write_frame`/`read_frame` mid-buffer is the crux; already
constrained by existing docs (deep-review primary references). Installed
tokio from Cargo.lock at baseline; no new dependency. No unresolved
version-sensitive fact blocks this packet.
