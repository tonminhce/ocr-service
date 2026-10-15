#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -f .env ]]; then
  set -a
  . ./.env
  set +a
fi
runtime="${OCR_VLLM_RUNTIME:-$HOME/.local/share/ocr-service/vllm-metal-0.31.0}"
[[ -x "$runtime/bin/vllm" ]] || { echo "Run scripts/install-vllm-metal-macos.sh first." >&2; exit 1; }
: "${VLLM_API_KEY:?Set a local inference API key in .env}"
# The host runtime uses Apple's GPU; Docker Desktop does not pass Metal to a Linux container.
# Start with one sequence; benchmark a larger value against memory and tail latency.
case "${VLLM_MAX_NUM_SEQS:-1}" in
  1|2|3|4) ;;
  *) echo "VLLM_MAX_NUM_SEQS must be 1..4 for this local profile." >&2; exit 1 ;;
esac
export VLLM_METAL_MULTIMODAL_MODE=multimodal-native
export HF_HUB_OFFLINE=1
exec "$runtime/bin/vllm" serve mlx-community/Qwen3.5-4B-MLX-4bit \
  --revision 32f3e8ecf65426fc3306969496342d504bfa13f3 \
  --served-model-name Qwen/Qwen3.5-4B \
  --host "${VLLM_BIND_ADDR:-127.0.0.1}" --port "${VLLM_PORT:-8000}" \
  --max-model-len "${VLLM_MAX_MODEL_LEN:-4096}" \
  --max-num-seqs "${VLLM_MAX_NUM_SEQS:-1}" --max-num-batched-tokens "${VLLM_MAX_BATCHED_TOKENS:-2048}" \
  --gpu-memory-utilization "${VLLM_GPU_MEMORY_UTILIZATION:-0.5}" \
  --limit-mm-per-prompt '{"image":1,"video":0}' \
  --mm-processor-kwargs "{\"min_pixels\":65536,\"max_pixels\":${VLLM_IMAGE_MAX_PIXELS:-1048576}}" \
  --mm-processor-cache-gb 0.1 \
  --default-chat-template-kwargs '{"enable_thinking":false}' \
  --no-enable-prefix-caching \
  --reasoning-parser qwen3
