# Third party notices

The application includes Rust adaptations of upstream algorithms. Changes
include fixed resource bounds, typed output and explicit failure handling.
The main repository MIT license does not replace the upstream licenses below.

- GLM-OCR SDK 0.1.4, copyright Z.ai, Apache-2.0. The layout postprocessing,
  crop rules and formatter were adapted from tag commit
  `d947dc2dca3d04ee1d48efce3be02b708cbe26c2`.
  Source: https://github.com/zai-org/GLM-OCR/tree/d947dc2dca3d04ee1d48efce3be02b708cbe26c2
  License text: `licenses/Apache-2.0.txt`.
- PaddlePaddle PP-DocLayoutV3 model and Hugging Face Transformers image
  processor, Apache-2.0. We export the official weights at revision
  `e4f489dc536556fc1be02b973ba06756e0d4a2ac`, and adapt its label mapping,
  mask threshold and polygon rules. Model:
  https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_safetensors/tree/e4f489dc536556fc1be02b973ba06756e0d4a2ac
  Processor: https://github.com/huggingface/transformers/tree/v5.17.0/src/transformers/models/pp_doclayout_v3
  License text: `licenses/Apache-2.0.txt`.
- PyTorch v2.13.0, BSD-3-Clause. Uint8 separable bicubic preprocessing in
  `crates/ocr-layout/src/preprocessing.rs` is adapted from `UpSampleKernel.cpp`.
  Copyright and complete license: `licenses/pytorch.txt`.
  Source: https://github.com/pytorch/pytorch/blob/v2.13.0/aten/src/ATen/native/cpu/UpSampleKernel.cpp
- OpenCV Douglas-Peucker initialization/cleanup, Intel license. Complete
  copyright, conditions and disclaimer are retained in
  `crates/ocr-layout/src/polygon.rs` and `licenses/opencv-approx.txt`.
  Source: https://github.com/opencv/opencv/blob/4.12.0/modules/imgproc/src/approx.cpp

The Docker image distributes Microsoft's ONNX Runtime 1.23.2 CPU library and
its bundled LICENSE and ThirdPartyNotices.txt under `/opt/onnxruntime`.
Repository notices are included under `/usr/share/doc/ocr-service`.
Cargo dependencies retain their own licenses; inspect Cargo.lock and package
metadata for the exact resolved versions. No code from glm-ocr-rs is vendored.
