#!/usr/bin/env bash
# One-time offline artifact build. The deployed detector/consumer are Rust.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v uv >/dev/null || { echo "Install uv: https://docs.astral.sh/uv/getting-started/installation/" >&2; exit 1; }
force=false
case "${1:-}" in '' ) ;; --force) force=true ;; *) echo "Usage: bash scripts/setup-layout-model.sh [--force]" >&2; exit 1 ;; esac
model_dir=${LAYOUT_MODEL_DIR:-artifacts/models/pp-doclayout-v3}
export_runtime=${LAYOUT_EXPORT_RUNTIME:-$HOME/.local/share/ocr-service/layout-export-v1}
env_file=${OCR_ENV_FILE:-.env}

python=(uv run --no-project --python 3.12 python)
if [[ "$force" == true ]] || ! "${python[@]}" tools/layout_artifact.py verify "$model_dir"; then
  if [[ ! -x "$export_runtime/bin/python" ]]; then uv venv --python 3.12 "$export_runtime"; fi
  uv pip install --python "$export_runtime/bin/python" --torch-backend cpu -r tools/layout-export-requirements.txt
  "$export_runtime/bin/python" - <<'PY'
from huggingface_hub import snapshot_download
snapshot_download("PaddlePaddle/PP-DocLayoutV3_safetensors", revision="e4f489dc536556fc1be02b973ba06756e0d4a2ac")
PY
  # Failed exports must leave the currently verified model intact.
  mkdir -p "$model_dir"
  build_dir=$(mktemp -d "$model_dir/.export.XXXXXX")
  trap 'rm -rf "$build_dir"' EXIT
  "$export_runtime/bin/python" tools/export_pp_doclayout_v3.py --output-dir "$build_dir"
  "$export_runtime/bin/python" tools/layout_artifact.py verify "$build_dir"
  mv "$build_dir/model.onnx" "$model_dir/model.onnx"
  mv "$build_dir/manifest.json" "$model_dir/manifest.json"
  rm -rf "$build_dir"
  trap - EXIT
fi
"${python[@]}" tools/layout_artifact.py configure "$model_dir" --env-file "$env_file"
echo "Layout is ready. Run: docker compose up -d --build --wait"
