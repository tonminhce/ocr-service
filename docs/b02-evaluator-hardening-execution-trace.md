# B02 — Evaluator provenance and private diagnostic hardening

Execution date: 2026-10-16. Scope: offline evaluator validation and decoding errors, synthetic regression controls, and this trace. No runtime OCR, raw source/result, API schema, review workflow, migrations, dependencies, model settings or live services change.

## Status

**IMPLEMENTED / VERIFIED LOCAL SYNTHETIC.** The two defects were reproduced before editing using the existing local `target/debug/ocr-eval` and temporary synthetic fixtures. The patched evaluator library, CLI, v1/v2 and arrival/benchmark regressions passed in the serial ordinary workspace run: 297 total tests passed, with 90 explicit integration tests ignored. Strict workspace/all-targets Clippy, workspace format, whitespace and both comment guards passed. The pre-existing binary reproduction establishes the defect; the current-source suite establishes the patch within these synthetic controls.

## Findings, implementation and controls

| Finding | Evidence before the patch | Change and verification scope |
|---|---|---|
| An all-recovery prediction could declare `found` and obtain full grounded/all-critical credit | `v2::validate_graph` checked normalized values and observation states for `Found`; it checked recovery provenance only when the prediction already declared `recovery_only`. A one-field synthetic prediction with only `origin=recovery`, valid source/span/geometry, and `state=found` exited 0 and reported grounded/all-critical accuracy 1.0. | A predicted `Found` must contain first-pass support. Independent gold remains `human_gold`; its states, attribution and values are not inferred from machine origin. Negative controls cover one and two all-recovery observations supporting the same value. The positive control uses independent gold with two real fixture regions and a machine first-pass/recovery pair, requiring unchanged full grounding. |
| V1 decoding errors could expose private enum values or unknown property names | A malformed synthetic v1 manifest used a sentinel as `split`; CLI stderr included that sentinel in the Serde cause chain. Manifest, business and extraction decoding retained Serde sources; the raw result decoder was also inconsistent with the fixed v2 diagnostics. | Manifest/result/business/extraction decoding boundaries now return fixed diagnostics without the Serde source. Tests invoke the CLI for malformed split, unknown property, label state, malformed JSON, malformed business state, bare extraction, OCR-result extraction and API-job extraction; each must fail, emit no stdout, and omit both the private sentinel and a retained cause chain. Direct `load_business` checks cover its public JSON boundary too. |

The existing singleton ambiguity test continues to prove that valid but incorrect first-pass `found` output scores zero against independent ambiguous gold. Its incorrect prediction now declares first-pass origin so the test still reaches semantic scoring. A separate recovery-only control requires rejection before scoring. Existing v1 valid fixture metrics and v2 candidate/person/card/page/transform controls remain required regression evidence.

## Pre-edit impact analysis

GitNexus upstream impact ran before every modified existing symbol. The current index reports the following; direct source inspection confirmed the CLI paths because indexed callers are incomplete.

| Symbol | Risk | Direct callers | Indexed scope |
|---|---|---|---|
| `v2::validate_graph` | LOW | `v2::Dataset::load`, `v2::Dataset::prediction` | 3 impacted symbols; 2 summarized load/score processes, covering 12 listed flow traces |
| `Dataset::load` in v1 `lib.rs` | LOW | None indexed; CLI call inspected | 0 indexed impacted symbols/processes |
| `Dataset::result` | LOW | v1 `score` | 1 impacted symbol; 1 summarized process with 9 listed score flows |
| `load_business` | LOW | `Dataset::result` | 2 impacted symbols through `score`; 1 summarized process with 9 listed score flows |
| `gold_singleton_ambiguity_is_valid_but_does_not_become_found` | LOW | None | 0 indexed callers/processes |

No HIGH/CRITICAL result occurred. New test functions have no prior indexed implementation to analyze. No rename or index refresh was performed.

## Evidence and commands

Private local evidence directory: `artifacts/b02-evaluator-hardening-2026-10-16/`. It is ignored by Git and contains impact JSON plus `pre-edit-reproductions.json`. The reproduction record contains only aggregate synthetic observations and fixed sentinel diagnostics. Temporary source/prediction/manifest fixtures were deleted automatically after their CLI processes exited.

The coordinating agent ran the broader serial workspace suite and strict workspace/all-targets Clippy, covering these targeted commands' evaluator tests. The final commands and logs are under `artifacts/production-hardening-2026-10-16/`. Equivalent targeted commands are:

```sh
cargo test -p ocr-eval --locked
cargo clippy -p ocr-eval --all-targets --locked -- -D warnings
cargo fmt --all -- --check
python3 scripts/check_no_new_comments.py
python3 scripts/check_no_block_comments.py
node .gitnexus/run.cjs detect_changes --scope compare --base-ref codex/rust-ocr-service
```

Use the installed CLI's supported spelling for the final compare command. Combined GitNexus scope also includes A06b/E02 work and retains its CRITICAL warning; see the continuation evidence. Comment guards checked 128/105 source files with zero violations. No commit or remote push is performed by this package.

## Limits and rollback

These synthetic checks validate evaluator mechanics and diagnostic privacy. They do not establish OCR accuracy, independent annotation provenance, calibrated confidence, production throughput or acceptance risk. A model adapter must preserve honest first-pass/recovery origin; the scorer cannot cryptographically attest how an operator generated a sidecar.

Valid manifest/prediction/report shapes and scoring formulas remain unchanged. The v2 loader intentionally rejects a previously accepted malformed all-recovery `found` prediction; the producer must retain ambiguity and no selected machine truth. Diagnostic text remains fixed and gives no raw private decoding details. Rollback is a binary/source revert with no schema or data migration, but reintroduces the demonstrated evaluator defects.
