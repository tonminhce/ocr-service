# OCR monitoring candidate: operation and alert response

Updated 2026-10-16. This runbook describes the opt-in local monitoring candidate in [compose.monitoring.yaml](../compose.monitoring.yaml), [Prometheus configuration](../ops/monitoring/prometheus.yaml), [alert rules](../ops/monitoring/alerts.yaml), and [rule tests](../ops/monitoring/alerts.test.yaml). It does not authorize live activation or establish production readiness. Deployment, capacity, independent quality labels, reviewer identity, backup/retention and release gates remain in the [execution plan](production-execution-plan.md).

The configured receiver is `local-only` with no notification integration. Prometheus can evaluate firing alerts and forward them to the local Alertmanager API, but this configuration sends no email, chat, webhook or human page. Seeing an alert in either UI does not prove anyone was notified. Adding an external destination requires explicit authorization, destination/credential review and a delivery/recovery drill.

## Validate without activating services

Run from the repository root:

```sh
bash scripts/check-monitoring.sh
```

The script resolves only image names from the unexpanded Compose templates, requires digest-pinned tools, mounts the tracked monitoring configuration read-only, and supplies a temporary synthetic credential to `promtool check config`. It also runs the rule fixtures and `amtool check-config`. Tool containers have no network, a read-only root filesystem and bounded memory/CPU/PIDs/tmpfs. The temporary token is deleted on exit. It does not read a real secret, start the application stack, test real token access, demonstrate alert delivery or prove adequate monitoring resources. Exact pinned tool images must be available to Docker; normal image acquisition is separate from these network-disabled validation containers.

Rule fixtures cover per-instance freshness before replica aggregation, boundary holds and recovery, missing metrics, counters after reset, invalid future timestamps, cached stale saturation, and healthy/threshold equality cases. Configuration/rule validation is separate from isolated integration. Do not mark parser/model pause, PostgreSQL outage, worker outage, authentication or Alertmanager ingestion checks passed until the corresponding isolated harness evidence has been inspected.

## Opt in on an approved existing deployment

Use an existing approved project, the same secret directory and non-secret runtime settings used by that deployment, and its reviewed application/parser image lock. The worker image must include the current runtime/dependency metric contract. Attaching these rules to an older image can correctly raise missing-metric alerts. Follow [release operations](release-operations.md) and [restricted-role operations](a03-operations.md); monitoring activation is a separate reviewed deployment action.

The following is a template, not a command to run against the existing live stack during development. Execute reviewed blocks as a shell script with `set -eu` enabled so failed placeholder/preflight checks stop execution. Replace both placeholders, verify the selected project already contains the intended running API/worker, and retain its existing settings. `--no-deps`, `--no-build` and `--pull never` restrict startup to the two explicitly selected monitoring services using locally available images.

```sh
set -eu
ocr_monitor_project='APPROVED_EXISTING_PROJECT'
ocr_monitor_lock='artifacts/APPROVED_RELEASE/compose.lock.yaml'
test "$ocr_monitor_project" != 'APPROVED_EXISTING_PROJECT'
test "$ocr_monitor_lock" != 'artifacts/APPROVED_RELEASE/compose.lock.yaml'
test -f "$ocr_monitor_lock"
docker compose --env-file /dev/null --project-name "$ocr_monitor_project" \
  -f compose.production.yaml -f "$ocr_monitor_lock" -f compose.monitoring.yaml \
  --profile monitoring ps --status running --services
```

Only after the project, settings, image lock and activation have been approved:

```sh
docker compose --env-file /dev/null --project-name "$ocr_monitor_project" \
  -f compose.production.yaml -f "$ocr_monitor_lock" -f compose.monitoring.yaml \
  --profile monitoring up -d --no-deps --no-build --pull never prometheus alertmanager
```

This runbook does not initialize roles, migrate a database, generate/rotate real credentials, rebuild images, restart workers or change concurrency. A configuration check may use `config -q`; never print expanded real Compose configuration, full container environment/argv, secret files or raw document outputs.

Default local interfaces are [Prometheus](http://127.0.0.1:9290) and [Alertmanager](http://127.0.0.1:9293); the port variables can change these. Both UIs/APIs are unauthenticated. Host loopback and the internal Compose default network form the current trust boundary, not a dedicated monitoring network or egress firewall. Other containers on that network can reach them. Anyone with Alertmanager HTTP access can alter silences/alerts; do not expose these ports to a LAN or Internet, browser port forwarding or an untrusted shared host. Review authentication/TLS/proxy access before any broader deployment. The application exporters separately require the mounted metrics bearer token. See the official [Prometheus security model](https://prometheus.io/docs/operating/security/).

To stop only these services after an approved drill:

```sh
docker compose --env-file /dev/null --project-name "$ocr_monitor_project" \
  -f compose.production.yaml -f "$ocr_monitor_lock" -f compose.monitoring.yaml \
  --profile monitoring stop prometheus alertmanager
```

Retain named volumes for evidence. Do not use `down` on an existing application deployment or delete volumes as monitoring cleanup. Disposable integration projects have their own scoped cleanup procedure.

## Inspect safely

Start with readiness, target status, rule evaluation health and active alerts. These local UI endpoints need no application credential:

```sh
curl --fail --silent --show-error http://127.0.0.1:9290/-/ready
curl --fail --silent --show-error --get \
  --data-urlencode 'query=up{job=~"ocr-api|ocr-worker|prometheus|alertmanager"}' \
  http://127.0.0.1:9290/api/v1/query
curl --fail --silent --show-error \
  'http://127.0.0.1:9290/api/v1/rules?type=alert'
curl --fail --silent --show-error http://127.0.0.1:9293/api/v2/alerts
```

Use the actual approved loopback ports if overridden. In Prometheus, inspect a bounded instant query for the metric named below, preserving `job`, `instance`, and `dependency` labels. Do not infer that an empty result is zero or healthy: inspect `up`, expected metric presence and timestamps. Restrict log review to bounded, redacted operational events; do not copy document text, uploaded files, OCR results, review payloads, real tokens, full inspect/config output or arbitrary database rows into incident reports.

Record the first/last observation, target labels, deployment/image identity, last successful timestamps, rule health, action and observed recovery. Use an authorized incident channel; the checked-in receiver creates none. Do not reset counters, edit raw OCR, pick a conflicting field value, reset attempts, remove leases, flush Redis or delete durable outbox rows to clear an alert.

## Freshness, aggregation and timing

Scrapes/evaluations run every 15 seconds; worker observers normally sample every 30 seconds. The durable PostgreSQL/Redis observation is all-or-nothing under a five-second budget. Pool and parser/model-catalog probes have a separate runtime sample timestamp, so a Redis observation failure does not hide a fresh pool observation. Request handlers and scrapes read cached data; increasing scrape frequency will not make the background observations newer.

`ocr:operational_fresh`, `ocr:runtime_fresh` and `ocr:dependency_fresh` retain only positive timestamps no older than 90 seconds, no more than five seconds in the future, and with a successful scrape for the same `job,instance`. Their values are filtered ages, not boolean `1`; use their presence as the freshness set. A timestamp more than five seconds ahead is invalid and is treated as stale by the collector/dependency stale rules. Check host/Prometheus clock synchronization before diagnosing a sudden group of timestamp alerts.

Shared PostgreSQL/Redis gauges are filtered against each reporting instance's freshness before `max by(job)`. Replicas observe the same durable backlog/resource, so summing would double count. Pool occupancy stays per instance. Attempt counter increases are computed per process before summing because counters reset independently. The configured static `worker:9092` and `api:9091` targets support one container of each service. Before adding replicas, provide distinct target discovery/labels; DNS rotation behind one instance label is not replica-safe monitoring. Unit tests of aggregation are not proof that discovery is configured for scale.

`for` requires the expression to remain active for the stated hold. Scrape/evaluation alignment and the 15-second Alertmanager group wait add observation/ingestion delay. A 90-second stale window plus a one-minute hold is not a one-minute failure detector. The [Prometheus alert-rule documentation](https://prometheus.io/docs/prometheus/latest/configuration/alerting_rules/) explains pending and firing state. All thresholds below are `candidate-v1` operational starting points; tune from measured arrival rates, deadlines and false-alert/recovery evidence before acceptance.

## OCRTargetDown

- **Meaning/hold:** `up == 0` for one minute for API, worker, Prometheus or Alertmanager. A failed scrape may mean process/network/DNS/authentication or scrape-size failure, not specifically a database/model outage.
- **Inspect:** target page/last scrape error, expected service identity and loopback readiness; check credential mount presence/permissions without reading its value. Distinguish connection refused from 401/403 and resource/size errors.
- **Recovery:** restore the approved service/route or correct the reviewed mount/config; require repeated successful scrapes and the alert resolving. Do not disable authentication or sample limits to force green status.
- **Escalate:** sustained failure to the deployment owner. A dead Prometheus cannot evaluate its own alert; an unavailable Alertmanager cannot receive notifications. Out-of-band monitoring remains required.

## OCRObserverMetricsMissing

- **Meaning/hold:** worker scrape succeeds but its durable collector timestamp or runtime sample timestamp metric is absent for one minute. Zero is present but unobserved; it is handled by stale rules.
- **Inspect:** the worker's image/metric-contract version and presence of `ocr_operational_collector_last_success_timestamp_seconds` and `ocr_runtime_last_sample_timestamp_seconds`; look for observer startup/task failure in bounded operational logs.
- **Recovery:** restore the approved compatible observer/exporter, confirm both metrics exist and subsequently advance, and verify missing/stale alerts resolve.
- **Escalate:** to the application owner if a valid scrape lacks the contract. Do not interpret a missing collector as an empty queue.

## OCRDependencyMetricsMissing

- **Meaning/hold:** worker scrape succeeds but either `ocr_dependency_up` or `ocr_dependency_last_check_timestamp_seconds` is missing for expected `parser` or `model_catalog` for one minute. The alert retains that fixed dependency label.
- **Inspect:** both metrics for the named dependency and the compatible worker release. A zero result is different from an absent series.
- **Recovery:** confirm both series return with advancing check timestamps; verify missing and stale states clear independently.
- **Escalate:** to the application owner for exporter/probe-contract faults. Do not remove the expected dependency from the rule to hide an absent probe.

## OCRCollectorStale

- **Meaning/hold:** last complete PostgreSQL + Redis snapshot is over 90 seconds old, or its timestamp is more than five seconds ahead, continuously for one minute. A startup zero timestamp eventually qualifies as stale. Cached durable gauges remain exported but cannot trigger current backlog/resource alerts after freshness expires.
- **Inspect:** collector last-success timestamp, `ocr_operational_collector_error`, worker `up`, host clock and observer progress. A parser/model-catalog probe can remain fresh while the durable collection fails.
- **Recovery:** require a complete successful collection with an advancing timestamp; observe at least another normal sampling cycle and alert resolution.
- **Escalate:** to the application/database/Redis owner according to the failing path. Paired collector-failed and stale alerts can both be valid during a prolonged database/Redis failure.

## OCRCollectorFailed

- **Meaning/hold:** the latest bounded PostgreSQL/Redis collection failed for one minute while worker metrics remain reachable. A recent last-good snapshot can still be fresh briefly; the error does not overwrite it with zeros.
- **Inspect:** last-success age, repeated collection errors, database readiness/pool pressure and authenticated Redis connectivity using existing credential-safe tooling. The combined error signal does not identify which dependency failed.
- **Recovery:** repair the reviewed route/service/permissions or investigate the bounded query; require a new complete snapshot and error value zero. Do not enlarge deadlines/pools without evidence.
- **Escalate:** to the relevant database/Redis owner if persistent; preserve outbox and jobs through the outage.

## OCRDependencyUnavailable

- **Meaning/hold:** a completed, positive, recent parser or model-catalog probe reports failure for one minute. The check must satisfy the per-dependency 90-second/five-second-clock freshness bounds. Once the last check becomes stale, this current-failure alert stops and the stale-probe rule applies.
- **Inspect:** named dependency `up`, last-check time and duration. For parser, inspect its independent health/capacity and Unix socket access; for catalog, check bounded `/v1/models` reachability and the configured model listing.
- **Recovery:** require new successful checks, a clearing alert, and a scoped synthetic processing test where authorized. Parser recovery should allow queued work to resume without bypassing parser admission.
- **Escalate:** parser or inference owner as appropriate. `model_catalog` success proves catalog reachability/listing only; it does not prove inference readiness, loaded weight identity, quality or available generation capacity.

## OCRDependencyProbeStale

- **Meaning/hold:** a named dependency check has not advanced for over 90 seconds, or is more than five seconds ahead, for one minute while the worker scrape succeeds. A zero timestamp means no completed observation yet.
- **Inspect:** timestamp/duration, runtime sample timestamp, observer task progress and clock. Distinguish stalled observation from a fresh failed result and absent metric series.
- **Recovery:** require advancing bounded probes and clearing stale/invalid state; a resumed probe may correctly transition to `OCRDependencyUnavailable` if it finds a real failure.
- **Escalate:** to the observer/application owner if collection is stalled; to infrastructure owner for clock drift. Do not assume the dependency is healthy because the last `up` value was one.

## OCRBacklogAge

- **Meaning/hold:** maximum fresh age of an eligible PostgreSQL queued job exceeds 300 seconds for two minutes. Jobs intentionally delayed until `available_at` are excluded. Shared snapshots use max, not sum.
- **Inspect:** eligible/delayed/processing counts, parser admission, fresh dependency checks, durable outbox age and stage/model queue durations. A zero active count with eligible backlog can indicate admission/dispatch failure; a busy model can instead be the limiting stage.
- **Recovery:** backlog drains below the threshold with normal arrivals and completed job evidence; confirm no work/attempt accounting was lost.
- **Escalate:** to the processing/capacity owner when queue deadlines are at risk. Do not increase worker/region/layout concurrency or add threads until measured model, pool, CPU and memory headroom supports it.

## OCROutboxDelayed

- **Meaning/hold:** the maximum fresh age of a dispatcher-claimable durable outbox notification exceeds 60 seconds for one minute. Active dispatcher leases and ineligible delayed jobs are excluded.
- **Inspect:** due-outbox count/age, Redis readiness/authentication, dispatcher publication/acknowledgement error counters and worker availability. PostgreSQL is the durable source of truth; a Redis entry alone is not completion evidence.
- **Recovery:** reviewed dispatch path resumes, due notifications drain and jobs advance under lease fencing; require fresh snapshots and alert resolution.
- **Escalate:** dispatcher/Redis owner if persistent. Never flush Redis or delete outbox records as a shortcut.

## OCRExpiredLeases

- **Meaning/hold:** a fresh snapshot observes at least one expired processing lease for one minute. Short-lived expiry during a crash/recovery is expected to clear; persistent expiry suggests delayed reclaim, missing worker or coordination trouble.
- **Inspect:** expired count, worker/parser admission, outbox age, lease-renewal errors and fresh durable snapshots. Verify the worker that lost its lease cannot commit stale results.
- **Recovery:** normal fenced recovery reclaims or terminalizes the jobs according to existing attempt/deadline policy; count becomes zero without manual reset/deletion.
- **Escalate:** worker/database owner if recovery does not progress. Do not extend leases indefinitely or reset attempt counts to make the alert disappear.

## OCRJobFailures

- **Meaning/hold:** the sum of per-process `increase(ocr_job_attempts_total{outcome="failed"}[5m])` is at least three for one minute. This is sampled/extrapolated process telemetry, not an exact durable audit count. Restarts and failures between scrapes can lose observations; resets are handled per counter before summing. See the official [`increase` function semantics](https://prometheus.io/docs/prometheus/latest/querying/functions/#increase).
- **Inspect:** failure outcome versus retry-scheduled/lease-lost/aborted outcomes, bounded model/transport error categories and durable terminal-job accounting through authorized metadata access. Do not expose raw OCR/business fields.
- **Recovery:** identify and repair the fault, verify authorized synthetic or queued work succeeds and observe the rolling-window alert clear. Preserve failed inputs/results privately for authorized investigation.
- **Escalate:** application/model owner for sustained errors or representative workload regressions. This rule is not a quality benchmark or an acceptance/error-rate denominator.

## OCRPoolSaturation

- **Meaning/hold:** process-local `(size - idle) / max` SQLx pool occupancy is at least 90 percent for two minutes, gated by that worker's fresh runtime sample and successful scrape. It does not depend on successful Redis collection.
- **Inspect:** all three pool states, runtime timestamp, query/acquisition durations and database responsiveness. Busy connections are not waiter counts or measured checkout latency; low occupancy does not exclude another bottleneck.
- **Recovery:** occupancy falls with sustained fresh runtime samples and acceptable observed acquisition latency. Investigate held transactions/slow queries before changing pool size.
- **Escalate:** database/application owner if demand remains above the measured budget; coordinate total connections across replicas with PostgreSQL capacity. Do not promote a pool/concurrency increase from this gauge alone.

## OCRRedisMemoryHigh

- **Meaning/hold:** the maximum fresh per-instance `used_memory / maxmemory` ratio exceeds 85 percent for two minutes. The positive configured limit is read from the same bounded Redis `INFO memory` response as used memory; freshness filtering happens before replica aggregation. The threshold is a deployment candidate, not measured capacity or a cgroup RSS measurement.
- **Inspect:** fresh `ocr_operational_redis_memory_bytes`, `ocr_operational_redis_maxmemory_bytes`, PEL/outbox/backlog, approved noeviction policy and actual container headroom. The current startup script sets `maxmemory 128mb`, so the ratio gives an earlier warning against that limit instead of the larger 256MiB container budget. A zero `maxmemory` means Redis has no configured dataset limit: this ratio alert is disabled, which does not establish healthy capacity. A missing limit metric is also unavailable evidence, never a zero or safe budget.
- **Recovery:** confirm backlog/PEL progress and stable memory below the approved threshold without deleting durable work or pending acknowledgements. Treat rejected writes and allocation pressure separately.
- **Escalate:** Redis/capacity owner before the measured safety margin is exhausted; do not raise container/maxmemory budgets or flush data as automatic remediation.

## OCRRuleEvaluationFailing

- **Meaning/hold:** Prometheus reports rule-evaluation failures over five minutes. Monitoring output can no longer be trusted while this fires; a monitoring stack that cannot evaluate its own rules is not evidence of system health.
- **Inspect:** Prometheus targets page, `prometheus_rule_evaluation_failures_total`, rule-group errors and recent config changes. Bad rule syntax or a broken recording expression is the usual cause.
- **Recovery:** fix the rule/config error and confirm failures stop increasing and the affected alerts evaluate again. Do not silence this alert to make a broken stack look healthy.
- **Escalate:** monitoring owner; treat all other quiet alerts as unknown during the outage.

## OCRNotificationDeliveryFailing

- **Meaning/hold:** Alertmanager notification attempts failed over five minutes for at least one integration. With the current local-only receiver this means the last delivery hop is broken; once a real receiver is configured it means human paging is not reaching its destination.
- **Inspect:** Alertmanager UI, `alertmanager_notifications_failed_total` by integration, receiver endpoint reachability from inside the Compose network.
- **Recovery:** restore the integration endpoint; verify delivery with a synthetic alert. Real receiver configuration and an out-of-band watchdog remain owner gates — local Alertmanager success is not human paging.
- **Escalate:** on-call/owner once a real receiver exists; until then treat as monitoring-stack fault.

## OCRMonitoringDeadMan

- **Meaning/hold:** this alert fires continuously while Prometheus is up and evaluating. An external watchdog that stops seeing it must raise an out-of-band alarm: the monitoring stack itself is down, restart-looping, or not evaluating rules.
- **Inspect:** if the dead man is quiet while Prometheus looks healthy, check rule evaluation, scrape config and the watchdog's own transport. `up{job="prometheus"}` disappearing stops the dead man by design.
- **Recovery:** restore the Prometheus self-scrape/evaluation; the dead man resumes firing without operator action.
- **Escalate:** monitoring owner via the out-of-band channel (email/SMS/external probe); this is the one signal intentionally routed outside the monitored stack, and configuring that external route is an owner decision.

## Resource and production acceptance limits

The candidate gives Prometheus 0.5 CPU, 256MiB memory, 64 PIDs, 24-hour retention and a 1GB retention-size target; Alertmanager has 0.25 CPU, 128MiB memory, 64 PIDs and 24-hour data retention. These are bounded starting values, not measured sizing for production. Query/sample/body limits protect the process but can also cause a failed scrape/query; investigate before weakening them.

Prometheus retention size is not a filesystem quota: WAL/head data and temporary/compaction needs require extra free disk. See the official [storage guidance](https://prometheus.io/docs/prometheus/latest/storage/). No host swap, host/container free-disk or successful-backup exporter is included. Named-volume retention does not replace tested off-host backup. Review [backup operations](backup-operations.md) and its restore/activation gates.

Prometheus and Alertmanager self-scrapes provide health observations, but dedicated policies for rule-evaluation errors, notification/ingestion errors, delivery blackholes, monitoring resource pressure and configuration reload failures remain open. A self-scrape does not provide an independent watchdog for a dead monitoring process. A future authorized notification integration must prove both firing and resolution delivery and define an owner, escalation policy and silence/change audit.

Before production acceptance, inspect isolated authentication/outage/recovery evidence, establish distinct target discovery for any replicas, measure steady/burst/soak resource impact, validate the Redis ratio threshold and separate container-RSS headroom, approve host/disk/backup monitoring and an external notification/watchdog path, and calibrate thresholds against workload deadlines. Keep real model capacity, OCR quality, independent labels, release provenance and live rollout gates separate. Passing these rule tests alone satisfies none of those gates.
