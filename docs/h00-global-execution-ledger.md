# H00 — Global execution ledger (2026-10-18)

Status: ACTIVE ledger for the H00–H17 execution authorized by
`docs/production-executor-prompt-2026-10-17.md`. Companion packet specs live in
`production-agent-execution-plan-2026-10-17.md`; evidence register in
`production-deep-review-2026-10-17.md`; harness in
`production-executor-harness-2026-10-17.md`. This file records the actual
baseline before the first maintained source edit, per the hard planning gate.

## 1. Actual baseline identity (verified 2026-10-18)

- Branch `codex/rust-ocr-service`, HEAD `b885a48` (clean tree; only preexisting
  untracked `.claude/`, `AGENTS.md`, `CLAUDE.md` — preserved, not staged).
- Application source baseline for review findings: `1f5c6c4` (unchanged since;
  commits after it are docs/traces only, verified via `git diff --stat 211cc97..HEAD`).
- Toolchain: rustc/cargo 1.98.1 native; Docker 29.7.2. No `.codegraph/` (skip
  CodeGraph). GitNexus re-indexed at `b885a48`: 4,207 nodes / 11,431 edges.
- Comment guards pass on the clean tree: 130 files / 0 violations (baseline
  `6e4e851`), block comments 105 files / 0 violations.
- Live containers (read-only observation, NOT touched): 4 original `rust-ocr-*`
  (`ocr-service:local`, postgres:18, redis:8.2-alpine) up 2 days; prod-smoke
  stack (`ocr-service:p1`/`parser-p1`, postgres:18, redis:8.2-alpine) up 38h;
  verify postgres + 2 redis (one OOM fixture) up 39h. All 13 preserved.
- Host: 273 GiB free disk; memory free 23%; swap 11.95/13.31 GiB used — swap
  pressure exists; no real-model/memory A/B runs (H09/H14) until headroom is
  rechecked. Do not kill anything to "make headroom".

## 2. Effective settings (source + compose evidence, not assumptions)

| Setting | Production (`compose.production.yaml`) | Standalone default |
| --- | --- | --- |
| WORKER_CONCURRENCY (W) | 1 (`:187`) | 2 |
| REGION_CONCURRENCY (R) | 1 (`:188`) | 4 |
| LAYOUT_POOL_SIZE (L) | 1 (`:189`) | 1 |
| Parser max sessions | `${PROD_PARSER_MAX_SESSIONS:-8}` | 8 |
| Response large/small/serialization | env-overridable; defaults small=32, large=8, serialization=4 (`config.rs:119-121`) | same |
| Review auth | configured via secret files + principal (`:147-148`) | env-disabled unless both set |
| Worker mem_limit | 2g (`:163`), stop_grace 610s | — |
| Parser | cpus 2, mem 2g, pids 64, network_mode none | — |

ONNX: intra-op 2 / inter-op 1 defaults, arena on (per deep review; unchanged).

## 3. Packet order and current status

Execution order fixed by dependency, READY first:

1. **H00** (this ledger + business-review.md correction) — P1 READY, docs-only.
2. **H01** parser retained-session cancellation — P2 READY synthetic, reproduce-first.
3. **H02** parser per-operation response allowance — P1 READY synthetic; serialize
   with H01 (same file `parser_client.rs`; one writer).
4. **H03** outbox expiry-during-lock — P2 READY disposable DB, reproduce-first.
5. **H04** arrival submit/track separation — P1 READY fake HTTP.
6. **H05** minimal safe verification harness — P1 READY tooling.
7. **H06** scan/SBOM/CI identity coverage — P1 READY tooling.
8. H07 canonical candidate (LOCAL READY when 1–6 frozen; external gated).
9. H08/H09 resource+memory envelopes (H09 real sessions gated by headroom).
10. H10–H17: OWNER/DATA gated as registered; machine-side tooling continues.

Parallel tracks allowed when files/resources don't conflict. One coordinator
stages/commits. No concurrent native compilers.

## 4. Owner-input register (open decisions, from plan §6)

| Decision | Blocks | Continues meanwhile |
| --- | --- | --- |
| Representative corpus + independent annotators | H10/H15/H16 | synthetic contracts, H11 exporter |
| Peak window/arrival mix/deadlines | H14 acceptance | H04 driver, bounded profiling |
| Reviewer identity issuer/roles | H12 real integration | fail-closed contract tests |
| Backup destination/RPO/RTO/escrow | H13 | local corruption controls |
| Paging receiver/watchdog authority | H08 adoption | collector/rule fixtures |
| Hosted push/registry/signing | H07 external | local canonical builds |
| Long-run window/headroom | H09 real A/B, H14 | synthetic memory tests |
| Live cutover/retention/rotation | H17 | dry-run/config evidence |

These are asked once, recorded, and never fabricated. Missing input blocks only
the named acceptance, not unrelated READY work.

## 5. Invariants (must hold in every packet)

- PostgreSQL authoritative jobs/outbox/leases/results/reviews; Redis
  notification/replay only; fresh checks after job-row locks; XADD-before-mark.
- Migrations 0001–0006 byte-frozen; new schema only via new reviewed migration.
- Raw OCR/evidence immutable; corrections separate; conflicts unselected;
  confidence null; autoaccept off.
- W/R/L/L bounded; permits owned by work until completion/kill-reap; no
  thread/task per page/job; no unbounded retry/buffer/output.
- No explanatory comments in maintained `crates/`,`scripts/`,`tools/`,`ops/`
  (guards green at baseline; keep them green).
- Synthetic/disposable fixtures only; never live services; never push; never
  delete volumes; never `down -v`/`prune`/`git clean`.
- Privacy: no document text/names/IDs/secrets in Git, logs printed, or search
  queries; private receipts hashed/ignored.

## 6. Research ledger rule

Before version-sensitive edits (PostgreSQL row locking semantics for H03,
Tokio cancellation for H01/H02), refresh primary docs and record: question,
installed version, dated URL, constraint, applicability, local validation
plan. Facts from source ≠ hypotheses ≠ measured results. If web unavailable:
NOT_VERIFIED and proceed only with work not requiring the fact.

## 7. Verification ladder (per packet)

Targeted tests → affected component suite → workspace gates when source
changed (fmt, clippy -D warnings, workspace tests; explicit `--ignored` only
against verified disposable DB/Redis) → comment guards → `git diff --check` →
GitNexus compare/staged → trace update → focused commit. Documentation-only
changes: link/scope/style + guards, no cold rebuild.

## 8. Status vocabulary

PASS / FAIL / PARTIAL / NOT_RUN / NOT_AVAILABLE / WAITING_FOR_DATA /
WAITING_FOR_AUTHORITY — per packet, separately for implementation,
verification, measurement, adoption. Never relabel a gate to close it.

## 9. Change log

- 2026-10-18: ledger created at H00 start. GitNexus index refreshed to
  `b885a48`. Business-review.md reconciliation scoped (see H00 trace).
