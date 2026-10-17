# H02 execution trace — per-operation parser response allowance (DR02)

Date: 2026-10-18. Packet H02. Scope: `ParserSession::request` local response
allowance + header-only malicious-peer regressions. Serialized after H01
(`fd22a2a`), same file, fresh impact run.

## Status

- Implementation: PASS.
- Verification: PASS (12/12 parser_client incl. 4 new; 112 worker lib;
  322 workspace ordinary / 0 failed / 90 ignored not counted as passed).
- Measurement: N/A (bounded-memory correctness; no allocation benchmark
  claimed).
- Adoption: N/A (worker client behavior).

## Baseline identity

Branch `codex/rust-ocr-service`, H01 commit `fd22a2a` + this packet.
rustc/cargo 1.98.1. GitNexus `request` upstream re-run post-H01: LOW,
3 direct callers, 2 mapped processes. Shared CRITICAL codec
(`read_frame`/`exchange`) deliberately untouched; control-frame zero-data
fixes from earlier milestones untouched.

## Change (minimal)

`ParserSession::request` now derives the response data allowance from the
requested operation before any read:

- `PageInfo` → 0
- `NativeText` → existing `NATIVE_TEXT_LIMIT` (4 MiB)
- `RenderRgb{profile}` → `3 * min(max_pixels, edge(profile)²)` with
  `checked_mul`; overflow → transient typed error, no read
- anything else → 0

The allowance is passed as `read_frame`'s `data_limit`, so an oversized
header fails the existing header limit check
(`parser_protocol.rs:280-286`) before metadata read, body wait, or the
`vec![0; data_length]` allocation. Valid frames at/below the allowance are
byte-identical to before; `render_rgb`/`native_text` post-read validation
retained as defense in depth. No protocol, version, config, or shared-codec
change. Removed the now-lib-unused `RESPONSE_DATA_LIMIT` import (tests
import it locally).

## Regression evidence (baseline RED → candidate GREEN)

All malicious headers withhold body/metadata behind a barrier; the distinct
error message proves header-stage rejection (a metadata/body wait would
surface the 3 s operation-timeout message instead):

1. `oversized_rgb_header_is_rejected_before_body_wait` — 64 MiB
   RgbResult header vs 4800-byte CourseLayout allowance
   (min(4000 px, 40²)×3 in the fixture). Baseline would wait/read;
   candidate: `isolated parser IPC failed`, transient.
2. `oversized_native_text_header_is_rejected_before_body_wait` —
   4 MiB+1 header vs 4 MiB typed cap.
3. `page_info_response_data_is_rejected_before_body_wait` — nonzero
   data for a zero-allowance operation (control).
4. `exact_allowance_rgb_frame_is_still_accepted` — 40×40 frame at
   exactly 4800 bytes succeeds; parity preserved.

Gates after final edit: `cargo test --frozen -j 1 -p ocr-worker --lib`
112/0/1; `cargo clippy --frozen -j 1 --workspace --all-targets
-- -D warnings` clean; `cargo fmt --all -- --check` clean;
`cargo test --frozen -j 1 --workspace` → 322 passed / 0 failed / 90
ignored (baseline 315 + 3 H01 + 4 H02). Both comment guards 0 violations.

## Failed attempts (preserved)

- First draft moved `server` into the helper and returned it → E0382;
  restructured to return only the join handle.
- Removing `RESPONSE_DATA_LIMIT` from the lib import initially broke the
  test module (E0425 ×11); fixed with a test-scoped import.

## Budgets

Tempdir Unix sockets; no body >64 KiB ever sent (headers withheld);
≤10 s per test; observed serial suite ≤0.2 s. No DB/model/network/containers.

## Rollback

Revert the `parser_client.rs` diff; no migration/config.

## Remaining

None for H02. H03 (outbox expiry-during-lock, disposable PostgreSQL) next.
