# H11 execution trace — machine-only v2 sidecar exporter

Date: 2026-10-18. Packet H11. Scope: `ocr-eval` new `v2_export` module +
`export-v2` CLI + 4 integration tests. Loader/scorer untouched; v1 outputs
untouched.

## Status

- Implementation: PASS (supported scope; cards/region-precise geometry
  explicitly abstained — see limitations).
- Verification: PASS (4 new integration tests incl. loader/scorer
  acceptance + tamper rejection; workspace 328/0/91; clippy/fmt/guards).
- Measurement: N/A (structural export; no accuracy claim — the sidecar
  passing negative controls does not prove OCR quality).
- Adoption: N/A (offline tooling; real-data v2 scoring still waits on the
  H10 independent corpus).

## Change

`ocr-eval export-v2 EXTRACTION_JSON OCR_RESULT_JSON --source-sha256 H
--output SIDEcar.json` builds an `ocr-eval-prediction/2.0.0` sidecar from
two STORED machine artifacts only — the exporter has no manifest/gold
parameter at all, so no-gold-access is structural, not procedural.

- **Pages**: from the OCR result's `pages` (number, width, height);
  rotation 0 and CropBox `[0,0,w,h]` asserted from the stored render
  metadata contract; a page without usable dimensions ABSTAINS explicitly
  (`abstained_pages`, and the export fails if no page survives).
- **Entities**: `doc_{id}` per logical document + `party_{doc}_{role}` per
  extraction role (landlord/tenant/identity_holder from real v1 output).
  IDs use only the v2-legal charset.
- **Transcripts**: one per `source_artifact` (exact text, real page
  anchor). Anchors are full-page rectangles at `block` granularity — real
  page geometry, never invented regions; region-precise geometry needs the
  future layout-registry adapter and is recorded as a limitation.
- **Observations/Fields**: v1 `value_status` mapping — found→present
  observations (one per evidence, origin first_pass), missing→empty field,
  unreadable/placeholder→single state observation, conflict/ambiguous→all
  candidates exported (origin recovery preserved where v1 marks it),
  conflicts stay unselected. Field paths map `{doc}.parties.{role}.{field}`
  and `{doc}.{field}`; the extraction rules already emit exactly the v2
  field enum (`name`…`property_area`); anything outside (e.g.
  `signing_date`) lands in `unsupported_fields`, never silently dropped.
- **Report**: deterministic JSON with counts, unsupported list, abstained
  pages and three fixed limitations.

## Tests (`crates/ocr-eval/tests/v2_export.rs`)

1. `exported_sidecar_passes_the_v2_loader_and_scores` — the exported
   sidecar, embedded as the prediction of a matching synthetic gold
   manifest, is accepted by the REAL loader (validate_pages +
   validate_graph with prediction=true) and scores with 100% entity
   matching.
2. `tampered_sidecar_quote_is_rejected_by_the_loader` — forging
   `visible_text` without recomputing the evidence hash makes `score`
   fail with "evidence hash/UTF-8 byte span mismatch".
3. `export_is_deterministic` — byte-equal sidecars across runs.
4. `page_without_dimensions_abstains_explicitly` — missing width ⇒
   explicit abstain failure, no invented geometry.

Failed attempts preserved: entity IDs with `:` failed the v2 `id()`
charset (fixed to `_`); wrong normalization constant (`unicode-nfc-v1`
vs `nfc-typed/1.0.0`) caught by the loader; tamper test initially called
`load` (predictions are validated at score time, not load); clippy
rejected `.err().expect()`.

## Verification receipts

- `cargo test --frozen -j 1 -p ocr-eval` → 8 unit + 12 benchmark + 13
  scoring + 27 v2 + **4 v2_export**, 0 failed.
- `cargo test --frozen -j 1 --workspace` → **328 passed / 0 failed / 91
  ignored** (baseline 315 ordinary + 3 H01 + 4 H02 + 2 H04 + 4 H11).
- `cargo clippy --frozen -j 1 --workspace --all-targets -- -D warnings`
  clean; `cargo fmt --all -- --check` clean; comment guards 134/0 and
  107/0; 62 Python tests OK.
- Manual CLI run on synthetic fixtures produced a loader-valid sidecar
  with `signing_date` correctly abstained.

## Rollback

Remove `v2_export.rs`, its tests, the module registration and the CLI
branch. No data/schema impact.

## Remaining gates

Region-precise geometry adapter (layout-registry binding); card-entity
scope; real-corpus v2 scoring needs H10 independent gold (owner/data
gated). No accuracy or quality-qualified claim is made here.
