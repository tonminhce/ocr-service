# O02 hosted CI audit and runtime image prerequisites

Date: 2026-10-16. Status: CI prerequisite fix implemented and locally verified; hosted execution pending. Scope: read-only hosted evidence audit, one CI image-loading step, a static cross-file regression and this trace. No workflow was dispatched or rerun, no source was pushed, and no local images or services were changed.

## Hosted evidence checked

GitHub CLI read access succeeded for [tonminhce/ocr-service](https://github.com/tonminhce/ocr-service). The active workflow is `Rust checks`, `.github/workflows/ci.yml`. At audit time the local branch `codex/rust-ocr-service` had HEAD `aa88d07eac3c43e5bfa57adc25f1cb0642c4a3ee` plus uncommitted changes. GitHub returned zero hosted runs for that exact local HEAD. The remote branch remained at its ancestor `6e4e851ee40427159f0c11c7acb9cc0982bed8b9`.

| Branch run, newest first | Source | Hosted result |
|---|---|---|
| [38864544297](https://github.com/tonminhce/ocr-service/actions/runs/38864544297) | `6e4e851` | Failed; check and dependencies passed, images failed |
| [38841361608](https://github.com/tonminhce/ocr-service/actions/runs/38841361608) | `3194abd` | Success |
| [38829421979](https://github.com/tonminhce/ocr-service/actions/runs/38829421979) | `e3f3e97` | Success |
| [38765322391](https://github.com/tonminhce/ocr-service/actions/runs/38765322391) | `8851078` | Success |

The three older consecutive successes are real historical evidence; they do not validate the current local candidate or the later failed source. The latest run completed on 16/10/2026 at 14:21:04 in Asia/Ho_Chi_Minh. Its image job passed Linux parser/backup verification, all three application/parser/backup image builds and the configured SBOM/vulnerability scan. Step 12, `Local release inventory and digest lock`, failed; the packaged smoke was skipped afterward.

That failed run retains an unexpired `release-evidence` artifact of 54,009 bytes and five Docker build artifacts. Artifact contents were not downloaded. Their existence is not evidence of signed provenance, successful pipeline smoke or a release approved for production.

Selected safe metadata is retained under ignored `artifacts/production-verification-2026-10-16/hosted-ci-audit.json`. The requested job log was captured privately as `hosted-ci-images-failed.raw.log`, mode 0600; its 809,069 bytes were not emitted. The only exposed failure message was the fixed `Release inventory failed; no release approval implied`. The script suppresses its underlying exception, so the exact historical exception is not established by that message.

## Structural finding and fix

The inventory explicitly inspects `postgres:18` and the exact digest-pinned Redis reference, in addition to the three built images. The separate `images` job did not load PostgreSQL or Redis before the inventory. The `check` job's service images live on another runner; a PostgreSQL base fetched by BuildKit also does not establish that the Docker engine can inspect the `postgres:18` alias. The private job log contains PostgreSQL BuildKit base references but no explicit PostgreSQL/Redis pull command and no pinned Redis reference.

This is a concrete fresh-runner prerequisite gap and a source-supported explanation for the inventory failure. Because the failure text hides its cause, it is retained as an inferred historical cause rather than a proven underlying exception. The local monitoring additions had not closed this gap.

The `images` job now explicitly pulls the two required runtime references immediately before inventory. This starts no container and expands no deployment configuration. PostgreSQL is pulled using `postgres:18` because that is the exact lookup used by the inventory; pulling only a digest would not guarantee the tag alias exists. Its mutable tag remains the existing policy, and the inventory records the observed engine/repository identity. Redis keeps the existing exact digest. No registry login, image publication, model/configuration change or provenance claim is introduced.

## Impact and verification

GitNexus query/context and upstream impact were run before editing `ImagePinTests`: LOW, zero indexed direct callers and zero affected flows. Actual CI and local unittest-discovery callers were reviewed manually. The existing pinning test methods are unchanged. The new test was not yet indexed; its test-only scope is inspected directly. The raw impact report is retained beside the audit evidence. No reindex was performed during concurrent verification.

The new static regression derives required literal runtime image references from the inventory's Python AST, then tokenizes the preload commands in the workflow's `images` job before its inventory step. It requires the exact references, including aliases, and accepts only `docker pull` commands. It performs no Docker call, process launch, network request, service startup or secret read. This catches missing preloads, wrong digest/tag aliases and placement in another job or after inventory. It does not prove registry availability, architecture resolution or success on a hosted runner.

Targeted command, executed successfully by the coordinator with three passing tests:

```sh
python3 -m unittest discover -s scripts -p test_release_manifest.py
```

| Gate | Current result |
|---|---|
| Read-only GitHub workflow/run/job/artifact metadata audit | Completed |
| Required runtime image preload and static regression | Implemented; all three release-manifest tests passed |
| Python AST syntax and whitespace check | Passed |
| Workflow syntax/shell review | YAML parsed; 15 shell steps passed `bash -n` without execution |
| Python scripts test suite | 33 passed |
| No-new-comments and retained block-comment guards | Passed; 128/105 source files, zero violations |
| GitNexus compare-scope change detection | Combined CRITICAL scope reviewed and retained in continuation evidence |
| Hosted CI for the exact current sources | No runs; publication/execution remain gated |

## Evidence limits and remaining gates

The clean prior O01 inventory attests its own input hashes for `aa88d07`: seven image identities, 141 input hashes and six migration hashes, with monitoring included. Its image/source binding is explicitly operator-declared, registry publication is unverified and runtime configuration covers unexpanded templates only. It does not include this session's uncommitted changes. Existing local inventories, SBOMs and rollback drills remain evidence within their documented scopes; they are not absent merely because hosted acceptance is open.

Three successful hosted runs of reviewed candidate sources, trusted registry/source provenance, approved runtime/model/configuration, representative quality/capacity evidence and production adoption remain gates in the [release operations runbook](release-operations.md) and [execution plan](production-execution-plan.md). Neither this patch nor the historical runs authorize push, rerun, workflow dispatch, registry publication or live rollout. Rollback is a workflow/test source revert; it restores the fresh-runner image prerequisite gap and requires no data or schema change.
