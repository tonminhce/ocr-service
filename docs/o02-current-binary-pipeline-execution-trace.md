# O02: bounded current-binary pipeline verification

Date: 2026-10-17. Status: **VERIFIED ISOLATED**, all three synthetic scenarios passed in a fresh reviewed replay. Earlier partial/failed attempts below remain historical evidence, not passes. This verifies selected current binaries on historical runtime bases, not fresh canonical release/provenance, production activation or actual-model performance.

## Scope and safety

This follows the accepted [current-binary overlay/boundary replay](o02-current-binary-boundary-execution-trace.md). Application source remains `1f5c6c4`, with all 120 compiled paths and the same eleven installed executable hashes. The later `abcad39` checkpoint changes documentation only. Historical runtime bases and unverified registry provenance remain explicit limits.

The maintained `scripts/production-smoke.sh` provides the three-scenario contract, but was not invoked directly: it publishes localhost ports and its helper invocations do not supply the additional fixture limits required for this experiment. An ignored override/coordinator reuses the unchanged Compose sources, executable helpers and repository two-page synthetic PDF. No maintained code/configuration was modified for this attempt.

The declared scenarios are a 12-second mock response across parser idle gaps, a 15-second parser outage without spending a queued job attempt, and parser restart during an active first model request followed by retry. The coordinator has a 720-second total budget including cleanup reserve, bounded commands/output and sampled host pressure/swap/disk guards. These sampled host guards are not hard host resource enforcement.

All services use prechecked immutable cached ARM64 image IDs, no builds/pulls, no host ports or extra host mappings, an internal network, read-only root filesystems, no-new-privileges and explicit CPU/PID/memory ceilings with memory+swap equal to memory. The parser remains network-none. Only its socket initializer adds CHOWN; all other capability additions are empty. PostgreSQL/Redis data use private container tmpfs. The worker's only non-secret data mount is the parser socket; its read-only credential binds remain. It uses the packaged mock layout helper and targets only the synthetic model service; no real model weights are mounted.

Steady fixture memory ceilings are PostgreSQL 512 MiB, Redis 256 MiB, API 256 MiB, worker 512 MiB, parser 512 MiB and mock 64 MiB. Client/admin/provisioning helpers have separate small finite limits. These are disposable synthetic-test settings, not proposed production sizing. Fresh credentials follow the existing repository pattern: files `0444` inside host parent `0700` for nonroot bind-mounted readers. Other evidence files are private `0600`; credentials/expanded configuration are not printed or committed.

## Earlier attempts and observed results

| Attempt | Observed result | Not inferred |
| --- | --- | --- |
| First configuration preflight | Compose rejected duplicate merged security options; no resources created | Not an application/test pass |
| Second configuration preflight | Validator expected numeric JSON where Compose returned memory strings; no resources created | Not a memory-limit violation |
| First isolated startup | Provisioning, tmpfs infrastructure, bootstrap, migration and grants completed; private checker rejected Docker's capability spelling | No job submitted; no role/output test pass claimed |
| Fresh reviewed run | Runtime policy and nine configured role/ACL checks passed; slow-model client exited zero and returned one complete output | No full pipeline or durable attempt/status assertion |

The first two corrections were private representation/merge fixes, not weaker limits: explicit list overrides and integer normalization. The third checker expected `CHOWN`, while read-only existing Docker metadata proved the API spelling `CAP_CHOWN`. A separately reviewed, single fresh-project continuation normalized only the optional prefix, enforced the exact permitted set and persisted actual policy before validation. It did not add capabilities or increase budgets. The previous attempts and rationale for this exception to the initial preflight-attempt cap are retained; no further stack was launched after the next failure.

The final run stopped after 40.433 seconds because the coordinator merged Compose stderr into its stdout JSON file. The preserved stream contains three Compose progress/warning lines followed by exactly one complete JSON object and only trailing whitespace. A strict offline recovery and independent read-only review verify the payload's parser protocol 2, `course_layout_vlm`, pages 1/2, one attempted/raw region per page, two layout blocks, nonnull extraction, expected extraction profile, original/source hashes and equality of raw text/bounding boxes to source regions. No synthetic text/value is reproduced here.

This recovered first output is narrower than a passed original smoke scenario: the coordinator failed before its durable database status/attempt query. Actual runtime login identities, final three-job/profile/input preservation, checkpoint-row check, cgroup peak/event snapshots, outage gating and restart recovery were not executed. Configured role/ACL booleans are not the full negative-privilege test matrix. Runtime cleanup records `OOMKilled=false`, but no zero-cgroup-OOM-delta or measured RSS claim is made. The existing native parity and allocation probes remain the reuse-specific evidence; a PDF may carry native observations, so this pipeline does not establish reuse-branch frequency or whole-worker benefit.

## Independent audit and cleanup

The final audit rehashes the current 120-path source identity, accepted overlay chain, private override, 25 command logs and recovered payload. Eleven saved actual container policies verify no host ports/extra hosts, exact memory/swap ceilings, read-only roots, no-new-privileges, dropped capabilities and CHOWN only for the initializer. The raw pipeline report remains `accepted=false`; it was not rewritten into a pass after offline recovery.

Both launched fixture stacks were retired with ownership checks: their recorded ten and eleven containers and internal networks are absent. Provisioning helpers also terminated; no original service was stopped or recreated. All thirteen original running container IDs remain. Each stack retains its one owner-labelled socket volume; no database named volume or image/volume deletion occurred. The earlier boundary socket volume and candidate images remain separate retained evidence.

Private evidence is under `artifacts/current-binary-pipeline-2026-10-17-258033c6/` and `artifacts/current-binary-pipeline-2026-10-17-81c2c152/`, including preserved failed executors/overrides, source links, command receipts, observed policies and cleanup audits. GitNexus does not index these ignored private coordinator functions: upstream checks return UNKNOWN/not found, not a clean application impact result. Manual caller review confines changes to the owned test coordinator; compiled application source remains unchanged.

## Reviewed driver repair

A separate `prepared-next-execute.py` separates stdout JSON from a private bounded stderr log, enforces a combined 2 MiB cap and retains strict JSON parsing and credential scanning. AST validation plus two positive/eight negative offline controls pass. The failed executed coordinator remains byte-identical with its SHA-256 receipt. The prepared file was not executed in its retained old namespace: a fresh copied coordinator received a new UUID/project/output and a separately declared execution scope after independent diff review. Offline recovery of the previous payload is not the live parsing strategy.

## Completed fresh replay

Private evidence: `artifacts/current-binary-pipeline-2026-10-17-644bf3d9/pipeline-audit.json`. Source, immutable images, permissions and resource budgets remain unchanged. Its report records `accepted=true`, `pipeline_passed=true` and `cleanup_passed=true`; previous reports remain false. All three strict stdout JSON results parse without discarding diagnostic prefixes, while stderr is separately bounded and hash-recorded.

| Scenario/postflight | Verified observation |
| --- | --- |
| Slow-model course/profile | Two pages, protocol 2, ordered layout/regions, source hashes, exact raw text/block geometry and extraction profile; succeeded on attempt 1 |
| Parser outage | Job remains queued at attempt zero through the 15-second hold; restart permits completion on attempt 1 |
| Mid-job parser restart | First model request is observed active before its completion; controlled restart loses one attempt, then succeeds on attempt 2 |
| Durable state | Exactly three succeeded synthetic jobs/four attempts, expected profile order, source hashes/input bytes, no errors or remaining leases |
| Historical job immutability | Complete first succeeded job-row hash unchanged after later jobs/faults |
| Database privileges | Nine configured role/ACL booleans and actual API/worker restricted runtime session identities pass |
| Checkpoint default | Zero rows, consistent with opt-in checkpoints remaining disabled; no cache-hit claim |
| Resources | Three per-service cgroup snapshots, zero captured OOM/OOM-kill/group-kill counters, exact declared no-swap ceilings |
| Cleanup | Thirteen owned containers and their internal network removed; one new owned socket volume retained; all thirteen original containers still running |

The run completed in 141.592 seconds. Client intervals were 26.443, 57.266 and 40.588 seconds, including deliberately slow responses, outages and retry waits; these are fixture observations, not throughput or production latency estimates. Minimum sampled host free memory was 19%; maximum observed swap growth was 71.88 MiB, below the declared guard. Highest observed worker cgroup peak was 90,222,592 bytes (86.04 MiB). Parser peaks span restarted lifetimes; the maximum captured value was 24,113,152 bytes. No service peaks are summed into a purported host peak, and none is compared against the unrelated real-model memory baseline.

Actual runtime policy metadata, role checks, strict client/stdout/stderr receipts, durable assertions and resource snapshots are preserved for independent read-only acceptance. Original source hashes, historical failures, volumes and service identities remain separate evidence. No maintained source/configuration change, image pull, real model call, push or live cutover occurred.

Independent final review verifies all 64 successful command-log receipts, strict complete stdout JSON and separately hashed stderr, all 120 current compiled-source hashes, the accepted overlay audit binding, recorded cgroup counters and actual post-cleanup Docker identities. Both source comment guards pass with 130/105 files and zero violations; all 42 Python tests and source formatting pass. The four-file documentation checkpoint has LOW GitNexus compare/staged scope, three indexed headings and zero mapped processes; the new trace and ignored receipts are manually reviewed separately from graph coverage.

## Production boundary

The selected-binary synthetic pipeline gate is now closed for this recorded candidate. Fresh canonical builds, current dependency scans, backup/monitoring packaging/replay, hosted provenance, independent quality, representative memory and agreed capacity/deadline gates remain open. Passing small fake-model fixtures does not authorize increasing production W/R or lowering live memory limits. No further automatic stack retry is needed or authorized by this trace.
