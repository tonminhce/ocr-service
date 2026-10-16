#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

images=$(docker compose --env-file /dev/null -f compose.production.yaml -f compose.monitoring.yaml --profile monitoring config --images)
prometheus=$(printf '%s\n' "$images" | grep '^prom/prometheus:')
alertmanager=$(printf '%s\n' "$images" | grep '^prom/alertmanager:')
for image in "$prometheus" "$alertmanager"; do
  if ! [[ "$image" =~ @sha256:[0-9a-f]{64}$ ]]; then
    echo "Monitoring tools must use the exact digest-pinned Compose images" >&2
    exit 2
  fi
done
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
printf '%s\n' synthetic-monitoring-validation-token-only > "$temporary/token"
chmod 0444 "$temporary/token"
common=(--rm --network none --read-only --memory 256m --cpus 1 --pids-limit 64
  --tmpfs /tmp:size=64m,mode=1777
  --mount "type=bind,src=$PWD/ops/monitoring,dst=/etc/ocr-monitoring,readonly")
docker run "${common[@]}" --entrypoint /bin/promtool \
  --mount "type=bind,src=$temporary/token,dst=/run/secrets/metrics_token,readonly" \
  "$prometheus" check config /etc/ocr-monitoring/prometheus.yaml
docker run "${common[@]}" --entrypoint /bin/promtool \
  "$prometheus" test rules /etc/ocr-monitoring/alerts.test.yaml
docker run "${common[@]}" --entrypoint /bin/amtool \
  "$alertmanager" check-config /etc/ocr-monitoring/alertmanager.yaml
