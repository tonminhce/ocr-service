# B03/F00 private assistant annotation draft

Date: 2026-10-16. Status: one user-authorized private development annotation draft completed. Independent human gold, held-out quality acceptance and a representative corpus remain open.

## Scope and provenance

The user supplied a local four-page PDF and asked the assistant to label it. The coordinator rendered and visually inspected all pages and higher-resolution card details without consulting existing OCR predictions or extracting the source text layer. The original document was not modified. A second assistant separately reviewed the appendix image; this remains assistant review and does not count as another human annotator.

Private output is under ignored `artifacts/annotation-draft-2026-10-16/`: source hash/transform metadata, page images, a JSON draft, a local HTML review table, source-specific uncertainty notes and validation summary. The PDF path, source hash, personal values, images and labels are deliberately absent from this committed trace. Private directories/files use 0700/0600 permissions. No real document or annotation is staged or committed.

The draft contains 46 fields and 54 source observations across four pages, including eight critical fields. It preserves two definite typed-date conflicts as separate source candidates, one partially unreadable address observation, two source-text variants, two blank fields and three placeholder fields. No conflicting field is automatically selected. Names/roles/source sides remain separate; identifiers keep leading zeros; uncertain characters, placeholders and units are not guessed. Displayed signature text is transcribed without asserting cryptographic validity, legal effect or face/biometric identity.

Draft polygons are explicit assistant-estimated line/block rectangles on the 150-DPI rendered pages, not verified word boxes. Source dimensions, CropBox/rotation and image hashes are recorded privately; a geometry-bound check is not a claim of exact pixel grounding. Every observation remains `needs_review` and `human_reviewed=false`.

## Gate interpretation and checks

The output identifies itself as `ocr-assistant-annotation-draft/0.1.0`, not an evaluator-v2 gold manifest. It declares `independent_human_gold=false`, `gold_import_allowed=false` and no numeric confidence. No fake human actor IDs, prediction-hidden declarations or adjudication were invented. Some document-specific fields are outside the current v2 supported vocabulary and are retained only in the development draft.

The source SHA-256 remains unchanged, all four page dimensions/transforms are recorded, all 54 observation IDs are unique and referenced, all draft polygons are within their source raster, and both typed conflicts retain at least two candidates with no selected value. The review page exposes source-specific observations and original page images locally. These structural checks and assistant visual review do not prove independent annotation accuracy.

Before B03/F00 acceptance, critical fields still need two blind human annotations and a distinct adjudicator according to the [annotation guide](annotation-guide.md). Reading this assistant draft is assisted review; that same round must not be labelled prediction-hidden. If the document is used to tune code/rules/model/prompt, it stays in development and cannot become a blind held-out test. One draft does not satisfy the 10–20 development annotation exercise or the representative pilot corpus.

The user also confirmed that the arrival window/deadline remains undecided and asked to continue mechanism verification. No capacity target, SLA, workload mix or production throughput claim was inferred from this file.
