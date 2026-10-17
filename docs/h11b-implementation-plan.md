# H11b implementation plan — region-precise v2 sidecar geometry

Status: plan reviewed before edits. Target: `ocr-eval` `v2_export.rs`
upgrade + tests. No worker/schema change; uses ONLY already-stored
result fields (`regions` per page with content/bbox_2d/polygon in the
0..1000 frame, page width/height).

## Problem

The H11 exporter anchors every transcript/observation at the full-page
rectangle (block granularity) — real but coarse. The annotation guide's
remaining adapter gap: bind evidence to REAL region geometry without
inventing boxes.

## Design (deterministic binding, fail-closed)

For each page in the OCR result:
1. Collect formatted regions with non-empty `content` and a valid
   polygon; scale polygon vertices from the 0..1000 frame to stored page
   pixels: `x_pixel = x_1000 * width / 1000` (f64, clamped to page).
   Granularity stays `block` (regions ARE layout blocks; never split to
   word).
2. For each extraction evidence (artifact_id, quote, span): search the
   page's regions for those whose `content` contains the quote as a
   substring (byte search).
   - Exactly one match → emit the observation with THAT region's
     polygon anchor and a region-scoped transcript
     (`{artifact_id}_r{index}`, text = region content, anchor = region
     polygon), evidence span = quote offsets within the region text +
     sha256 of the quote. Observation anchor == transcript anchor
     (contract rule).
   - Zero matches → fall back to the page-rect anchor (current
     behavior), recorded in the report as `page_anchored_observations`.
   - Multiple matches → fall back to page-rect (ambiguous binding is
     not honest geometry), recorded as `ambiguous_binds`.
3. Report gains `region_bound_observations`, `page_anchored_observations`,
   `ambiguous_binds` counts; limitations text updated (region-precise
   where uniquely bound; page-true fallback otherwise).
4. Entities keep page-rect anchors (a person is not a region); only
   observation/transcript anchors upgrade.

## Invariants

- No invented boxes: region polygons come verbatim from stored layout
  output, only frame-scaled with clamping.
- Polygon validity: scaled polygons must satisfy the v2 anchor contract
  (3..16 vertices, convex, inside page). Region polygons are
  box-derived convex quadrilaterals in the 0..1000 frame — scaling
  preserves convexity; assert inside-page after clamping; degenerate
  (zero-area) → skip region (fallback).
- Determinism: same inputs → identical sidecar (region order fixed by
  index).
- Transcript anchor == observation anchor enforced as before.

## Tests (extend `tests/v2_export.rs`)

1. Region-unique quote → observation anchor == region polygon (scaled),
   transcript per region; loader/scorer acceptance as before.
2. Quote absent from every region → page-rect fallback + counted.
3. Quote in two regions → page-rect fallback + `ambiguous_binds`.
4. Degenerate region polygon (zero area after scaling) → skipped,
   fallback, counted.
5. Determinism re-check.

Existing fixtures extended: OCR-result fixture gains 2-3 regions with
known polygons and contents; extraction evidence quotes crafted to hit
unique/absent/duplicate cases.

## Budget

Library tests only; ≤5 s each; no services. Exporter diff ≤120 lines.

## Acceptance / rollback

All v2_export tests green (new + existing); workspace gates; comment
guards. Rollback: revert the exporter + tests; stored outputs untouched.
