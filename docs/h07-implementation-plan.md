# H07 implementation plan — local canonical candidate (LOCAL-READY slice)

Status: plan reviewed before execution. External provenance (push/registry/
signing/hosted runs) remains authority-gated and is NOT attempted here.

## Baseline

- Source frozen at `2277bdf` (H00–H11 changes complete; workspace gates
  green at 329/0/91; Python 62/0; guards 0/0).
- Host: 26% memory free, 9.6/10 GiB swap used (stable, no growth during
  rounds 1–2), 266 GiB disk free, Docker build cache 4.9 GiB present.
- 13 live containers + all volumes preserved; no `down -v`, no prune.

## Scope of THIS slice (bounded, honest)

1. **Canonical build**: `docker build` the application, parser-runtime and
   backup-ops targets from the Dockerfile at HEAD with tags
   `ocr-service:h07-local`, `ocr-service:parser-h07-local`,
   `ocr-service:backup-h07-local` (owner-labelled, never overwriting
   existing tags). Declared budget: one build run, ≤90 minutes wall,
   build uses the existing cache; stop condition: any OOM/swap spiral
   (kill by exact container, record partial state).
2. **Verification-only target**: run the `parser-verification` stage
   tests inside the image (the Dockerfile's own verification path) —
   app+parser contract on the packaged binary set.
3. **Exact-image boundary checks**: `scripts/parser-boundary-smoke.py`
   against the freshly built parser image (local current-source run; the
   historical Linux ARM64 matrix stays historical).
4. **Scan/lock**: Trivy is not installed on this host; local scan/SBOM
   receipts are NOT_RUN here (they run in hosted CI per H06 wiring).
   `release-manifest.py` inventory + lock IS runnable locally against the
   built images (no network): record image IDs, digests, platform,
   source binding — this is the local candidate lock.
5. **Rehearsal**: the packaged pipeline drill (`production-smoke.sh`)
   against the new images in a fresh UUID namespace with the strict
   capture controls (H05) — slow-model profile, parser outage, parser
   restart; 18-check monitoring drill only if time/memory budget remains
   (otherwise NOT_RUN recorded, not silently skipped).

## Invariants

- No push, no registry, no live cutover, no volume deletion, no
  `down -v`/prune; cleanup removes only the smoke project's containers/
  networks by exact name; volumes retained per script contract.
- Existing tags (`:local`, `:p1`, `:a05`, …) untouched; new tags only.
- Host pressure guard: abort the build if free memory < 12% or swap grows
  > 400 MiB during the run (checked between stages).
- Production W1/R1/L1 config unchanged; secrets synthetic per script.

## Acceptance (local candidate, explicitly NOT production approval)

- Built image IDs recorded; parser-verification tests PASS; boundary
  smoke PASS on the new parser image; release inventory written with
  `registry_publication_verified: false`; smoke scenarios PASS with
  original rows preserved; trace with all receipts/limitations.
- Anything not run = NOT_RUN with reason. No capacity/quality claim.

## Rollback

Delete the new image tags by exact name (docker rmi of the new IDs only);
no other state changes.

## Declared public artifact retrieval (digest-pinned bases)

The canonical build requires two digest-pinned public bases not present
locally. Declared per the harness (source = Docker Hub official images,
identity = the Dockerfile's own sha256 pins, licenses = respective
upstream licenses, no mutability — digest pulls only):

1. `rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6`
2. `debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587`
3. `postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336` (backup-ops base; already local — verify digest match)

Pull budget: ≤3 fetches, ≤15 minutes; no other tags/versions fetched; a
digest mismatch aborts before any build step. No private data leaves the
host; the fetch is the only network egress in this packet.
