# A06b — Bounded container boundary matrix

Date: 2026-10-16. Status: **VERIFIED LOCAL BOUNDARY MATRIX ON SUPPLIED HISTORICAL IMAGES**. This trace records an isolated boundary harness, synthetic unit verification and an actual Linux ARM64 container run. It does not attest current worktree binaries, real OCR quality/throughput, a kernel boundary or production approval.

## Scope and invocation

`scripts/parser-boundary-smoke.py` accepts local application/parser references, resolves them through selected image metadata to immutable local image IDs, and requires matching Linux ARM64 or AMD64 platforms. It never pulls, builds, publishes or runs an OCR/model workload. Historical supplied images are permitted and always reported with `current_worktree_binary_attested=false`. A future current-worktree rebuild remains a separate gate.

After flag review, run against explicit reviewed local identities:

```sh
python3 scripts/parser-boundary-smoke.py \
  --parser-image sha256:REVIEWED_LOCAL_PARSER_ID \
  --app-image sha256:REVIEWED_LOCAL_APPLICATION_ID \
  --output artifacts/parser-boundary-UNIQUE
```

The output directory must not exist. A fresh randomized `rust-ocr-…smoke…` namespace is generated unless explicitly supplied. Existing namespace labels, exact network/volume names and matching container names are refused before creating a container. Output uses a private0700 directory and0600 files under a restrictive process umask; only fixed summary JSON reaches stdout. Bounded raw Docker diagnostics remain private in numbered logs. No `.env`, expanded Compose, secret values, real uploads or raw OCR are read.

## Actual matrix

The harness reads the public `compose.production.yaml` source and rejects changes to the relevant literal parser UID, network, root filesystem, capabilities, CPU/memory/PID, socket volume, scratch flags and environment contract. It uses Docker CLI directly so no other production service or secret declaration is loaded.

Only four new containers are created:

| Container | Bound and purpose |
|---|---|
| Socket initializer | Network none, UID0, onlyCHOWN capability, read-only root,64MiB memory+swap,0.25CPU,16PIDs; applies the existing0710/parser-owned directory contract on a fresh volume |
| Parser | Exact supplied parser image, UID10002/GID10001, init, network none, read-only root, no-new-privileges, all capabilities dropped,2CPUs,2GiB memory/4GiB memory+swap,64PIDs;1GiB private scratch tmpfs with noexec/nosuid/nodev |
| Positive control | Same parser image/UID and dropped capabilities,64MiB memory+swap,0.25CPU,16PIDs; fresh internal bridge and only synthetic read-only sentinel bind mounts |
| Canary | Existing CPU-only `mock-vllm` executable from the supplied app image,64MiB memory+swap,0.25CPU,16PIDs, fresh internal bridge; no host ports, credentials or inference calls |

Limits are cgroup/scratch ceilings, not an assertion that2GiB/1GiB is allocated or measured. The parser performs health checks only. API, worker, PostgreSQL, Redis, real layout and real VLM services are never started.

Seven checks must all complete:

1. Selected container metadata confirms the parser identity, network none, UID, read-only root, no-new-privileges, dropped capabilities, exact resource ceilings, init, no published ports and only the socket/scratch mounts.
2. The same-image positive control connects to the synthetic canary's actual numeric IPv4 address/port8000 and reads HTTP200 from `/health`.
3. The actual parser attempts TCP to that same live address. Connection failure must produce the fixed `connect_denied` marker and exit42. Timeout124, missing tools126/127, generic failures or a successful connection are not accepted as denial. A Docker configuration inspection alone cannot satisfy this active check.
4. The control can read the synthetic sibling secret/model/input sentinel mounts; these paths and Docker socket paths are absent in the actual parser. The parser's UID/GID, zero effective capabilities, no-new-privileges and scratch/socket directory ownership/modes are checked. A root-UID write attempt to world-writable `/tmp` must fail under the read-only root filesystem.
5. Interpreting a tiny scratch fixture through Bash succeeds, while direct execution fails126 under the noexec mount. This is mount behavior, not a claim that no interpreter can execute scratch content.
6. UID10001/GID10001 can perform parser health over the shared socket while a direct attempt to create a socket-directory sentinel fails; the socket remains present.
7. The same canary positive HTTP check succeeds again after denial/mount checks, excluding a dead endpoint as evidence of network isolation.

The active-egress scope is controlled **IPv4 TCP to one fresh synthetic canary**, alongside the exact network-none metadata. It is not an internet-wide, UDP/IPv6, container escape, seccomp exploit or host-kernel penetration assessment. Missing Bash/timeout is an inconclusive tool failure, never security success.

## Resource ownership and cleanup

Every run has an independent owner UUID in addition to its project label. Successful Docker create calls return container IDs; probes, inspections and cleanup use these immutable IDs. A name collision is not registered as owned. Cleanup verifies the independent owner label before removing an exact container/network ID, so another run with the same project name cannot be removed by project-name matching.

`docker volume create` is idempotent, so its returned name does not prove creation. The harness reads its owner UUID and refuses a raced foreign volume before initializer execution or any mount. Socket volumes are **never deleted**, and a proven-owned retained volume name/owner UUID is recorded in the report.

Commands have individual deadlines and a hard64KiB combined stdout/stderr cap while streaming. The normal phase has140seconds; cleanup/baseline verification has its own30second allowance even after the normal deadline. Timed-out local CLI process groups are killed/reaped. Each known owned resource receives best-effort cleanup despite another cleanup failure. The result fails if cleanup is incomplete or any previously running container ID is absent afterward; no existing container is restarted or otherwise changed to hide failure.

## Unit verification

Focused standard-library tests use fake Docker calls or redirect the process runner to tiny synthetic Python children. They never invoke Docker. Coverage includes policy drift, extra secret/host mounts and capabilities, wrong observed network/resource limits,126/127/124 misclassification, missing positive controls, foreign named volume/container refusal, failed create ownership, owner-UUID replacement races, cleanup continuation after a timeout, idempotent foreign volume creation, platform mismatch, immutable-ID execution, output overflow and bounded process-group termination. A static workflow test checks that CI invokes the harness only after image building and the packaged drill with a new evidence path.

```sh
python3 -m unittest discover -s scripts -p test_parser_boundary_smoke.py
```

Result: **9 focused tests pass**, and the full Python script suite passes **42 tests**. Python compilation, workspace formatting and whitespace checks pass. No-new-comments and block-comment guards checked 130/105 source files with zero violations. Workflow YAML and 16 shell steps passed syntax checks without executing their commands. No existing maintained symbol was edited by the harness implementation: GitNexus context inspected the nested monitoring boundary helper and existing smoke refusal workflow before this standalone implementation. The workflow file returned UNKNOWN/not-indexed; its images-job caller and artifact paths were reviewed manually. New functions had no prior indexed implementation to analyze; a final index refresh/change detection is recorded in the session continuation evidence.

## Observed container execution

The coordinator ran the matrix using the already-local Linux ARM64 parser image `sha256:ac44b2b63f7c65ad459688f0d23922c1979ed1aa46252de31333a97409069555` and application image `sha256:037dee9d1c00e0e5e2d68e55ec404ff81372112e0d076041c71211d0c06ed42e`. These are the earlier monitoring-reviewed images; current source changes were not built into them. Both identities were resolved/checked before creation. No image was pulled, built or published.

All seven required checks passed. The parser denied IPv4 TCP to the same numeric address whose HTTP health endpoint was reachable from the positive control before and after the denial. Synthetic sibling mounts were readable only in the control; the parser passed exact resource/mount/privilege checks, scratch noexec/interpreter controls and worker-UID socket health without directory creation rights. The report explicitly retains `current_worktree_binary_attested=false`.

Cleanup removed all four owned containers and the fresh internal network, verified by independent owner-label queries afterward. The single socket volume remains with its owner UUID recorded in private evidence; no volume was deleted. All 13 previously running container IDs remained running. A second invocation using the retained namespace and a new output path was rejected before creating the new output or any resource, proving the existing-namespace guard against actual retained Docker state as well as fake-CLI fixtures.

Private selected metadata, fixed report, refusal evidence and bounded logs are under ignored `artifacts/production-rgb-boundary-2026-10-16/`. An independent source/flag review found no blocking cleanup or verification defect before execution. The CI images job now resolves its newly built tags to exact IDs and invokes this same harness after its packaged drill; only the report is added to uploaded evidence. This is configured CI coverage, not proof that the new step ran on GitHub.

The [Docker network-none documentation](https://docs.docker.com/engine/network/drivers/none/) describes the loopback-only namespace underlying this check. The [tmpfs documentation](https://docs.docker.com/engine/storage/tmpfs/) distinguishes mount flags from memory isolation: tmpfs counts toward cgroup memory and may be swapped. Neither a scratch ceiling nor this noexec test establishes a whole-host memory guarantee or confidential non-persistence.

## Remaining acceptance

The older A06b trace remains evidence for earlier packaged PDF/image/fault checks; it explicitly states egress was inspected rather than actively attempted at that time. This subsequent run adds the defined active IPv4/mount evidence only for the supplied historical images and current checked container policy. Current-source Linux verification/rebuild, representative IPC/memory overhead, broader malformed inputs and independent boundary review remain open under the production plan.
