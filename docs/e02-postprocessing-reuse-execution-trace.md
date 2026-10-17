# E02: reuse equivalent OCR-only extraction issues

Date: 2026-10-17. Status: native implementation and synthetic verification passed; final isolated Linux release verification is recorded in the accompanying Cargo execution trace. No live adoption or production throughput claim.

## Review and bounded implementation

The worker previously extracted fields from the selected source blocks, validated that result, and then ran the complete extractor again against raw OCR solely to retain its issues. For ordered OCR-only inputs, the selected blocks and raw OCR are identical.

The change is confined to `crates/ocr-worker/src/post_processing.rs`. `make_output` reuses a clone of the first result's issues only when native and recovery inputs are empty and OCR page numbers are nondecreasing. Otherwise it retains the separate raw-OCR extraction. Existing signatures, `select_sources`, source selection, schemas, evidence, review policy and cancellation/validation boundaries remain unchanged.

`select_sources` visits pages in ascending order and retains input order within each page. Therefore the guard proves equality of the entire block sequence, not merely equal text or a matching page count. Nondecreasing order permits multiple blocks on the same page. Empty input is also equivalent. Any native/recovery input or descending/interleaved page order falls back to the old calculation.

Pre-edit GitNexus impact was LOW: one direct caller (`apply`), six upstream symbols and one mapped process. Actual callers/source were inspected because the graph does not completely describe dynamic worker dispatch. `select_sources` was HIGH and was deliberately not edited. After adding the parity helper/tests and refreshing the index, `make_output` maps HIGH: two direct callers and thirteen upstream symbols, including the new test paths. This warning was reported; no further production-function expansion followed.

Independent review found no concrete unbounded-concurrency defect in the reviewed worker/parser/layout paths. Existing bounded job tasks, shared model admission, CPU permits and layout-helper ownership remain. No extra thread, pool or speculative abstraction was added. A possible same-size crop copy was deferred because its frequency/benefit is unmeasured.

## Verification

Six added tests compare both complete JSON outputs against a test-only implementation of the legacy calculation:

| Case | Preserved behavior |
| --- | --- |
| Ordered OCR-only pages/regions | Exact extraction, raw issues, source metadata and outer warning |
| Descending/interleaved pages | Raw issue ordering stays distinct from selected-page ordering |
| Selected and sparse/unselected native text | Parallel observations/selection are unchanged; no unsafe reuse |
| Recovery-only alternative | Ambiguous, unselected business field; raw first-pass quality remains separate |
| Placeholder and source-size budgets | Same budget issues and empty extraction source projection |
| Empty input | Same complete missing-field/review output |

Initial synthetic fixtures contained an invalid monthly-period spelling and incorrectly expected `conflict` for two observations with the same contract origin. The legacy equality checks passed, but those extra semantic assertions failed. The fixtures were corrected to the repository's `VND/tháng` grammar. The native disagreement assertion now follows the existing resolver: `ambiguous`, null selection and exactly two candidates. Production extraction rules were not changed to satisfy tests. Both failed Linux attempts/logs/snapshots are retained privately.

Native Rust/Cargo 1.98.1 verification passes all 315 ordinary workspace tests under each of the full-debug/incremental baseline, line-tables-only and nonincremental configurations. The 90 ignored cases are not counted as passed by those commands. Strict workspace/all-targets Clippy passes for all three configurations. Native source formatting, Python harness and comment-policy results are recorded with the final package verification.

## Allocation probe and limits

A private single-threaded allocation probe uses 32 synthetic source blocks. It computes primary extraction before measurement, then compares the old raw-issue derivation (`extract(...).issues`) with cloning the equivalent primary issues. All seven rounds produce equal serialized issues.

| Median per derivation | Old second extraction | Reuse |
| --- | ---: | ---: |
| Allocation/reallocation requests | 3,012 | 903 |
| Requested bytes | 191,600 | 55,716 |

The reductions are approximately 70.0% in requests and 70.9% in requested bytes for this substep. These are allocation traffic, not live/peak heap, process RSS or whole-worker savings. The probe is an instrumented debug build; its timings are not a production latency benchmark. It does not measure rendering, inference, storage, real OCR accuracy or quality-qualified goodput.

The first out-of-tree link attempt selected incompatible Serde feature variants by file timestamp. The corrected probe selects dependencies from the exact creating Rust compiler invocation. No dependency/version or application serialization change was made.

Private evidence: `artifacts/cargo-target-execute-2026-10-16/raw-issue-probe-summary.json`, individual probe rows, native test/Clippy logs, source manifests and `artifacts/cargo-performance-2026-10-17/linux-evidence/`. No private document, label, model output or credentials are included in this trace.

## Rollback and open gates

Revert the guarded reuse branch to restore the second extraction; no migration/data rewrite is needed. The parity tests remain useful regression coverage. Whole-parser/worker representative RSS, model capacity, independent quality labels and E04 arrival/deadline acceptance remain open. This patch does not authorize raising W/R, changing models, automatic acceptance or live deployment.
