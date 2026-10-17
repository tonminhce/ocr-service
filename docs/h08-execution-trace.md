# H08 execution trace — monitoring-of-monitoring and log budgets (READY slice)

Date: 2026-10-18. Packet H08 (READY portion). Scope: three self-monitoring
alert rules + promtool fixtures, monitoring-service log budgets, runbook
and observability documentation. No new services or images.

## Status

- Implementation: PASS.
- Verification: PASS (`check-monitoring.sh` promtool config + 168 rule
  scenarios + amtool, all green; 62 Python tests; comment guards).
- Measurement: N/A (signals/controls; thresholds are recorded candidates).
- Adoption: GATED — takes effect on the next opt-in monitoring profile
  deployment; real paging/watchdog remains owner-gated.

## Scope decision (honest)

Full H08 (host/VM/native collectors, measured production template limits,
real receiver) stays partially owner-gated: a macOS host collector needs a
reviewed component decision (node_exporter sees the Docker VM, not the Mac
host; Docker-socket collection is forbidden by the worker security
boundary), and paging needs a receiver authority. Delivered READY slice
uses only metrics the pinned Prometheus/Alertmanager already export.

## Changes

1. `ops/monitoring/alerts.yaml` (+3 rules, 13→16):
   - `OCRRuleEvaluationFailing` (critical): `prometheus_rule_evaluation_failures_total`
     increase > 0 over 5 m — monitoring output untrustworthy while firing.
   - `OCRNotificationDeliveryFailing` (critical): `alertmanager_notifications_failed_total`
     increase per integration over 5 m — last delivery hop broken.
   - `OCRMonitoringDeadMan` (severity `none`): fires while
     `up{job="prometheus"} == 1`; an out-of-band watchdog must observe it
     firing — silence means the monitoring stack itself failed
     (DR07's monitoring-cannot-monitor-itself gap).
2. `ops/monitoring/alerts.test.yaml` (+5 scenarios, 163→168): firing and
   quiet boundaries for the two failure alerts; dead man fires when
   healthy and stops when the self-scrape disappears.
3. `compose.monitoring.yaml`: explicit json-file log budgets
   (10 MiB × 3 files) on prometheus and alertmanager — the DR08 template
   gap closed for the monitoring profile; production service log/resource
   budgets remain a measure-first (H09) decision, not guessed here.
4. `docs/alert-runbook.md`: three full runbook entries
   (meaning/inspect/recovery/escalate, with the out-of-band watchdog
   contract for the dead man). `docs/observability.md`: 16-alert count,
   self-monitoring explanation, log-budget note, remaining gates.

## Failed attempts (preserved)

- Dead-man fixture initially asserted non-firing at 60 s with `1x10`
  series (still 1 at 60 s) — fixture math error, corrected to `1x2 0x37`.
- Notification fixture's `integration: null` YAML-parsed to an empty label
  while the series carried the literal string "null"; promtool caught the
  mismatch; fixed to quoted `"null"`.

## Verification receipts

- `bash scripts/check-monitoring.sh` → exit 0: prometheus config SUCCESS,
  19 rules found (16 alerts + 3 recordings), all 168 scenarios pass, 0
  FAILED, amtool SUCCESS.
- `python3 -m unittest discover -s scripts` → 62/0.
- Comment guards: 132/0 and 105/0.
- `compose.monitoring.yaml` re-validated as YAML with both logging blocks.

## Rollback

Revert the four files. No runtime impact until the next opt-in monitoring
deployment.

## Remaining gates (unchanged)

Host/disk/inode/backup-freshness collectors; production API/worker/PG log
and resource budgets (H09 measure-first); real receiver/on-call/watchdog
delivery authority; threshold calibration on representative data; replica
discovery only with real replicas.
