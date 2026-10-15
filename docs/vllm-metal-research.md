# Qwen3.5 on this Mac

Research date: 14 October 2026. This document records upstream compatibility and runtime choices. Installation, native Metal startup, and real four-page OCR subsequently succeeded; the live test record also contains a failed synthetic image transcription. See [real-model evidence](real-e2e-report.md) for results rather than treating compatibility as an accuracy guarantee.

## Selected runtime and checkpoint

Use native macOS **vLLM Metal 0.31.0** with **vLLM core 0.31.0**. The stable Metal release was published on 13 October 2026 and requires Apple Silicon, macOS 15 or newer, and native arm64 Python 3.12. Its release supplies a prebuilt extension, so the wheel installation does not require compiling Metal or C++. [Official release](https://github.com/vllm-project/vllm-metal/releases/tag/v0.31.0).

The selected model is **`mlx-community/Qwen3.5-4B-MLX-4bit`**, pinned to revision **`32f3e8ecf65426fc3306969496342d504bfa13f3`**. It is a roughly **3.03 GB** MLX SafeTensors conversion of `Qwen/Qwen3.5-4B`, with affine 4-bit quantization and group size 64. It retains a vision configuration and image processing assets. The application-facing model alias is **`Qwen/Qwen3.5-4B`**; the actual weights are the pinned MLX quantized conversion. [Checkpoint and conversion details](https://huggingface.co/mlx-community/Qwen3.5-4B-MLX-4bit), [checkpoint configuration](https://huggingface.co/mlx-community/Qwen3.5-4B-MLX-4bit/blob/32f3e8ecf65426fc3306969496342d504bfa13f3/config.json).

The **pinned 0.31.0 support matrix explicitly lists this checkpoint for experimental native Qwen3.5 dense image inference**. Its scope is images, without video; FP8 Qwen3.5 checkpoints stay text-only. This is stronger evidence than older cached documentation that lists Qwen3.5 only in the text model table. [Pinned support matrix](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/docs/supported_models.md).

## Installation and reproducibility

The selected isolated virtual environment is:

```text
/Users/tonminh-mac/.local/share/ocr-service/vllm-metal-0.31.0
```

Repository helpers are `scripts/install-vllm-metal-macos.sh` and `scripts/serve-vllm-metal-macos.sh`. The installation pins these official wheel artifacts:

```text
https://github.com/vllm-project/vllm/releases/download/v0.31.0/vllm-0.31.0%2Bcpu-cp312-cp312-macosx_11_0_arm64.whl
https://github.com/vllm-project/vllm-metal/releases/download/v0.31.0/vllm_metal-0.31.0-cp312-cp312-macosx_15_0_arm64.whl
```

The upstream installer otherwise defaults to the development channel; `--stable` selects the latest stable release dynamically rather than pinning a historical version. Its release metadata declares the compatible core version, and its core wheel URL uses the macOS arm64 CPU build. [Pinned installer source](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/install.sh), [core version metadata](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/.github/vllm-release-tag.commit).

Metal's important dependency bounds are `mlx==0.32.1`, `mlx-vlm>=0.6.8,<0.7`, and `transformers>=5.10.4,<5.18.0`. The exact MLX version is an ABI requirement for the prebuilt extension. The project also pins `mlx-lm` to Git commit `9e6acca691e64d6d8bb808c328fcdea459099cca`. The dependency comments explain that older mlx-vlm 0.6.4 mishandled Qwen3.5 mRoPE state and garbled output. Preserve the installation's resolved package versions when recording a successful run. [Pinned dependency manifest](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/pyproject.toml).

## Conservative 16 GiB test settings

The following were initial conservative planning settings for this M4 Mac, not the final tested configuration or an upstream benchmark guarantee:

```text
VLLM_METAL_MULTIMODAL_MODE=multimodal-native
--served-model-name Qwen/Qwen3.5-4B
--host 127.0.0.1 --port 8000
--max-model-len 4096 --max-num-seqs 1
--max-num-batched-tokens 1024
--gpu-memory-utilization 0.5
--limit-mm-per-prompt '{"image":1,"video":0}'
--mm-processor-kwargs '{"min_pixels":65536,"max_pixels":262144}'
--mm-processor-cache-gb 0.1
--default-chat-template-kwargs '{"enable_thinking":false}'
--reasoning-parser qwen3
```

`multimodal-native` prevents a silent compatibility fallback to the text path. Current `auto` already keeps non-FP8 dense Qwen3.5 checkpoints on the native multimodal path. The memory utilization flag limits the serving budget; unified memory remains shared with macOS, Docker, and other applications. [Metal configuration](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/docs/configuration.md).

Qwen3.5 thinks by default. Both request-level `chat_template_kwargs.enable_thinking=false` and the server's default template kwargs can disable that; the Rust worker already sends the request setting. [Qwen model instructions](https://huggingface.co/Qwen/Qwen3.5-4B), [vLLM serve options](https://docs.vllm.ai/en/latest/cli/serve/).

The checkpoint's default image pixel ceiling is much larger than this small test budget. vLLM 0.31's Qwen image processor supports `min_pixels` and `max_pixels` overrides. With patch size 16 and spatial merge size 2, the 262,144-pixel ceiling corresponds to approximately 256 image tokens, below the 1,024-token prefill batch allowance. This is a calculation from the processor's token geometry, not measured runtime output. Increase the pixel ceiling after a successful test when assessing small-text OCR quality; downsampling can lose document details. [Pinned Qwen processing implementation](https://raw.githubusercontent.com/vllm-project/vllm/v0.31.0/vllm/model_executor/models/qwen3_vl.py).

The actual runs used a 2,048-token prefill batch and first a 1,048,576-pixel cap, then 1,572,864 pixels. The current local setup uses the latter, with thinking and prefix caching disabled. Installed dependencies include MLX 0.32.1, mlx-vlm 0.6.17, and Transformers 5.17.0. Higher resolution fixed one identifier error on the private PDF; it did not prevent hallucination on the synthetic image. This is local evidence, not a general tuning conclusion.

## Deployment boundary and validation

The Rust API, durable jobs, consumer worker, rendering orchestration, and inference HTTP client remain Rust application components. **The external inference runtime is Python vLLM with MLX/Metal**, running on the native Mac. A CUDA inference container does not provide the Mac's Metal runtime.

The existing Rust service containers can call `http://host.docker.internal:8000/v1`. Verify that Docker can reach the initial loopback-bound listener. If a broader bind is needed, configure the inference API credential and the worker's matching credential before using it.

Validation should progress through dependency/version checks, native Metal availability, `/health` and `/v1/models`, one genuine image request, then document upload through the Rust API and worker. A generated image/PDF with known text demonstrates functional OCR integration. It does not establish parity with the course's layout pipeline or production OCR accuracy.
