# H01 execution trace — parser retained-session cancellation (DR01)

Date: 2026-10-18. Packet H01. Scope: `ParserSession::request` fail-closed
repair + regression tests. No schema/config/protocol change.

## Status

- Implementation: PASS.
- Verification: PASS (targeted 8/8; full worker lib suite below).
- Measurement: N/A (correctness packet).
- Adoption: N/A (worker client behavior; no deployment action).

## Baseline identity

Branch `codex/rust-ocr-service`; H00 commit `2708164` + this packet's
unstaged changes. Source baseline `1f5c6c4` for the finding. rustc/cargo
1.98.1 native.

## Hypothesis and classification

DR01 (REPRODUCE FIRST): `request()` at `parser_client.rs:239` cleared the
socket only in the processed-error branch. Dropping the in-flight future
(cancellation `select!` drop path) bypassed cleanup, leaving a half-consumed
stream in the retained session; protocol responses carry no request
correlation, so a delayed response1 could be consumed by request2.

## Reproduction (baseline RED)

`aborted_request_fails_closed_against_delayed_stale_response`:
synthetic Unix-socket server accepts Open/Upload handshake (session with
pages=2), withholds render1's response behind a oneshot barrier, test drops
the pending page1 future (direct drop, not token), server releases a valid
RgbResult{20,10}, then client requests render2.

Observed baseline: `stale page-1 response was accepted as page 2: 20x10` —
REPRODUCED. Deterministic; no timing sleeps (oneshot + select barriers).

## Change (minimal, per reviewed plan)

`ParserSession::request` now takes the socket out of the session
(`stream.take()`) for the entire exchange, so dropping the future
mid-exchange closes the peer stream instead of leaving a half-consumed
socket in the retained session. The socket is restored after any complete
frame (matching the previous complete-frame behavior, including typed
service rejections that intentionally keep the session); it is dropped on
cancellation, timeout, IO/protocol error, or future drop mid-exchange. A
later request on the retained session then fails closed with the existing
typed transient error `isolated parser is unavailable`. No new protocol
field, version bump, or shared-codec edit; error strings byte-identical.

## Blast radius (GitNexus + manual)

`request` upstream: LOW, 3 direct callers (page_info/render_rgb/native_text),
2 mapped processes. Manual coverage beyond graph: production callers
`preparation.rs::prepare_page`/`prepare_rgb_page` via `Arc<ParserSession>`,
plus `parser_service.rs` integration tests. Public behavior preserved:
valid sequential reuse (regression test green), cancellation/error paths
unchanged in message and retryability.

## Tests (all synthetic; tempdir Unix sockets; no DB/model/network)

New in `parser_client.rs`:
1. `aborted_request_fails_closed_against_delayed_stale_response` — RED on
   baseline, GREEN on fix; also asserts the server's stale write fails
   because the client closed the aborted socket.
2. `aborted_request_after_partial_header_fails_closed_on_reuse` — abort
   mid-header; reuse fails closed.
3. `sequential_renders_on_one_session_still_succeed` — valid reuse intact.

Results: `cargo test --frozen -j 1 -p ocr-worker --lib parser_client` →
8 passed / 0 failed (5 preexisting + 3 new). Full gates after the final fix:

- `cargo test --frozen -j 1 -p ocr-worker --lib` → 108 passed / 0 failed / 1 ignored.
- `cargo test --frozen -j 1 -p ocr-worker --test inference_contract` → 20/0/1.
- `cargo test --frozen -j 1 -p ocr-worker --test layout_pipeline` → 14/0/0.
- `cargo clippy --frozen -j 1 --workspace --all-targets -- -D warnings` → clean.
- `cargo test --frozen -j 1 --workspace` → 318 passed / 0 failed / 90 ignored
  (baseline 315 ordinary + 3 new; ignored cases NOT counted as passed and not
  run against inherited services).
- `cargo fmt --all -- --check` clean; both comment guards 0 violations.

## Design correction during verification (preserved)

First fix candidate dropped the socket on typed service rejections
(`operation_result` Err), breaking the documented invariant that a complete
frame — even a typed permanent rejection such as page-number errors — keeps
the session (`typed_errors_keep_canonical_messages_and_permanent_errors_keep_the_session`).
Final semantics: socket restored after ANY complete frame (Ok, typed error,
or unexpected-operation error, matching prior behavior byte-for-byte);
socket dropped only when the exchange is incomplete (cancellation, timeout,
IO/protocol error, or future drop mid-exchange). The DR01 hazard is exactly
the incomplete-exchange case; the reproduction stays green.

## Failed attempts (preserved)

- First test draft created futures without polling them (requests never
  reached the server) and one draft double-connected the session open,
  deadlocking the handshake; both rewritten with a spawned handshake task
  + oneshot barriers before any fix was applied.
- An intermediate run hung on the flawed orchestration; process killed by
  exact binary name, no live resource touched.

## Budgets

Test wall time ≤0.2s serial; fixtures ≤64 KiB; no processes/containers
created; host pressure unaffected.

## Rollback

Revert the single-function diff in `parser_client.rs`; tests are additive.

## Remaining gates

None for this packet. H02 (per-operation allowance) serialized next on the
same file with fresh impact run.
