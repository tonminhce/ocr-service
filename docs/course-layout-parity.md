# Course layout parity and remaining limits

Audited on 15 October 2026. The Rust course profile now executes PP-DocLayoutV3, applies the course's region policy, crops regions, recognizes them through the configured OpenAI-compatible endpoint, and assembles ordered JSON/Markdown. It uses the repository's own Rust implementation, not the suggested `glm-ocr-rs` engine. The measured layout contract passed on five pages containing 93 retained regions. The separate strict numerical test failed on three pages; full numerical identity is not established.

## Reference and artifact pins

| Component | Exact reference |
| --- | --- |
| Course application | Read-only `/Users/tonminh-mac/Documents/GitHub/production-ocr-course`, root consumer profile |
| SDK | `glmocr[selfhosted,layout]==0.1.4`, extracted wheel in `artifacts/research/glm-ocr-sdk/wheel-0.1.4` |
| Official SDK tag | `d947dc2dca3d04ee1d48efce3be02b708cbe26c2`; detector, pipeline, and policy source match the wheel by SHA-256 |
| Layout weights | `PaddlePaddle/PP-DocLayoutV3_safetensors`, revision `e4f489dc536556fc1be02b973ba06756e0d4a2ac` |
| Exported graph | ONNX opset 17, 134,407,951 bytes, SHA-256 `98798e25d88318d44c68cb0d63242026b6006246bd83390365d894d459efb43c` |
| QA runtime | Torch 2.13.0, Transformers 5.17.0, ONNX Runtime 1.23.2, OpenCV 5.0.0, CPU |

The [graph manifest](/Users/tonminh-mac/Documents/GitHub/ocr-service/artifacts/models/pp-doclayout-v3/manifest.json) records its shape, outputs, and export versions. The [parity report](/Users/tonminh-mac/Documents/GitHub/ocr-service/artifacts/layout-validation/parity-report.json) records source, graph, binary, input, and course-config hashes. The course-config hash in that run is `9326ec38a1a70a894a69e76c1e913a2a7146e34a74649947790fc6e2fca9997b`. Later model, source, dependency, or configuration changes require rerunning the comparison.

## Implemented stages

The [Rust model adapter](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-layout/src/model.rs:40) consumes an 800×800 RGB float32 tensor and reads classification, boxes, learned order, and masks. This corresponds to the V3 outputs described in the [official Transformers documentation](https://huggingface.co/docs/transformers/model_doc/pp_doclayout_v3). The application keeps the course's 25-class identities even where weight metadata collapses multiple classes under a shared label.

The [Rust postprocessor](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-layout/src/postprocess.rs:55) implements the 0.3 threshold, tiny-box exclusion, NMS, large-image suppression, class-specific overlap policy, learned ordering, polygon reconstruction, and normalized 0–1000 geometry. It assigns contiguous indices after filtering. Text, tables, and formulas use the [course prompts](/Users/tonminh-mac/Documents/GitHub/production-ocr-course/client_rt_consumer/config.yaml:120) through bounded HTTP recognition. The [formatter](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/formatting.rs:1) applies label normalization, formula-number merging, text-block merging, bullet formatting, and image placeholders. Layout JSON remains spatial OCR. The optional [scoped extraction stage](structured-extraction.md) adds separate typed business fields and review issues; it is an enhancement beyond course layout behavior.

The course YAML says `use_polygon: false`, but this is an unused extra field in SDK 0.1.4. The active streaming path used by parsing always passes the returned polygon to the crop utility. The Rust implementation follows that executable behavior. Evidence: [course YAML](/Users/tonminh-mac/Documents/GitHub/production-ocr-course/client_rt_consumer/config.yaml:197), [SDK config model](https://github.com/zai-org/GLM-OCR/blob/d947dc2dca3d04ee1d48efce3be02b708cbe26c2/glmocr/config.py#L186), [SDK active crop path](https://github.com/zai-org/GLM-OCR/blob/d947dc2dca3d04ee1d48efce3be02b708cbe26c2/glmocr/pipeline/pipeline.py#L630).

## Measured contract and strict result

The [comparison tool](/Users/tonminh-mac/Documents/GitHub/ocr-service/tools/verify_layout_parity.py:82) imports the actual pinned SDK detector and policy. It skips the unrelated utility-package initializer to avoid loading unused PDF-lock dependencies; it does not substitute those algorithms. The QA harness adds observers for diagnostics, runs the reference on CPU with batch size one, and compares identical upright RGB page images. This establishes the tested page-level contract, not CUDA batching equivalence or PDF-renderer identity.

| Fixture | Retained regions | Exact retained-region contract | Strict raw `reading_order` metadata |
| --- | ---: | --- | --- |
| Public fixture | 1 | Passed | Passed |
| Private page 1 | 35 | Passed | Failed: 5 rank values differ |
| Private page 2 | 32 | Passed | Failed: 2 rank values differ |
| Private page 3 | 17 | Passed | Failed: 2 rank values differ |
| Private page 4 | 8 | Passed | Passed |
| Total | 93 | 5/5 pages passed | 2/5 pages passed |

The exact retained-region fields are `index`, `class_id`, `label`, `task_type`, `bbox_2d`, and normalized `polygon`; the emitted retained-region sequence is identical. The largest confidence-score absolute error is `1.908416290274939e-6`, below the declared `1e-4` tolerance. All five uint8 layout-resize fixtures match the reference bytes exactly, including 1×1 and 17×29 edge cases.

The SDK's public result exposes the contiguous output `index`, whereas Rust additionally preserves each detection's rank among all 300 raw queries as `reading_order`. The strict test compares this additional metadata. Its nine differences cross only unretained queries with equal order votes; the retained sequence is unchanged. Diagnostic execution of SDK postprocessing on the exported ONNX outputs preserves those differences, and float32 fold probes identify reduction/tie behavior. One page also swaps two unretained ranks during export. This supports the narrower contract result; it does not justify changing the report's overall `status: failed` or calling every parity check passed.

The fixtures cover one public page and four pages from one private document. They are not a representative accuracy benchmark. The report contains structural metadata and hashes; private recognized text must not be copied into tracked documentation.

## Image policy and fidelity limits

The course maps `image` and `chart` to `skip`, retaining their region placeholders without OCR. On private page 4 all eight retained regions are images, so course-compatible recognition skips their text. This is policy fidelity, not proof that the page is blank. The optional `OCR_RECOGNIZE_IMAGES=true` setting recognizes those crops with the text prompt and declares `course-root-sdk-0.1.4+image-text` in the result; that enhanced profile intentionally differs from the course. The [result coverage counters and warnings](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/inference.rs:209) expose attempted, skipped, abandoned, and empty-response counts.

The current application still has these measurable limits:

- PDF rendering uses Poppler/CropBox at a configured 200 DPI with a default 3500-pixel edge cap. The SDK uses PDFium at 200 DPI with the same edge cap. Font rendering and pixels can differ, so the shared-image comparison does not prove identical detections for independently rendered PDFs. See [Rust renderer](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/preparation.rs:191) and [SDK renderer](/Users/tonminh-mac/Documents/GitHub/ocr-service/artifacts/research/glm-ocr-sdk/wheel-0.1.4/glmocr/dataloader/page_loader.py:190).
- Region crops use white polygon masking, SDK resize dimensions, and JPEG quality 75 with 4:2:0 sampling. Rust resize/JPEG implementation details can produce different pixels/bytes from Pillow. The exact five-fixture preprocessing result applies to the layout input stage, not all recognition crops. See [crop implementation and boundary fixtures](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/preparation.rs:477).
- Resource limits intentionally constrain the course's unlimited PDF-page behavior. Defaults include 20 pages, 40 million decoded pixels, 128 regions per page, and 2000 recognition-crop edge pixels. A cap can change the image given to recognition; inspect result metadata and select limits for the workload. See [settings](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/settings.rs:60).
- Matching prompts and deterministic sampling do not guarantee identical Qwen text across model revisions, precision, servers, or crop pixels. Formatter golden fixtures verify known outputs; they do not prove complete document fidelity.

The [layout end-to-end report](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/layout-e2e-report.md) records recognition runs, including the strict-profile failure and optional table-input padding. `TABLE_CROP_PADDING_PIXELS=0` preserves the course crop; a nonzero value declares the enhanced `+table-padding` profile and records its preprocessing transform. These recognition extensions do not change the measured layout contract above. The [earlier full-page report](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/real-e2e-report.md) remains historical baseline evidence. Production acceptance still requires a broader labeled corpus, recognition accuracy and coverage measurements, target-server latency/throughput measurements, renderer/crop sensitivity checks, and regression checks after changing pins. No throughput or deployment readiness is inferred from the layout comparison.
