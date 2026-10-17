# H06 implementation plan — release inventory/scan/CI identity coverage (DR05/DR06/DR15)

Status: plan reviewed before edits. Targets: `.github/workflows/ci.yml`,
`scripts/release-manifest.py` (+ its Python tests), `compose.production.yaml`
postgres pin, `scripts/install-vllm-metal-macos.sh` hashes,
`tools/layout-export-requirements.txt` lock, MSRV contract documentation.

## Findings being closed (all CONFIRMED in current source)

- **DR05**: CI scans app/parser/backup/monitoring images only; PostgreSQL and
  Redis runtime dependencies are pulled (`ci.yml:153-154`) and inventoried
  (`release-manifest.py:68-69`) but never scanned/SBOM'd. Set mismatch: the
  scanned set ≠ the deployed set.
- **DR06**: `postgres:18` used as a mutable tag everywhere (CI service, pulls,
  compose.production.yaml, release manifest). Actions use mutable tags
  (`actions/checkout@v4`, `dtolnay/rust-toolchain@stable`); Trivy is already
  checksum-pinned. Workspace declares `rust-version = 1.89` but evidence only
  covers 1.98.1/1.99.0 — an untested compatibility promise.
- **DR15**: `install-vllm-metal-macos.sh` pins wheel URLs without full
  hashes; `tools/layout-export-requirements.txt` pins top-level versions only.

## Minimal changes (YAGNI-scoped)

1. **One reviewed runtime image set** (DR05): extend the CI scan loop to also
   SBOM+scan the exact pinned PostgreSQL and Redis runtime images, and extend
   `scripts/release-manifest.py` with a set-equality check: every deployed
   runtime image (app/parser/backup/postgres/redis/monitoring) must have a
   matching SBOM/scan receipt name; missing or extra receipts fail the build.
   Preserve the guarded inactive-RSA exception and visible HIGH reports.
2. **Pin postgres by digest** (DR06): replace the mutable `postgres:18` tag
   with `postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336`
   (the digest actually running in the live local stack, verified via
   `docker manifest inspect`) in `compose.production.yaml`, CI services/pulls
   and `release-manifest.py`. Redis is already digest-pinned. Document that
   apt updates inside image builds mean digest pinning gives identity
   traceability, not bit-for-bit rebuilds.
3. **Action/toolchain policy** (DR06): pin `actions/checkout` and
   `dtolnay/rust-toolchain` to exact commit SHAs with a documented update
   process in `docs/release-operations.md` (reviewed bumps only); keep
   `stable` as the tested toolchain and record the decision: the declared
   MSRV 1.89 stays a *source-compatibility floor claim* explicitly marked
   UNTESTED in the docs until an owner chooses to add an MSRV CI leg
   (adding a second toolchain leg doubles CI Rust cost — owner decision,
   recorded, not fabricated).
4. **vLLM install hashes** (DR15): add sha256 for each pinned wheel URL in
   `install-vllm-metal-macos.sh` (hashes fetched now from the pinned URLs
   only, recorded in the trace; no version upgrades).
5. **layout-export transitive lock** (DR15): generate a pip-hash-locked
   requirements format for the two pinned top-level entries using
   `pip download --hash` locally is NOT authorized (network fetch of new
   artifacts); instead record the limitation explicitly in the file and
   docs: top-level pins retained, transitive lock remains an open
   reproducibility gap with a documented owner action. (No silent upgrade.)

Alternatives rejected: hosting our own mirrored images (new infrastructure);
scanning an unlimited image list (set must equal deployment); adding an MSRV
CI leg without owner approval (cost); rewriting the install script to a new
package manager.

## Invariants

- No runtime behavior changes; compose pin change is identity-only (same
  digest as the running image).
- Scan/SBOM receipts stay per-image named artifacts; the existing
  fail-on-fixable-CRITICAL policy unchanged; HIGH visible reports unchanged.
- No migration, schema, service-code change.
- The release manifest's `scope: local_candidate_not_production_approval`
  stays; no provenance overclaim.

## Tests

- Extend `scripts/test_release_manifest.py`: a set-equality fixture —
  deployed-images vs receipts (missing receipt → error; extra receipt → error;
  exact match → pass) and postgres digest-pinning assertions.
- YAML/shell syntax validation of the edited workflow and script (no dispatch).
- Both comment guards; full Python test discovery; targeted Rust unaffected
  (docs/tooling only) — no cold rebuild needed, but workspace tests run once
  at commit time per harness.

## Budget

No containers, no image pulls beyond digest resolution (already done
locally), no hosted dispatch. Shell/Python edits ≤200 lines total. Time ≤1 h.

## Acceptance / rollback

- CI workflow parses; release-manifest tests pass locally; documented MSRV
  status is truthful; vLLM hashes recorded; gap register updated.
- Rollback: revert the five edited files; no data impact.

## Research

- Docker digest pinning semantics: manifest-list digest used (verified
  locally via `docker manifest inspect postgres:18` → same index digest as
  the running containers' image). [Docker docs on image digests]
  (https://docs.docker.com/reference/cli/image/tag/) — identity constraint,
  not rebuild reproducibility.
- GitHub Actions pinning guidance per the reviewed primary references
  (secure-use): pin to full commit SHA; still requires reviewed updates.
