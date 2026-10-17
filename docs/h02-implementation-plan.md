# H02 implementation plan — per-operation parser response allowance (DR02)

Status: plan reviewed before edits, per harness §4. Target:
`ParserSession::request` (local allowance derivation) + regression tests.
Serialized after H01 (same file, one writer; H01 commit `fd22a2a`).

## Problem / classification

CONFIRMED code-level budget gap (DR02). `request()` reads established-session
responses with the global 128 MiB `RESPONSE_DATA_LIMIT`
(`parser_client.rs:257`), although each operation needs at most:
- `PageInfo` → `PageInfoResult` carries **zero** data.
- `NativeText` → typed `NativeTextResult` ≤ 4 MiB (protocol + client check).
- `RenderRgb{profile}` → `RgbResult` body = width×height×3 where the
  profile's own bounds cap it at `3 * min(max_pixels, edge²)`.

A malicious/compromised peer can make the client wait for and buffer a body
far larger than the requested operation requires while staying under the
global/type bounds — finite amplification, not unbounded allocation
(`read_frame` still enforces `data_length ≤ data_limit` at the header, and
`validate_payload` binds RgbResult bodies to exactly width×height×3).
Already-fixed controls NOT reopened: health/admission/upload-ack zero-data
(`exchange(..., 0)`), NativeTextResult 4 MiB preallocation guard, typed
no-data rejection before allocation.

## Minimal change

In `request()`, derive the response allowance from the requested operation
before reading, using checked arithmetic:

- `PageInfo` → 0
- `NativeText` → `NATIVE_TEXT_LIMIT` (4 MiB, existing constant)
- `RenderRgb{profile}` → `3 * min(max_pixels, edge(profile)²)` as u64 via
  `checked_mul`, where `edge(FullPage) = max_image_edge`,
  `edge(CourseLayout) = page_max_edge`; on overflow → transient typed error
  without any read.

Pass that value as `read_frame`'s `data_limit`. Oversized headers then fail
at the existing header limit check — before metadata read, body wait, or
allocation. No `read_frame`/`exchange`/protocol changes (CRITICAL shared
codec scope deliberately untouched); no new configuration.

Alternatives rejected: widening `read_frame` with a per-call table (shared
CRITICAL scope); validating only post-read in callers (already exists but
reads the body first — the gap); lowering the global constant (would break
legitimate full-resolution renders).

## Invariants

- Valid renders within the requested envelope continue to pass byte/pixel
  exactly: the allowance equals the maximum legal body for the profile, so
  no legitimate frame is rejected. `render_rgb`'s existing dimension checks
  stay (defense in depth).
- Typed error strings/cancellation semantics unchanged.
- Protocol frames/version unchanged.
- `health_check`/`admit`/`exchange` untouched.

## Baseline / control

New header-only regressions must fail on the CURRENT code by receiving the
oversized body (or waiting on it) — but per the harness, the malicious-peer
tests must reject **before body waiting**, without sending a giant body:
- RGB: request render with small profile bounds; server sends RgbResult
  header whose `data_length` exceeds `3 * min(max_pixels, edge²)` (e.g.
  dims 40×40=4800 bytes legal cap vs header data_length 64 MiB, body
  withheld). Baseline: client waits/reads 64 MiB (gap). Candidate: typed
  transient error immediately, server write fails (client closed).
- NativeText: header data_length 4 MiB + 1 byte (over the typed 4 MiB cap),
  body withheld → reject before body.
- PageInfo: nonzero data_length → reject before body (baseline already
  enforces zero for PageInfoResult at `validate_payload` — control case).
- Wrong-phase: valid small `NativeTextResult` while `RenderRgb` was
  requested — already invalid via `validate_payload` mismatch? (verify:
  NativeTextResult allows ≤4 MiB regardless of request; the phase check is
  caller-side `let Operation::RgbResult{..} = frame.operation else` — so
  the client would read ≤4 MiB then reject; acceptable, unchanged).
- Exact boundary: RgbResult at exactly the allowance passes.

## Budget

Synthetic tempdir sockets only; bodies withheld behind barriers (never
actually sending >64 KiB); ≤10s per test; no DB/model/network.

## Tests

1. `oversized_rgb_header_rejected_before_body_wait` (baseline RED at the
   header stage: read_frame's `data_length > data_limit` with derived
   allowance; GREEN = typed transient error without body arrival).
2. `oversized_native_text_header_rejected_before_body_wait`.
3. `page_info_response_data_still_rejected` (zero-allowance control).
4. `exact_allowance_rgb_frame_still_accepted` (boundary parity).
5. Existing parser_client + parser_service suites green (no behavior change
   for valid frames).

## Acceptance / rollback

Baseline-red/candidate-green for 1–2; controls 3–4 green; full worker lib +
workspace gates. Rollback: revert `parser_client.rs` diff only.

## Research

`read_frame` header-limit semantics verified against installed source
(`parser_protocol.rs:280-286`) — no external fact unresolved. Tokio
cancellation behavior already constrained by H01 receipts.
