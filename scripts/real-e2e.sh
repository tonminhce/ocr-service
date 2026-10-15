#!/usr/bin/env bash
# Actual-model accuracy check. The running worker must use the real model.
# This script does not start/reconfigure containers or an inference server.
set -euo pipefail
cd "$(dirname "$0")/.."

for executable in docker curl jq; do
  command -v "$executable" >/dev/null || { echo "Missing required command: $executable" >&2; exit 1; }
done
: "${OCR_API_TOKEN:?Export OCR_API_TOKEN; source .env with set -a before running this script}"

compose=(docker compose -f compose.yaml)
model_base=${REAL_VLLM_BASE_URL:-${VLLM_BASE_URL:-http://localhost:8000/v1}}
expected_model=${REAL_VLLM_MODEL:-${VLLM_MODEL:-Qwen/Qwen3.5-4B}}
client_timeout=${REAL_E2E_TIMEOUT_SECONDS:-600}
if ! [[ "$client_timeout" =~ ^[0-9]+$ ]] || (( client_timeout < 1 || client_timeout > 86400 )); then
  echo "REAL_E2E_TIMEOUT_SECONDS must be 1..86400" >&2
  exit 1
fi

# URLs are saved as evidence. Refuse embedded credentials/query secrets.
validate_base() {
  case "$1" in http://*|https://*) ;; *) return 1 ;; esac
  case "$1" in *'?'*|*'#'*) return 1 ;; esac
  local authority=${1#*://}
  authority=${authority%%/*}
  case "$authority" in ''|*@*) return 1 ;; esac
}
if ! validate_base "$model_base"; then
  echo "REAL_VLLM_BASE_URL must be an HTTP(S) URL without credentials, query, or fragment" >&2
  exit 1
fi
model_base=${model_base%/}
case "$model_base" in */v1) ;; *) echo "The model base URL must include /v1" >&2; exit 1 ;; esac
model_name_lower=$(printf '%s' "$expected_model" | tr '[:upper:]' '[:lower:]')
case "$model_name_lower" in
  *qwen3.5-4b*|*qwen3_5-4b*) ;;
  *) echo "Set VLLM_MODEL/REAL_VLLM_MODEL to the exact served Qwen3.5-4B model ID" >&2; exit 1 ;;
esac

run_id=$(date -u '+%Y%m%dT%H%M%SZ')-$$
output_dir=${REAL_E2E_OUTPUT_DIR:-artifacts/real-e2e/$run_id}
mkdir -p "$output_dir"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/ocr-real-e2e.XXXXXX")
chmod 700 "$scratch"
started_at=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
phase=preflight
failure=
worker_endpoint=
worker_strategy=
identity_ok=false
api_ok=false
image_ok=false
pdf_ok=false
image_seconds=0
pdf_seconds=0
fixture_dir=/tmp/ocr-real-e2e-$run_id
fixtures_created=false

write_report() {
  local exit_code=$1
  local status=failed
  if (( exit_code == 0 )) && [[ "$image_ok" == true && "$pdf_ok" == true ]]; then status=passed; fi
  jq -n \
    --arg status "$status" --arg phase "$phase" --arg failure "$failure" \
    --arg started_at "$started_at" --arg ended_at "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    --arg model "$expected_model" --arg endpoint "$model_base" --arg worker_endpoint "$worker_endpoint" \
    --arg strategy "$worker_strategy" \
    --argjson identity_ok "$identity_ok" --argjson api_ok "$api_ok" \
    --argjson image_ok "$image_ok" --argjson pdf_ok "$pdf_ok" \
    --argjson image_seconds "$image_seconds" --argjson pdf_seconds "$pdf_seconds" \
    '{schema_version: 1, status: $status, phase: $phase, failure: $failure,
      started_at: $started_at, ended_at: $ended_at,
      expected_model: $model, model_endpoint: $endpoint, worker_endpoint: $worker_endpoint,
      ocr_strategy: $strategy,
      verification: {model_identity: $identity_ok, api_readiness: $api_ok,
        image_transcription: $image_ok, ordered_pdf_transcription: $pdf_ok},
      fixtures: [{input: "fixture.png", result: "image.result.json", pages: 1,
          expected_text: ["OCR TEST 123"], passed: $image_ok, elapsed_seconds: $image_seconds},
        {input: "fixture.pdf", result: "pdf.result.json", pages: 2,
          expected_text: ["OCR SERVICE TEST PAGE 1", "OCR SERVICE TEST PAGE 2"],
          passed: $pdf_ok, elapsed_seconds: $pdf_seconds}],
      comparison: "Case-insensitive exact token equality; punctuation and whitespace normalized. Extra words and OCR character substitutions fail.",
      limitations: ["Two simple synthetic fixtures do not establish production OCR quality or GLM layout parity.",
        "The models endpoint records advertised identity; deployment records must establish the underlying weights/runtime."]}' \
    > "$output_dir/verification.json"
}

cleanup() {
  local exit_code=$?
  trap - EXIT
  if (( exit_code != 0 )) && [[ -z "$failure" ]]; then failure="Command failed during $phase"; fi
  write_report "$exit_code" || true
  rm -rf "$scratch"
  if [[ "$fixtures_created" == true ]]; then
    "${compose[@]}" exec -T api rm -rf "$fixture_dir" >/dev/null 2>&1 || true
  fi
  if (( exit_code == 0 )); then
    printf 'Real Qwen OCR passed for image and ordered two-page PDF. Evidence: %s\n' "$output_dir/verification.json"
  else
    printf 'Real OCR verification failed during %s. Evidence: %s\n' "$phase" "$output_dir/verification.json" >&2
  fi
  exit "$exit_code"
}
trap cleanup EXIT
die() { failure=$1; printf '%s\n' "$failure" >&2; exit 1; }

# Header secrets go into a private temporary curl configuration, not argv/logs.
: > "$scratch/curl.conf"
chmod 600 "$scratch/curl.conf"
if [[ -n "${VLLM_API_KEY:-}" ]]; then
  case "$VLLM_API_KEY" in *$'\n'*|*$'\r'*) die "VLLM_API_KEY contains an invalid header character" ;; esac
  escaped_key=${VLLM_API_KEY//\\/\\\\}
  escaped_key=${escaped_key//\"/\\\"}
  printf 'header = "Authorization: Bearer %s"\n' "$escaped_key" > "$scratch/curl.conf"
fi

phase=model_readiness
if ! curl --config "$scratch/curl.conf" --fail --silent --show-error \
  --connect-timeout 5 --max-time 30 --output "$output_dir/models.json" \
  "$model_base/models" 2> "$scratch/model.stderr"; then
  die "The real inference models endpoint is unavailable; document tests were not submitted"
fi
if ! jq -e --arg model "$expected_model" \
  '(.data | type == "array") and any(.data[]; .id == $model)' \
  "$output_dir/models.json" >/dev/null 2>&1; then
  die "The inference endpoint does not advertise the exact requested Qwen3.5-4B model ID"
fi
identity_ok=true

phase=worker_configuration
worker_model=$("${compose[@]}" exec -T worker printenv VLLM_MODEL) \
  || die "The Rust worker container is unavailable"
worker_endpoint_raw=$("${compose[@]}" exec -T worker printenv VLLM_BASE_URL) \
  || die "Cannot read the Rust worker inference endpoint"
worker_strategy=$("${compose[@]}" exec -T worker printenv OCR_STRATEGY) \
  || die "Cannot read the Rust worker OCR strategy"
case "$worker_strategy" in course_layout_vlm|full_page_vlm) ;; *) die "Unknown worker OCR strategy" ;; esac
[[ "$worker_model" == "$expected_model" ]] || die "The worker model differs from the verified real model; recreate the worker with matching settings"
validate_base "$worker_endpoint_raw" || die "The worker inference endpoint cannot be recorded safely"
worker_endpoint=$worker_endpoint_raw
endpoint_lower=$(printf '%s' "$worker_endpoint" | tr '[:upper:]' '[:lower:]')
case "$endpoint_lower" in *mock*|*fixture*) die "The worker still points to a mock/fixture endpoint" ;; esac

phase=api_readiness
"${compose[@]}" exec -T api service-client --healthcheck > "$scratch/api.stdout" 2> "$scratch/api.stderr" \
  || die "The Axum API is not ready; document tests were not submitted"
api_ok=true

phase=fixtures
"${compose[@]}" exec -T api make-fixtures "$fixture_dir" > "$scratch/fixtures.stdout" 2> "$scratch/fixtures.stderr" \
  || die "Cannot generate the labeled Rust image/PDF fixtures"
fixtures_created=true
"${compose[@]}" exec -T api cat "$fixture_dir/fixture.png" > "$output_dir/fixture.png" 2> "$scratch/image-copy.stderr" \
  || die "Cannot save the exact image fixture"
"${compose[@]}" exec -T api cat "$fixture_dir/fixture.pdf" > "$output_dir/fixture.pdf" 2> "$scratch/pdf-copy.stderr" \
  || die "Cannot save the exact PDF fixture"

run_fixture() {
  local filename=$1
  local expected_pages=$2
  local result_name=$3
  "${compose[@]}" exec -T -e OCR_API_TOKEN \
    -e EXPECTED_PAGES="$expected_pages" -e CLIENT_TIMEOUT_SECONDS="$client_timeout" \
    -e IDEMPOTENCY_KEY="real-e2e-$run_id-$result_name" \
    api service-client "$fixture_dir/$filename" \
    > "$output_dir/$result_name.result.json" 2> "$scratch/$result_name.stderr" \
    || die "The $result_name job did not complete successfully; check worker/API logs without exposing credentials"
  jq -e --arg model "$expected_model" --arg strategy "$worker_strategy" \
    '.strategy == $strategy and .model == $model and (.pages | type == "array")' \
    "$output_dir/$result_name.result.json" >/dev/null 2>&1 \
    || die "The $result_name result does not identify the expected real model/strategy"
  if [[ "$worker_strategy" == course_layout_vlm ]]; then
    jq -e '(.layout_model.engine == "rust_onnxruntime")
      and (.layout_model.onnx_sha256 | test("^[a-fA-F0-9]{64}$"))
      and ((.layout | length) == (.pages | length))
      and all(.pages[]; .coordinate_system == "normalized_0_1000"
        and (.detections | length > 0)
        and all(.detections[]; (.bbox_2d | length == 4)
          and all(.bbox_2d[]; . >= 0 and . <= 1000)
          and (.polygon | length >= 3)))' \
      "$output_dir/$result_name.result.json" >/dev/null 2>&1 \
      || die "The $result_name result has no valid Rust detector/layout evidence"
  fi
}

# Compare exact words/numbers in order while allowing Markdown/punctuation/case.
normalizer='def normalized: ascii_upcase | gsub("[^A-Z0-9]+"; " ") | gsub(" +"; " ") | gsub("^ +| +$"; "");'
phase=image_inference
step_start=$(date +%s)
run_fixture fixture.png 1 image
image_seconds=$(( $(date +%s) - step_start ))
if ! jq -e "$normalizer"'(.pages | length) == 1 and .pages[0].page_number == 1
    and .pages[0].empty == false
    and (.pages[0].markdown | normalized == "OCR TEST 123")
    and (.markdown | normalized == "OCR TEST 123")' \
  "$output_dir/image.result.json" >/dev/null 2>&1; then
  die "Image OCR did not preserve the exact tokens OCR TEST 123"
fi
image_ok=true

phase=pdf_inference
step_start=$(date +%s)
run_fixture fixture.pdf 2 pdf
pdf_seconds=$(( $(date +%s) - step_start ))
if ! jq -e "$normalizer"'(.pages | length) == 2
    and .pages[0].page_number == 1 and .pages[1].page_number == 2
    and .pages[0].empty == false and .pages[1].empty == false
    and (.pages[0].markdown | normalized == "OCR SERVICE TEST PAGE 1")
    and (.pages[1].markdown | normalized == "OCR SERVICE TEST PAGE 2")
    and (.markdown | normalized == "OCR SERVICE TEST PAGE 1 OCR SERVICE TEST PAGE 2")' \
  "$output_dir/pdf.result.json" >/dev/null 2>&1; then
  die "PDF OCR did not preserve both exact page labels in page order"
fi
pdf_ok=true
phase=complete
