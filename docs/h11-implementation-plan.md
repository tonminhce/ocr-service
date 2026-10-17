# H11 implementation plan — machine-only v2 sidecar exporter

Status: plan reviewed (hard gate satisfied); implementation next round.
Target: new `ocr-eval` CLI subcommand `export-v2` + exporter module; reads
ONLY stored machine artifacts; never gold files/IDs/human corrections.

## Problem (DR09 implementation gap, READY supported-scope design)

The v2 scorer exists with strict validation (`v2.rs`), but no live adapter
produces `ocr-eval-prediction/2.0.0` sidecars from actual worker output —
the annotation guide names this the remaining gate for machine v2
evaluation. Worker artifacts contain real geometry and provenance: the
stored OCR result JSON has `pages` (page_number, per-page width/height via
layout runner output, EXIF/render metadata), `layout` (formatted regions
with `bbox_2d`/`polygon` in the 0..1000 page frame), and the extraction
JSON has `source_artifacts` (per-page OCR text + sha256), `roles`,
`logical_documents`, `fields` with evidence (`artifact_id`, quote, utf8
span, nullable polygon) and typed `normalized_value`.

## Design (smallest opt-in exporter)

`ocr-eval export-v2 EXTRACTION_JSON OCR_RESULT_JSON --output SIDEcar.json`
new subcommand + `crates/ocr-eval/src/v2_export.rs`:

1. **Pages**: page number/width/height from the OCR result's layout
   entries (real render dims; rotation 0 + CropBox [0,0,w,h] asserted from
   the stored rendering metadata — anything absent makes the exporter
   ABSTAIN for that page, never invent).
2. **Entities**: one per `(logical_document_id, role_id)` from extraction
   roles — machine-stable IDs already exist in v1 output; card entities
   only where the v1 output actually carries card structure (current
   profile: none — supported scope excludes cards, recorded as abstained).
3. **Transcripts**: one per `source_artifact` (id, exact text, anchor from
   the artifact's page + the layout region whose formatted content matches
   the artifact region index binding, real polygon from the 0..1000 frame
   scaled to stored page pixels; granularity = block). No word-splitting,
   no invented boxes (annotation-guide invariant).
4. **Observations**: from extraction fields: state mapping
   (found→present with value; ambiguous/recovery→ambiguous with
   recovery_only reason + origin recovery; conflict fields export ALL
   candidates as separate observations, none selected), origin first_pass
   (recovery where marked), anchor/evidence from the field's evidence
   entries (transcript id = artifact_id, byte span, quote sha256 —
   validated structurally by the loader).
5. **Fields**: only the supported v2 field enum; v1 paths outside the
   enum ABSTAIN explicitly (listed in a `unsupported_fields` section of
   the sidecar's own metadata, not silently dropped).
6. `machine_only: true`, source sha256 from the extraction metadata;
   no gold access possible (exporter takes no dataset/manifest input).

**No-gold-access execution** is structural: the exporter's inputs are two
stored artifact files; it has no manifest/gold path parameter at all.

## Invariants

- Never reads gold/IDs/corrections; never fabricates polygons or word
  boxes; conflicts stay unselected; recovery observations origin=recovery;
  no numeric confidence (v2 has none).
- Loader/scorer untouched; v1 outputs untouched; additive CLI only.
- Unsupported scope abstains explicitly per guide.

## Tests (synthetic only)

- Golden small fixture: extraction+result JSONs (synthetic two-page
  example) → exported sidecar passes `v2` loader validation AND
  `score-v2` negative controls (swapped party/card/page, forged
  quote/hash/span, missing conflict candidates, unicode offsets,
  no-gold-execution).
- Abstention: page without dims → page absent + explicit abstain record;
  field outside enum → unsupported list; card entities → none exported.
- Determinism: same inputs → byte-identical sidecar.
- Unit: 0..1000→pixel polygon scaling convexity preserved (layout
  polygons are axis-box-derived; convex by construction — assert).

## Budget

Pure-library tests, no services; exporter ≤400 lines + tests; CLI ≤40.
Time: 1–2 h implementation; workspace gates at commit.

## Acceptance / rollback

Exporter produces loader-valid, scorer-accepted sidecars on synthetic
fixtures with truthful geometry and explicit abstentions; v1/v2 existing
suites unchanged; workspace + comment guards green. Rollback: remove
module + CLI registration. Gate honesty: this does NOT prove OCR accuracy
or close B03/F00 (independent human gold remains the quality gate).
