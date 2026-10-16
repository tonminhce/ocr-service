# VS Code Copilot continuation —2026-10-16

The user explicitly requested continuing the single VS Code Copilot session after its quota ended. The session was found in the workspace whose folder URI exactly matches this repository. The exact session identity and transcript location are retained in private evidence; unrelated workspaces/chats were not read. A verbatim user/assistant recovery snapshot is kept only under ignored `.agent-handoff/COPILOT_HISTORY.md`.

## Recovered starting point

HEAD was `66737d8`. Copilot had already committed A06b protocolv2/parser gating/O01 corrections/E03 and packaging changes beyond the earlier Codex handoff. Nine files had uncommitted edits: a cancellation/permit DRY refactor and removal of a SQL-fixture block comment. No Cargo/build/test process was still running. Copilot's retained `rust-ocr-prod-smoke` and `rust-ocr-verify` infrastructure was present, in addition to the original four live containers.

The last human instructions in that transcript authorize completing the phased production plan, clean code/YAGNI, local commits and a hard rule against C-style block comments. Existing no-push, confidentiality and live-boundary constraints remain. This resumes engineering work; it does not authorize live cutover, credential changes or fabrication of independent labels.

## Review and completed fixes

- Preserved the initial worktree patch and migration hashes in private evidence before editing.
- GitNexus incremental refresh failed with an inconsistent FTS index, then a WAL checkpoint failure. Repaired FTS and rebuilt the local index with one worker/larger checkpoint budget. No application reset occurred. Its impact report marks shared cancellation admission CRITICAL:7direct callers,24symbols and5processes. Reviewed actual callers as well; the graph is navigation evidence, not proof of complete dynamic dispatch.
- Completed Copilot's cancellation helper refactor without changing permit ownership. New tests prove cancellation takes priority over available/closed admission, queued cancellation does not consume the next permit, and aborting an async waiter cannot release a CPU permit held by active blocking work. Public canonical cancellation text remains stable for parser transport/callers.
- The SQL fixture still passed after the comment was removed because other body formatting differed. Replaced that fragile distinction with an explicit behavior-preserving `PERFORM1` body mutation. The test now proves exactly the intended function hash changes and tables/other function hashes stay equal. Published migrations are unchanged.
- Added `.github/copilot-instructions.md`, local generated AGENTS/CLAUDE style rules, a lexical source comment checker, focused checker tests and a CI gate. Strings/globs/line comments are not incorrectly rejected. Existing safety-contract line comments/doc comments remain; generated tooling/licenses are outside maintained application source.
- The original packaged smoke was full-page, despite the requested course-layout verification. Added Rust `mock-layout` and isolated smoke overrides to exercise layout → crop → recognition → formatter → extraction with12-second model responses. This is a synthetic detector/model, not a real PP-DocLayout/Qwen quality run.
- Hardened smoke resource ownership: fresh project/output/secrets only; cleanup trap; no credential-bearing curl argv; exact built images, no implicit builds. A negative test proves refusing an existing project does not stop/recreate its containers.
- Added the missing Linux parser/backup verification and backup image build to CI, dependency audit guard, commit-tagged candidate images and local digest inventories. Added a synthetic N→N+1→N rollback harness. See [release operations](release-operations.md) for limits and commands.
- Image scanning found real fixable CRITICAL base-package findings. Applied distribution updates in runtime stages and removed unused backup-image `gosu`. All findings, including unfixed HIGH/CRITICAL, remain in reports; scan policy was not weakened.

## Verification evidence and limits

Private evidence lives under `artifacts/codex-resume-2026-10-16/`. Native format and strict Clippy passed. The workspace full ordinary run passed275tests; the explicit serialized integration run passed90tests. After strengthening the final backup-body assertion, its integration target is rechecked separately. Python policy/profiling/secret/release-pin tests passed11tests. Source comment policy has no violations in maintained indexed source.

Linux ARM64 parser verification passed113tests:80worker unit,19inference-contract and14layout-pipeline; two ignored tests were excluded from that Linux command, not silently counted passed. Linux backup binary verification passed17tests. Packaged smoke passed a two-page course-layout profile job on attempt1, parser outage with queued attempts0, and parser restart recovery on attempt2. These tests use real isolated services and synthetic inference/detection. They do not prove OCR accuracy, real ONNX/Qwen compatibility, memory headroom or1,000+documents/day.

The rollback drill uses Copilot's original protocol-v2 `p1` image pair as N and the current patched candidate as N+1. It verifies complete original job-row hashes remain equal and a new synthetic profile job succeeds on attempt1 in all three stages. It is same-schema/full-page compatibility evidence, not a migration-crossing or real-model rollback certification. A final patched-image course-layout smoke is recorded separately.

The raw cargo audit reported RSA advisoryRUSTSEC-2023-0071 in an inactive optional SQLx/MySQL lockfile dependency. The reviewed audit first rejects RSA if activated in any workspace feature/target, then applies that single documented exception; it reports other informational warnings normally. Local Trivy version is0.72.0; hosted configuration pins0.76.0. These are not claimed to have identical databases/results.

Final machine counts/image/scanner summaries, source hashes, cleanup and gate statuses are recorded in private `verification.json`. Historical intermediate failures/scans are retained. No user PDF, OCR JSON, secret or session history is committed. No live-container change, push, remote message or model concurrency increase is performed.

## What remains

O02 is not production-complete: three consecutive hosted CI runs, trusted registry/provenance and model/configuration approval remain gates. Real-model Mac staging/24h observation cannot be replaced by synthetic timing tests. The host still has about12.9GiB swap used, so no extra real ONNX/model sessions or concurrency sweep was started. Independent labels/adjudication, reviewer identity, backup/RPO/RTO/retention decisions, broader parser boundary review and live adoption remain as specified in the execution plan.
