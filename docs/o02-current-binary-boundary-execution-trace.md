# O02/A06b: current-binary packaging and boundary replay

Date: 2026-10-17. Status: selected current executable bytes packaged on historical runtime bases and all seven boundary controls passed. This is not a fresh canonical image build, full OCR pipeline test, vulnerability approval, registry provenance or production activation.

## Source and package binding

The [full Linux release acceptance](cargo-target-optimization-execution-trace.md) verifies source commit `1f5c6c4`, all 120 compiled source paths, 316 ordinary tests, all 90 explicit tests, strict release Clippy and 12 ARM64 executable exports. This follow-up packages eleven of those exports: six application CLIs, four synthetic/helper examples and the parser renderer. The backup executable is not included in these two runtime images.

Only ignored, newly owned build contexts were created. They contain accepted executable bytes and two COPY-only Dockerfiles; no application source, private documents, labels, credentials or OCR results. The app context is approximately 37.37 MB and parser context 3.87 MB. Builds run sequentially with explicit Linux ARM64 platform, `--pull=false`, `--network=none`, no `RUN`, no compilation and finite 180-second command deadlines. Sampled host guards stop on sustained low free-memory reserve or additional swap growth; they are not hard native resource enforcement.

| Role | Historical base image | New image |
| --- | --- | --- |
| Application | `sha256:179dd1be747b170044ed8605e41dc69a6550cdf59a0de6c189953d8f5e03cd55` | `sha256:c326ed86707f171109d649fcecd781e4620d5387951db03b94573fa8ec221faf` |
| Parser | `sha256:a65dc35e99ce6e93e316d44e05f3c4223c14baece95eb5fa08d3f3c1a1136540` | `sha256:b36bd62c726a0483b0b180d7341ca7e5bcdf89db7d4dc3b820b45c6f159cdf3d` |

Previously absent UUID-scoped local aliases resolve to the exact cached base IDs; no existing service image tag was replaced. BuildKit resolved only those local IDs. Build exit codes are zero, with observed durations 1.835 and 0.566 seconds. These are small packaging-command observations, not OCR performance measurements.

Two stopped owner-labelled audit containers were created without starting their entrypoints. Selected installed files were copied out and checked against accepted export SHA-256, byte size and mode `0755`. Both containers were removed with ownership checks and without a volume-removal flag. Actual image inspection confirms the historical base layer prefix, runtime user, entrypoint/command and environment are preserved. The chain is compiled source identity → accepted exports → eleven installed paths → new image IDs → boundary report image IDs. Independent read-only review rehashed this chain and checked actual image/container/volume state.

## Unchanged boundary harness

The existing `scripts/parser-boundary-smoke.py` ran against the new immutable image IDs in a new UUID-scoped project/output directory. No maintained harness/source change was needed. It uses synthetic control data, no real model or document. All seven controls passed:

| Control | Observed result |
| --- | --- |
| Parser runtime policy | Expected restricted runtime settings |
| Canary positive before | Control HTTP path works before denial checks |
| Network denial | Parser IPv4 path denied |
| Mount sentinels | Sibling synthetic sentinels unavailable |
| Scratch noexec | Synthetic executable denied on scratch |
| Worker socket permissions | Expected worker access to parser socket |
| Canary positive after | Control HTTP path still works after checks |

The harness has a 140-second execution budget plus 30 seconds for cleanup and bounded per-command output. It creates four owned containers and one internal network; at most the parser and two small controls remain running together. Configured concurrent ceilings are approximately 2.125 GiB memory and 2.5 CPUs; the parser's allowed memory+swap is 4 GiB. These are ceilings, not measured usage or proof of a no-swap production envelope. No live worker/model concurrency or resource limit changed.

Execution exited zero in approximately 1.77 seconds. Owner-checked cleanup removed its four containers and internal network. Its new socket volume remains retained, as required by the no-volume-deletion rule. All thirteen original running container IDs were expanded unambiguously and preserved. An initial short-ID/full-ID comparison failed preflight before mutation; that failed attempt remains in private evidence rather than being hidden as a passed command.

## Evidence, limits and next gate

Private evidence: `artifacts/current-binary-overlay-2026-10-17-3f86a696/`, including `overlay-audit.json`, COPY contexts, bounded build logs, extracted binary hashes, resource samples, initial preflight failure and the unchanged raw boundary report. The companion audit is accepted with `selected_11_binary_chain_verified=true`, while `canonical_fresh_build=false` and `registry_provenance_verified=false` remain explicit.

The raw harness retains `current_worktree_binary_attested=false`; it cannot infer source identity by itself and was not rewritten to manufacture attestation. Source binding is the independently checked companion chain for the selected eleven paths, not a claim that all runtime bytes/dependencies were rebuilt from this source. No image pull, push, hosted dispatch, real inference, live cutover or volume deletion occurred. New local candidate images/aliases and the small owner-labelled socket volume are retained for inspection.

Fresh canonical builds, current base/dependency inventory and scans, the backup/monitoring release set, full synthetic pipeline/monitoring replay, hosted provenance and release approval remain open. An available cached scanner database had already reached its update time; no historical offline scan is presented as fresh release approval. Real corpus quality, representative model/worker memory and E04 deadline/capacity remain governed by the [production execution plan](production-execution-plan.md).

The local verification checkpoint contains six documentation files only. Both required GitNexus compare/staged checks report LOW, eight mapped headings and zero mapped processes; the new trace has no indexed symbols and was reviewed separately. Both comment guards pass with 130/105 files, all 42 Python tests pass, and formatting/whitespace plus 77 local links pass. Compiled source is unchanged from the accepted `1f5c6c4` increment. Private contexts, logs, binaries and preexisting untracked harness files are excluded from Git.
