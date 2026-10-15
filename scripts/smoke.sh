#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

compose=(docker compose -f compose.yaml -f compose.demo.yaml)
: "${OCR_API_TOKEN:?Set OCR_API_TOKEN or source .env before running this script}"
"${compose[@]}" up --build -d --wait
"${compose[@]}" exec -T api make-fixtures /tmp/ocr-fixtures
for fixture in fixture.png fixture.pdf; do
  pages=1
  if [[ "$fixture" == fixture.pdf ]]; then pages=2; fi
  "${compose[@]}" exec -T -e EXPECTED_PAGES="$pages" -e CLIENT_TIMEOUT_SECONDS=30 \
    api service-client "/tmp/ocr-fixtures/$fixture"
done
echo "Docker API -> PostgreSQL -> Rust worker -> mock vLLM passed for image and two-page PDF."
