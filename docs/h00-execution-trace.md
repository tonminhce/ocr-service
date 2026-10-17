# H00 execution trace — baseline freeze and contract reconciliation

Date: 2026-10-18. Packet H00 of the H00–H17 plan. Scope: documentation-only
baseline reconciliation; no source/config behavior change.

## Status

- Implementation: PASS (docs corrected against current source).
- Verification: PASS (claims re-checked line-by-line against `response.rs`,
  `config.rs`, `lib.rs`, `compose.production.yaml`; comment guards; scope
  checks). No tests required — no maintained source changed.
- Measurement: N/A this packet.
- Adoption: N/A (documentation).

## Baseline identity

- Branch `codex/rust-ocr-service`, HEAD `b885a48`, clean tracked tree.
  Preexisting untracked `.claude/`, `AGENTS.md`, `CLAUDE.md` preserved.
- Application baseline `1f5c6c4` confirmed still-current for review findings
  (all later commits docs-only). GitNexus re-indexed at `b885a48`
  (4,207 nodes / 11,431 edges / 300 flows).
- rustc/cargo 1.98.1, Docker 29.7.2. Host: 273 GiB disk free, 23% memory free,
  swap 11.95/13.31 GiB used → real-model A/B deferred (H09 gate).
- 13 live containers preserved read-only (4 original `rust-ocr-*`, 6
  prod-smoke `:p1`/`:parser-p1`, 3 verify fixtures).

## Findings reconciled (DR16)

Stale prose in `docs/business-review.md` versus actual controls:

| Claim (before) | Actual source | Correction |
| --- | --- | --- |
| "Eight read operations admitted" unqualified | `config.rs:119-121` defaults large=8, small=32 | Named env knobs; large 8 default + separate small budget 32 |
| "eight-slot semaphore" for projection/serialization CPU | `response.rs:41-42`: `small_cpu=2`, `large_cpu=serialization_concurrency` (default 4) | Fixed 2 small slots; `RESPONSE_SERIALIZATION_CONCURRENCY` default 4 large slots |
| "This admission limit bounds … not bytes buffered by slow clients" (muddled) | `response.rs:156-160` reservation trim into body; `:248` 16 KiB copied chunks; body timeout | Explicit reserve-full-then-trim + 16 KiB chunk + body-timeout bound |

Review-enable prose: environment-only default is disabled, but production
Compose configures token+principal from secret files (`compose.production.yaml:147-148`),
so "sample configuration does not enable review automatically" was corrected to
state the mode distinction. No behavior changed to match prose (rule: fix docs, not code).

Verified untouched: review admission is a separate fixed 4-slot semaphore
(`lib.rs:40`), review body limit 64 KiB (`lib.rs:66`), artifact caps
50 MiB source / 16 MiB JSON as documented.

## Checks run

- `python3 scripts/check_no_new_comments.py` → 130 files, 0 violations.
- `python3 scripts/check_no_block_comments.py` → 105 files, 0 violations.
- Docs-only change: no cold Cargo rebuild required per harness §6.
- `git diff --check` on the staged scope.

## Owner-input register

Opened in `h00-global-execution-ledger.md` §4 (corpus/annotators, arrival
mix, reviewer identity, backup destination, paging receiver, hosted push,
headroom window, live cutover). No owner decisions invented.

## Rollback

Docs-only; `git revert` of the commit restores prior prose. No runtime,
schema, config or data impact.

## Next

H01 (parser retained-session cancellation reproduction) with GitNexus impact
on `ParserSession::request` before any edit; H02 serialized behind it
(same file); H03/H04/H06 parallelizable with disposable resources.
