# Deep-review and execution-handoff trace — 2026-10-17

## Scope and starting checkpoint

The latest user direction requests deep review, traces and a detailed plan/harness for another implementation agent, **not more implementation in this phase**. The preceding synthetic pipeline completion is already committed as `acb8958`. Application source remains `1f5c6c4`; the branch is `codex/rust-ocr-service`. Preexisting untracked `.claude/`, `AGENTS.md` and `CLAUDE.md` remain excluded.

Read repository rules, GitNexus guides, production plan and relevant previous traces. There is no CodeGraph directory, so no indexing was introduced. Read-only reviews cover worker/parser/layout and cancellation/memory, API/core/auth/durability/review/retention, and evaluator/release/backup/monitoring/model installation. Coordinator cross-checks include Dockerfile, CI, all six package manifests and key parser/store/arrival source. This is a focused cross-layer review, not a claim that every source line, production setting or attack surface has been exhaustively assessed.

## Evidence inventory and navigation

Ignored evidence is under `artifacts/production-deep-review-2026-10-17/`. `review-inventory.json` hashes162 tracked source/configuration/tooling paths and separately verifies all120 compiled-source manifest paths still match the accepted application. Hash inventory is identity evidence, not proof every inventoried line was reviewed. It contains no real document/OCR/label values.

GitNexus concept queries/context guided unfamiliar exploration. Fresh upstream impact receipts are preserved:

| Target | Result | Manual interpretation |
| --- | --- | --- |
| ParserSession request | LOW;3 direct/3 upstream/2 mapped processes | Inspect preparation/native/layout call paths too |
| Shared parser read_frame | CRITICAL;8 direct/36 upstream/20 mapped processes | Warning surfaced; plan avoids broad codec edits; no edit performed |
| Store mark_dispatched | LOW;1 direct/2 upstream/1 mapped process | Real caller dispatch_outbox at worker.rs:241/265; production defect remains unproven |
| Arrival run | LOW;0 direct/0 mapped processes | Incomplete alias mapping: actual CLI import/call is main.rs:4/130; zero is not absence of callers |

Graph output includes broad generic/dynamic/test mappings and is not a security proof or future edit approval. Execution agents must refresh impact at their actual baseline.

## Review outputs and corrections

1. [Deep review](production-deep-review-2026-10-17.md):18 classified findings/candidates with source anchors, verified baseline, preserved architecture and honest scope limits.
2. [Execution plan](production-agent-execution-plan-2026-10-17.md):18 H00–H17 packets, READY/conditional/owner gates, dependency order, targeted tests, measurements, acceptance and rollback.
3. [Executor harness](production-executor-harness-2026-10-17.md): copy-ready prompt plus preflight/impact/privacy/resources/streams/testing/commit/trace/continuation rules.

Independent review corrected material details before handoff:

- Typed NativeTextResult already has a4 MiB preallocation guard; DR02/H02 now target requested RGB envelope and wrong response phase, not an invented missing typed-text bound.
- Nominal W active RGB pages excludes bounded cancellation/helper ownership tails; it is not an exact process peak proof.
- Loss-host recovery means simulated source unavailability in a disposable rehearsal, never wiping/stopping actual hosts/volumes/caches.
- Final quality-qualified H14 acceptance depends on independent corpus and supported sidecar/scorer; raw mechanism runs do not close that gate.
- Dataset splits remain the supported dev/calibration/test set; no speculative training schema was introduced.
- Human review preserves existing history entries but appends new auditable decisions; acceptance does not incorrectly require the entire history to stay unchanged.

Static observations are distinguished from reproduction: retained-session future-drop and outbox expiry-during-unchanged-lock are test-first hypotheses. No new reproduction/production fix was run. The arrival lifetime-slot limitation and scan-set mismatch are confirmed measurement/coverage gaps, not claims of service capacity or known vulnerabilities.

## Safety and verification boundary

No application source, Cargo profile/lock, migration, live configuration or model changed in this phase. No new Cargo compilation, Docker resource creation, database mutation, real inference, private-PDF inspection, push or volume deletion occurred. Read-only Docker inspection for independent acceptance of the preceding pipeline and public primary-documentation retrieval are separate from new runtime execution. Native target remains2,606,344 KiB allocated, approximately2.49 GiB.

Documentation-only verification includes both source comment guards,42 Python harness tests, format/whitespace, local links and required compare/staged GitNexus scope checks. New unindexed documents are manually reviewed; zero mapped processes does not replace their review. Earlier315 native/316 Linux+90 explicit counts are scoped prior evidence, not newly rerun Rust tests or proof that future changes pass.

Rollback is documentation-only. Existing application/evidence remains available. Production quality/capacity/identity/off-host recovery/notifications/canonical release/provenance/adoption gates are intentionally not marked complete by this review or its planning documents.

## Final documentation gate

PASS:130-file no-new-comments guard,105-file block-comment guard,42 Python tests, source formatting/whitespace,97 local documentation links and syntax-only validation of five Bash snippets. All120 compiled source hashes still match; no application/configuration/migration diff exists against `1f5c6c4`. Both required GitNexus compare/staged checks report LOW for six documentation files, one indexed heading and zero mapped processes. Four new documents and the resume addition have no corresponding indexed symbols and were reviewed manually and through independent read-only domain reviews; the graph result is not described as complete document coverage.

The staged scope contains exactly the four new review/plan/harness/trace documents and two existing documentation entry-point updates. It excludes source, private artifacts/receipts and preexisting untracked harness files. Per-document hashes are retained in ignored `handoff-manifest.json`. No new runtime acceptance or implementation is inferred from this planning commit.
