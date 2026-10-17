#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo "This installer requires an Apple Silicon Mac. Use the CUDA vLLM image on a Linux NVIDIA host." >&2
  exit 1
fi
command -v uv >/dev/null || { echo "Install uv first: https://docs.astral.sh/uv/getting-started/installation/" >&2; exit 1; }
runtime="${OCR_VLLM_RUNTIME:-$HOME/.local/share/ocr-service/vllm-metal-0.31.0}"
if [[ ! -x "$runtime/bin/python" ]]; then
  uv venv --python 3.12 "$runtime"
fi
wheel_dir="$(mktemp -d)"
trap 'rm -rf "$wheel_dir"' EXIT
vllm_wheel='https://github.com/vllm-project/vllm/releases/download/v0.31.0/vllm-0.31.0%2Bcpu-cp312-cp312-macosx_11_0_arm64.whl'
metal_wheel='https://github.com/vllm-project/vllm-metal/releases/download/v0.31.0/vllm_metal-0.31.0-cp312-cp312-macosx_15_0_arm64.whl'
vllm_sha256='21378516a51ff15029f71168e827b94660e21b39484eb52277166f01ac7dd686'
metal_sha256='e2ab31296f8c42d8b65c8d5f5490f160087d1070178444870a5672e44aaacf6e'
curl -fsSL "$vllm_wheel" -o "$wheel_dir/vllm.whl"
curl -fsSL "$metal_wheel" -o "$wheel_dir/vllm_metal.whl"
printf '%s  %s\n' "$vllm_sha256" "$wheel_dir/vllm.whl" | shasum -a 256 -c -
printf '%s  %s\n' "$metal_sha256" "$wheel_dir/vllm_metal.whl" | shasum -a 256 -c -
uv pip install --python "$runtime/bin/python" \
  "$wheel_dir/vllm.whl" \
  "$wheel_dir/vllm_metal.whl"
"$runtime/bin/hf" download mlx-community/Qwen3.5-4B-MLX-4bit \
  --revision 32f3e8ecf65426fc3306969496342d504bfa13f3
echo "Installed vLLM Metal 0.31.0 in $runtime and cached Qwen3.5-4B MLX 4-bit."
