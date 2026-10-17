# H11b execution trace — region-precise v2 sidecar geometry

Date: 2026-10-18. Packet H11b. Target: `ocr-eval` `v2_export.rs` region
binding + tests. No worker/schema change; uses only already-stored result
fields (`regions` per page: content/bbox_2d/polygon in the 0..1000 frame
+ page width/height).

## Status

- Implementation: PASS.
- Verification: PASS (4 v2_export tests incl. explicit region-polygon
  assertion; 63 ocr-eval tests; workspace 329/0/93; clippy/fmt/guards).
- Measurement: N/A (structural).
- Adoption: N/A (offline tooling).

## Change

Observations now bind to REAL region geometry when the evidence quote
uniquely matches exactly one stored layout region's content:

- Region registry built from the result's per-page `regions`: polygons
  scaled from the 0..1000 frame to stored page pixels (`x*width/1000`,
  clamped), validated for vertex count and non-degenerate area (shoelace
  > 0.01); invalid/degenerate regions are skipped (fallback, never
  invented).
- `bind_region(page, quote)`: byte-substring search across the page's
  region contents. Exactly one match → region transcript
  (`{page}_r{index}`) + region-polygon anchor + quote span within the
  region text; the observation anchor equals its transcript anchor
  (contract rule). Zero matches → page-rect fallback. Multiple matches →
  page-rect fallback + `ambiguous_binds` counter (ambiguous binding is
  not honest geometry).
- Region transcripts are added lazily and deduplicated.
- Report adds `region_bound_observations`, `page_anchored_observations`,
  `ambiguous_binds`; the limitations text now states the truthful binding
  semantics. Entities keep page-rect anchors (a person is not a region).

## Tests (fixtures extended: 3 regions with known polygons/contents)

1. Loader/scorer acceptance — unchanged, plus the explicit assertion:
   the `name` observation carries region 0's polygon scaled to page
   pixels ([0,0]→[500,60] on the 1000×600 page), NOT the page rect.
2. Ambiguity: the `id_number` quote appears in two region contents →
   page-rect fallback + counted (`ambiguous_binds >= 1`).
3. Absent quote (`rent` line) → page fallback, counted.
4. Tamper rejection + determinism — unchanged and still green.

Failed attempts preserved: fixture initially expressed "ambiguity" as a
substring that made the bind unique (test caught the design error — the
quote must appear verbatim in two regions); tuple-return complexity
rejected by clippy (factored into a `RegionBind` struct); several
edit-anchor mismatches from rustfmt reflow recorded as churn, final state
verified by full gates.

## What this closes

The annotation guide's remaining adapter gap ("lấy geometry từ
native/layout registry thật") for the SUPPORTED contract scope: v2
sidecars now carry region-precise anchors wherever the machine's own
layout output unambiguously supports them, with honest page-true
fallback and explicit counts otherwise. Cards remain out of scope;
semantic attribution still requires independent gold (unchanged gate).

## Rollback

Revert `v2_export.rs` + tests; stored outputs untouched.
