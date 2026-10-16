#!/usr/bin/env bash
# Disposable packaged smoke of the production topology with a fixture model.
# It verifies protocol, permissions and recovery paths of the exact images under
# test. It does not certify OCR quality or real-model capacity.
#
#   PROD_IMAGE=ocr-service:x PROD_PARSER_IMAGE=ocr-service:parser-x \
#     scripts/production-smoke.sh
#
# SMOKE_MOCK_DELAY_MS (default 12000) must exceed the parser's 10 s frame
# timeout so idle gaps between parser requests are exercised.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${PROD_IMAGE:?set PROD_IMAGE to the application image under test}"
: "${PROD_PARSER_IMAGE:?set PROD_PARSER_IMAGE to the parser image under test}"
project=${SMOKE_PROJECT:-rust-ocr-prod-smoke}
if [[ "$project" != *smoke* ]]; then
  echo "SMOKE_PROJECT must contain 'smoke' so a live project is never targeted" >&2
  exit 2
fi
out=${SMOKE_OUTPUT_DIR:-$PWD/artifacts/$project}
export PROD_SECRETS_DIR=${PROD_SECRETS_DIR:-$out/secrets}
export SMOKE_MOCK_DELAY_MS=${SMOKE_MOCK_DELAY_MS:-12000}
export PROD_API_PORT=${PROD_API_PORT:-8580}
export PROD_API_METRICS_PORT=${PROD_API_METRICS_PORT:-9591}
export PROD_WORKER_METRICS_PORT=${PROD_WORKER_METRICS_PORT:-9592}
export PROD_IMAGE PROD_PARSER_IMAGE
fixture=crates/ocr-worker/tests/fixtures/two_pages.pdf
compose=(docker compose -p "$project" --env-file /dev/null
  -f compose.production.yaml -f compose.production.smoke.yaml)

mkdir -p "$out"
if [[ ! -d "$PROD_SECRETS_DIR" ]]; then
  parent=$(cd "$(dirname "$PROD_SECRETS_DIR")" && pwd)
  docker run --rm --network none --user "$(id -u):$(id -g)" \
    --mount "type=bind,src=$parent,dst=/provision" \
    --entrypoint ocr-secrets "$PROD_IMAGE" init "/provision/$(basename "$PROD_SECRETS_DIR")"
fi

step() { printf '\n== %s\n' "$*"; }
sql() { "${compose[@]}" exec -T postgres psql -U ocr_admin -d ocr -Atc "$1"; }
latest_job() { sql "SELECT status || ' ' || attempts FROM jobs ORDER BY created_at DESC LIMIT 1"; }
expect_latest() {
  local actual
  actual=$(latest_job)
  if [[ "$actual" != "$1" ]]; then
    echo "expected latest job '$1', got '$actual'" >&2
    exit 1
  fi
  echo "latest job: $actual"
}
client() { # output-name profile expected-pages
  docker run --rm --network "${project}_default" --user 10001:10001 --read-only \
    --tmpfs /tmp:size=16m,mode=1777 \
    --mount "type=bind,src=$PWD/$fixture,dst=/input/two_pages.pdf,readonly" \
    --mount "type=bind,src=$PROD_SECRETS_DIR/api_token,dst=/run/secrets/api_token,readonly" \
    --env OCR_API_TOKEN_FILE=/run/secrets/api_token --env OCR_API_URL=http://api:8080 \
    --env CLIENT_TIMEOUT_SECONDS=300 --env "EXPECTED_PAGES=$3" --env "EXTRACTION_PROFILE=$2" \
    --entrypoint service-client "$PROD_IMAGE" /input/two_pages.pdf >"$out/$1.json"
}

step "start disposable project $project (mock delay ${SMOKE_MOCK_DELAY_MS} ms)"
"${compose[@]}" up -d --wait postgres redis parser
"${compose[@]}" --profile ops run --rm --no-deps admin ocr-admin roles bootstrap
"${compose[@]}" run --rm --no-deps migrate
"${compose[@]}" --profile ops run --rm --no-deps admin ocr-admin roles grant
"${compose[@]}" up -d --wait api worker mock-vllm

step "1/3 two-page PDF with extraction profile across model latency > parser frame timeout"
SECONDS=0
client profile-slow-model vi_rental_contract_v1 2
echo "elapsed ${SECONDS}s"
jq -e '[.pages[].page_number] == [1,2]
  and .execution.document_process_policy.parser.protocol_version == 2
  and .extraction != null' "$out/profile-slow-model.json" >/dev/null
expect_latest "succeeded 1"

step "2/3 parser stopped: claims pause without spending attempts"
"${compose[@]}" stop parser
client parser-outage none 2 &
pending=$!
sleep 15
expect_latest "queued 0"
"${compose[@]}" start parser
wait "$pending"
expect_latest "succeeded 1"

step "3/3 parser restarted during a job: one attempt lost, retry succeeds"
client parser-restart none 2 &
pending=$!
sleep 4
"${compose[@]}" restart parser
wait "$pending"
expect_latest "succeeded 2"

step "parser health and worker gate metrics"
"${compose[@]}" exec -T parser parser-renderer --healthcheck
curl -fsS -H "Authorization: Bearer $(cat "$PROD_SECRETS_DIR/metrics_token")" \
  "http://127.0.0.1:$PROD_WORKER_METRICS_PORT/metrics" |
  grep -E '^ocr_events_total\{event="parser_unavailable"\}|^ocr_job_attempts_total|^ocr_operational_(oldest_eligible|database_bytes)' |
  tee "$out/worker-metrics.txt"

if [[ "${SMOKE_KEEP:-0}" != 1 ]]; then
  step "stop project (volumes kept for inspection)"
  "${compose[@]}" down
fi
echo "Packaged smoke passed: slow-model profile job, parser outage gating, parser restart recovery."
