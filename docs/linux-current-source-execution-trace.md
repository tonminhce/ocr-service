# Current-source Linux ARM64 release verification

Date: 2026-10-16. Status: verified release tests and binary exports from the frozen current source. Canonical runtime-image builds, new-image pipeline/boundary replay, registry provenance and production acceptance remain separate gates.

## Bounded build and source binding

The interrupted build continued in its original owned container; it was not restarted merely because the chat turn ended. The coordinator later confirmed its terminal state and obtained an independent read-only acceptance audit before accepting its result.

The offline build used immutable cached verification image `sha256:8bcd27ddcf45786842f319504c39c8457e50f0055510cc7168f5b07916d8b372`, Linux ARM64, Rust/Cargo 1.99.0 and its existing Cargo dependencies/target cache. One new container had one CPU, a 1 GiB memory ceiling with no container swap, 256 PIDs, network none, dropped capabilities and no-new-privileges. Source was copied to a private snapshot and bound read-only at `/app`; build/output paths were private. No live service, volume, model, concurrency or secret setting changed.

The acceptance audit independently rehashed all 120 source-manifest paths against both the frozen snapshot and current repository, including byte-only checks of repository synthetic PDF fixtures. All hashes matched, with no missing/extra source-inventory path. This specifically closes the possible copy/hash race in the temporary orchestration script. The final source snapshot was also checked because its output bind could otherwise expose a writable alias. No user-provided document was part of this snapshot or Linux verification.

## Observed results

| Check | Result |
|---|---|
| Current worker release library tests | 99 passed; 1 Redis test ignored |
| Current inference-contract release tests | 20 passed; 1 Poppler test ignored by the ordinary command |
| Current layout-pipeline release tests | 14 passed |
| Release binary exports | Exactly 12 regular executable ELF64 little-endian AArch64 files; individual SHA-256 recorded |
| Frozen and current source hashes | 120 paths each; zero mismatches |
| Build/attach/container completion | Exit 0, stopped, expected image and independent owner UUID, phase `complete` |
| Container memory peak | 763,166,720 bytes, approximately 727.81 MiB, within the 1 GiB ceiling |
| cgroup memory event deltas | Zero, including OOM/OOM-kill |
| Existing running containers | All 13 baseline IDs preserved |

The total is 133 passed and 2 ignored, not 135 passed. The ignored cases were not silently counted as Linux runtime verification. Native explicit integration evidence remains separate.

The eight main binaries are API, worker, parser renderer, layout detector, evaluator, administration, secrets and backup. Four synthetic/helper examples are also exported for the packaged drills. The completed build log records compilation of current worker/evaluator source; cache reuse alone is not presented as source verification. The acceptance audit additionally required all expected exports, source identities, terminal image/owner identity and memory events rather than relying on the shell exit code alone.

Private evidence is under ignored `artifacts/linux-candidate-2026-10-16/`: source manifest/snapshot, bounded build logs/state, resource samples/counters, 12 binary hashes and `acceptance-audit.json`. The stopped build container is retained for inspection; it has no running process and is not a deployed service.

## Limits and next gate

This is a current-source release build/test on existing Linux builder layers, not a new canonical Dockerfile image build, fresh OS/dependency scan, signed source/image provenance, full Linux workspace integration pass or real-model capacity result. Exported application/parser/backup binaries still need source-bound runtime packaging, fresh inventory/scan/lock validation, and pipeline/monitoring/boundary replay on those new image IDs before candidate acceptance. Earlier historical-image matrix results do not attest these new binaries.

No image pull/publication, workflow dispatch, remote Git push, real inference or live cutover was performed. Production readiness remains governed by the execution plan's data, identity, operations, headroom, capacity and release gates.
