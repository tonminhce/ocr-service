# H06 execution trace — complete release inventory/scan/CI identity (DR05/DR06/DR15)

Date: 2026-10-18. Packet H06. Scope: CI workflow scan loop, release-manifest
set-equality, postgres digest pin, vLLM wheel hashes, MSRV/action policy
documentation. No runtime service code changed.

## Status

- Implementation: PASS.
- Verification: PASS (47 Python tests incl. 5 new; YAML/shell syntax; compose
  image resolution; comment guards). Rust untouched — no rebuild required;
  workspace gates run at commit per harness.
- Measurement: N/A (identity/coverage packet).
- Adoption: GATED — CI changes take effect on the next hosted run; no push
  performed (explicit authority required). Local evidence only.

## Changes

1. **DR05 scan-set equality**: CI `SBOM and vulnerability scan` now scans the
   pinned PostgreSQL and Redis runtime images alongside app/parser/backup/
   monitoring. `release-manifest.py` gained `--scan-receipts-dir` +
   `verify_scan_receipts(images, dir)`, failing closed on receipts missing
   for a deployed image OR receipts naming a non-deployed image (set
   equality, both directions). Fail-on-fixable-CRITICAL and visible HIGH
   reports unchanged; inactive-RSA exception untouched.
2. **DR06 postgres digest pin**: `postgres:18` →
   `postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336`
   in `compose.production.yaml`, CI services, CI pre-inventory pulls and the
   release manifest. Digest verified as the identity the live local stack
   runs (`docker manifest inspect` + local image RepoDigests). Standalone
   dev fixtures (`compose.yaml`, `compose.verify.yaml`) deliberately left
   on the mutable tag (dev-only, not the deployment set).
3. **DR15 vLLM wheel hashes**: installer downloads both pinned wheels to a
   mktemp dir, verifies SHA-256 (`shasum -c`) before `uv pip install`, and
   cleans up via trap. Hashes fetched 2026-10-18 from the exact pinned URLs
   (vllm 0.31.0 cpu cp312 macosx_11_0_arm64:
   `21378516…dd686`; vllm_metal 0.31.0 cp312 macosx_15_0_arm64:
   `e2ab3129…acf6e`); downloads were verified as real wheel archives before
   hashing; temp files removed. No version changes.
4. **DR06/DR15 documentation**: `docs/release-operations.md` now records
   the scan-set-equality contract, digest-vs-rebuild distinction, the
   layout-export transitive-lock gap, the action-pinning policy (SHA pins =
   reviewed updates; current mutable tags recorded truthfully), and the
   MSRV truth: `rust-version = 1.89` is an untested source-compatibility
   floor (evidence covers 1.98.1/1.99.0 only); an MSRV CI leg needs an owner
   decision. `tools/layout-export-requirements.txt` transitive lock remains
   an explicit open gap (no silent upgrade or unauthorized fetch).

## Tests

New in `scripts/test_release_manifest.py` (8 total now):
- `test_runtime_images_are_digest_pinned` — manifest image literals all
  carry `@sha256:`.
- `test_ci_scan_loop_covers_every_inventoried_runtime_image` — non-app
  manifest images appear in the CI scan loop text.
- `ScanReceiptTests` × 3 — exact receipt set passes; missing receipt fails
  closed with "missing receipts"; extra receipt fails with "receipts
  without a deployed image".

Results: `python3 -m unittest discover -s scripts -p 'test_*.py'` → 47
passed / 0 failed. `python3 -c yaml.safe_load(...)` for workflow + compose
OK. `bash -n install-vllm-metal-macos.sh` OK. `docker compose config
--images` resolves both runtime digests. Comment guards 0/0.

## Rollback

Revert the five edited files (workflow, manifest script, its tests, compose
pin, installer, docs). No data/schema/runtime impact; the compose pin is
identity-only (same digest the stack already runs).

## Remaining gates (unchanged, honest)

- Hosted execution of the extended scan loop requires push authority.
- Action SHA pinning and MSRV CI leg: recorded owner decisions.
- Layout-export transitive lock: open reproducibility gap.
- No provenance/registry claims made by these local receipts.
