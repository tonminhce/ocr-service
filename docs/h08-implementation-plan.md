# H08 implementation plan — monitoring-of-monitoring + log budget controls (DR07/DR08 READY slice)

Status: plan reviewed before edits. Targets: `ops/monitoring/alerts.yaml`,
`ops/monitoring/alerts.test.yaml`, `compose.monitoring.yaml` (log limits),
`docs/alert-runbook.md`, `docs/observability.md`.

## Scope decision (YAGNI + measure-first, documented honestly)

The full H08 packet (host/VM/native collectors, real paging, measured
template limits) stays partially gated: real host collectors for macOS +
Docker Desktop need an owner-reviewed component choice (node_exporter
measures the Linux VM, not the Mac host; Docker-socket collection is
forbidden by the worker's security boundary), and real paging needs a
receiver authority. This packet delivers the READY slice with zero new
services:

1. **Monitoring-of-monitoring alerts (DR07 explicit gap)** using metrics
   the existing Prometheus/Alertmanager self-scrapes ALREADY export:
   - `OCRRuleEvaluationFailing`: `prometheus_rule_evaluation_failures_total`
     increases (rules broken — monitoring cannot trust itself).
   - `OCRNotificationDeliveryFailing`: rate of
     `alertmanager_notifications_failed_total` > 0 for 2m (notifications
     failing — with local-only Alertmanager this is a distinct, honest
     signal that the last hop is broken even before a real receiver
     exists).
   - `OCRMonitoringDeadMan`: `absent()`-style always-firing watchalert on
     the Prometheus self-scrape — if it stops firing, the whole
     monitoring stack is down or not evaluating (out-of-band watchdog
     signal; the alert that must always fire).
2. **Log byte/retention budgets (DR08 template gap)**: explicit
   `logging: driver: json-file with max-size/max-file` on the monitoring
   Compose services so container stdout/stderr cannot grow unbounded
   (template-level; daemon defaults documented as un-inspected).
   API/worker/postgres/redis production template limits remain a
   measure-first decision (H09) — NOT guessed here.
3. **Runbook/observability docs** updated with the new signals, their
   meaning and the explicit remaining gates (host/VM/disk/backup
   collectors, real receiver, watchdog delivery, threshold calibration).

Non-goals preserved: no document IDs/text in metrics; fixed-cardinality
labels only; thresholds are candidate values recorded as such.

## Tests

`alerts.test.yaml` promtool fixtures for each new rule: firing boundary
(hold), recovery, and for the dead-man: firing when healthy, ABSENT when
the self-scrape disappears (the dead-man's own missing series proves stack
failure — validated via the OCRTargetDown-style input series). Run via the
existing `scripts/check-monitoring.sh` in the pinned disposable promtool
container (no host state, no network, 256 MiB).

## Budget

promtool test run ≤2 min in the existing 256 MiB disposable container;
file edits ≤150 lines; no new images/services/dependencies.

## Acceptance / rollback

`check-monitoring.sh` passes (config check + all rule fixtures +
amtool); comment guards green; runbook/observability updated. Rollback:
revert the four files; no runtime impact until the next monitoring
profile deployment (which itself is opt-in and adoption-gated).

## Research

Prometheus self-metrics (`prometheus_rule_evaluation_failures_total`) and
Alertmanager notification metrics (`alertmanager_notifications_failed_total`)
are standard, version-stable exports of the pinned images; validated
locally by the promtool fixtures rather than assumed from documentation.
Dead-man/absent semantics validated in the fixture itself.
