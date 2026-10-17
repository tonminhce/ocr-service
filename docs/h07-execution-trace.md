# H07 execution trace — local canonical candidate build and rehearsal

Date: 2026-10-18. Packet H07 (LOCAL slice). External provenance — push,
registry publication, signing, hosted runs — remains authority-gated and
was NOT attempted.

## Status

- Implementation: PASS — canonical images built from the frozen source at
  `145845f` (clean tracked tree), exact-image verification/boundary/
  pipeline rehearsals run.
- Verification: PASS (parser-verification tests in-image; 7-check
  boundary matrix; 3-scenario packaged pipeline drill; release inventory
  + digest lock). Trivy scan/SBOM receipts: NOT_RUN locally (no Trivy on
  this host; the H06-wired CI loop produces them on the hosted run).
- Measurement: build wall time ~6 min parser (cache-assisted) + ~4 min
  app + ~2 min backup + ~3 min verification image; host pressure stable
  (21–26% free, swap decreased during the run).
- Adoption: NONE — `scope: local_candidate_not_production_approval`,
  `registry_publication_verified: false`. No push/cutover/live change.

## Canonical identities (built, tagged, never overwriting existing tags)

| Tag | Image ID | Size | Digest-pinned base |
| --- | --- | --- | --- |
| `ocr-service:h07-local` | `e5e743d77776…` | 243 MB | bookworm-slim `7c7b2c96…` |
| `ocr-service:parser-h07-local` | `021854b7ab9d…` | 192 MB | bookworm-slim `7c7b2c96…` |
| `ocr-service:backup-h07-local` | `261083404c97…` | 691 MB | postgres:18 `74935e72…` |
| `ocr-service:parser-verify-h07` | (verification-only) | — | rust builder `114c7a44…` |

Declared public base retrievals (digest pulls only, from the plan):
`rust:1-bookworm@sha256:114c7a44…`, `debian:bookworm-slim@sha256:7c7b2c96…`,
postgres:18 verified to match its existing pin. No mutable tags fetched.

## Receipts

1. **Parser-verification stage (in-image, release mode)**:
   `cargo test --release --locked -p ocr-worker --lib
   --test inference_contract --test layout_pipeline` →
   **112 + 20 + 14 passed / 0 failed / 1 ignored** (cgroup-explicit case,
   never counted as passed).
2. **Boundary matrix** (`parser-boundary-smoke.py`, current-source
   images): `result: passed`, 7/7 checks (runtime policy, dual canary,
   IPv4 network denial, mount sentinels, scratch noexec, worker socket
   permissions), `baseline_preserved: true` with 13 running originals,
   retained socket volume owner-recorded; honest
   `current_worktree_binary_attested: false` for the local (non-hosted)
   run.
3. **Packaged pipeline drill** (`production-smoke.sh`, fresh UUID
   project, synthetic secrets, loopback-only): scenario 1 slow-model
   profile job `succeeded 1`; scenario 2 parser outage — claims paused
   `queued 0` without spending an attempt, recovery `succeeded 1`;
   scenario 3 parser restart mid-job — one attempt lost, retry
   `succeeded 2`. Worker gate metrics captured. Project fully removed;
   volumes retained per script contract; 13 originals untouched.
4. **Release inventory + lock** (`release-manifest.py`): 5 images
   (app/parser/backup/postgres/redis) with engine digests, platform
   linux/arm64 uniform, source binding to `145845f` with
   `tracked_worktree_dirty: false`, `compose.lock.yaml` +
   `compose.backup.lock.yaml` written (pull_policy never), manifest
   sealed with sha256. Private evidence under
   `artifacts/h07-canonical-<ts>/` (ignored, never committed).

## Failed attempt (preserved)

First smoke run failed at worker startup: default metrics port 9592
already allocated by the preexisting prod-smoke stack. No containers or
originals were affected (the script refused and cleaned its own project).
Rerun with the script's own port overrides
(`PROD_API_PORT=8583/9595/9596`) passed. Lesson recorded: fresh-namespace
smokes must override the four default loopback ports whenever the
baseline prod-smoke stack is up.

## Not run (recorded, not silently skipped)

- Local Trivy scan/SBOM: NOT_RUN (tool absent on host; wired for hosted
  CI in H06).
- 18-check monitoring drill on these images: NOT_RUN this round (time/
  memory budget after the pipeline drill; next candidate step before any
  adoption decision).
- Hosted CI, registry publication, signing/provenance: authority-gated.
- No capacity/quality claim follows from these rehearsals.

## Rollback

`docker rmi` the four new tags/IDs (exact names only); delete the private
artifact directory. No other state changed.
