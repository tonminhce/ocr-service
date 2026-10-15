# Audit of the suggested Rust GLM-OCR engine

Audited on 15 October 2026. The suggested [glm-ocr-rs repository](https://github.com/saravananravi08/glm-ocr-rs) was inspected at commit `c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d`, package `glm-ocr` 0.1.0. The research checkout is [artifacts/research/glm-ocr-rs](/Users/tonminh-mac/Documents/GitHub/ocr-service/artifacts/research/glm-ocr-rs/Cargo.toml). No code from this checkout is vendored or used by the service.

The library is a useful native inference experiment, but it cannot replace the course consumer while preserving the course's processing contract. This service therefore implements its own Rust PP-DocLayoutV3 adapter and region pipeline. The evidence and remaining fidelity limits are recorded in [course-layout-parity.md](/Users/tonminh-mac/Documents/GitHub/ocr-service/docs/course-layout-parity.md).

## Comparison with the actual course

The reference is the course's pinned `glmocr[selfhosted,layout]==0.1.4` wheel, whose detector, pipeline, and layout-policy files match the official 0.1.4 tag at `d947dc2dca3d04ee1d48efce3be02b708cbe26c2` by SHA-256. The course uses Qwen3.5-4B through vLLM rather than native GLM-OCR inference. See [course configuration](/Users/tonminh-mac/Documents/GitHub/production-ocr-course/client_rt_consumer/config.yaml:58) and [SDK detector](/Users/tonminh-mac/Documents/GitHub/ocr-service/artifacts/research/glm-ocr-sdk/wheel-0.1.4/glmocr/layout/layout_detector.py:60).

| Area | Suggested library at the pinned commit | Course SDK 0.1.4 / implemented Rust profile |
| --- | --- | --- |
| Recognition | Direct Candle GLM-OCR 0.9B engine; default model `unsloth/GLM-OCR` | HTTP region requests to the configured Qwen model |
| Execution | CPU, optional CUDA; no Metal feature declared | Rust layout helper with ONNX Runtime; external inference endpoint |
| Layout | PP-DocLayout-M, 640×640 input, 23 classes | PP-DocLayoutV3, 800×800 input, 25 classes |
| Region order | Coordinate sort with a 20-pixel row tolerance | Learned pairwise order from V3 |
| Geometry | Bounding boxes | Normalized boxes and mask-derived polygons |
| Tables | Text prompt plus known-column heuristics | Course's table-specific Markdown prompt |
| Graphics | Image/seal labels omitted | Course retains image/chart placeholders; optional image-text profile |
| Failures | Failed strips/regions are logged and omitted from an otherwise successful result | Rust worker propagates recognition failure into the durable job outcome |

Evidence: pinned [Cargo features](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/Cargo.toml#L15), [model loader](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/model_loader.rs#L9), [layout source](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/layout.rs#L12), [region recognition](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/lib.rs#L369), [table prompt selection](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/lib.rs#L640). The course's exact behavior is documented separately rather than inferred from README feature claims.

## Source-level defects reproduced without model inference

The repetition helpers slice strings by arbitrary byte offsets. A temporary probe using the unmodified helpers reproduced a panic for `"điện".repeat(3)` and for `"đ".repeat(53)`. These inputs exercise [prefix slicing](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/lib.rs#L713) and [suffix slicing](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/lib.rs#L729). Unicode-safe boundaries are required before adoption.

The [strip calculation](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/src/lib.rs#L223) also permits zero divisors: 28×8500 enters strip mode with `merged_w=0`; 8400×100 produces a zero stride. These are verified arithmetic paths, not claims that a loaded model was run on those images.

## Build, licensing, and adoption limits

`cargo check --locked --all-targets` passed for a pristine, isolated archive of the pinned commit on this macOS host with default CPU features and four warnings. Running Cargo directly inside the research subdirectory encounters the enclosing service workspace's membership check; isolation avoids changing upstream or adding the research package to the service workspace. CUDA compilation and real GLM model inference were not tested. A successful check establishes type/build compatibility for this target, not OCR accuracy, inference speed, or runtime operator correctness. Upstream README timings are unverified here and are not service capacity estimates.

The [package manifest](https://github.com/saravananravi08/glm-ocr-rs/blob/c6f12dd8634b7b2ba3956554c6ef6389ef39aa9d/Cargo.toml#L1) declares MIT, but the pinned Git tree has no LICENSE file. Resolve that packaging omission before redistributing upstream source. The official GLM-OCR SDK has an [Apache-2.0 license](https://github.com/zai-org/GLM-OCR/blob/d947dc2dca3d04ee1d48efce3be02b708cbe26c2/LICENSE); code and model-weight licenses must be tracked separately.

Any future native GLM engine should be introduced as a separate adapter with pinned weights, real document accuracy fixtures, complete region error reporting, bounded resource use, and measured target-device performance. That work would replace the recognition runtime; it would not establish V3 layout parity automatically. Invoice-style key/value and table heuristics do not establish a business-field extraction contract.
