# O01 candidate alerting and cached dependency telemetry

Date: 2026-10-16. Scope: local candidate tooling and isolated synthetic verification. No live deployment or external notification delivery is authorized or claimed. This extends the [A06b/O01 trace](a06b-execution-trace.md); it does not restart or supersede parser isolation.

## Implementation and boundaries

The worker's existing 30-second observer now samples SQLx pool counters and probes parser health plus the configured model catalog. Parser and model probes run concurrently inside one sequential collection loop; missed ticks do not create accumulating tasks. PostgreSQL/Redis collection and runtime probes are independent. Scrapes render cached, fixed-cardinality values and perform no database, network, parser or model work.

Each dependency probe has a two-second total deadline. The model probe uses `/v1/models`, checks the exact configured model ID, caps the streamed body at 64 KiB, disables redirects and proxies, and marks its authorization header sensitive. It does not call inference, warm the model or prove inference readiness. Parser health reports active/maximum sessions; failed probes remove capacity observations. Pool size/idle/max counters describe this worker's pool, not database-wide connections or pool waiters.

Redis `INFO memory` now supplies actual `maxmemory` alongside used memory without an additional round trip. The candidate warning compares used memory with that configured limit, rather than the larger container memory allowance. A zero Redis limit disables this ratio alert and leaves capacity protection unavailable.

`compose.monitoring.yaml` adds opt-in, digest-pinned Prometheus and Alertmanager services. Both run as UID/GID 65534 with read-only roots/configs, dropped capabilities, no-new-privileges and bounded memory/CPU/PIDs. Only Prometheus receives the metrics token; neither receives application/database/model secrets. UI/API ports bind to loopback. Alertmanager's `local-only` receiver stores/groups alerts without a human notification integration.

There are 16 rules: three freshness records and 13 alerts. Freshness requires a positive timestamp, age at most 90 seconds, no more than five seconds in the future, and a healthy scrape target. Freshness filters apply per instance before shared-state aggregation, so a healthy replica cannot bless another replica's stale gauges. The [alert runbook](alert-runbook.md) documents every alert, holds, recovery and remaining operational limits.

The release inventory optionally includes monitoring image identities and lock services via `--monitoring`, and hashes `ops/` configuration inputs. These remain local/operator-declared inventories, not signed source/image provenance. CI is configured to validate pinned monitoring tools, scan their images and run the packaged monitoring fault drill. Hosted CI execution is still an open gate.

## Review findings resolved

- Parser connect and exchange previously had separate budgets; an outer timeout now enforces the advertised total probe budget.
- Replica aggregation previously allowed a fresh timestamp to accompany a stale high shared gauge; per-instance filtering now happens before `max`.
- Pool occupancy and failed-dependency alerts now use the relevant runtime/dependency freshness; missing observer/dependency metrics have explicit alerts.
- Redis memory warning now uses observed `maxmemory`, including regression fixtures for the actual 128 MiB limit, exactly 85 percent and unlimited-zero behavior.
- The fault harness checks safe selected Docker metadata instead of printing full inspect output, and verifies complete original synthetic job-row hashes after faults and a new job.

GitNexus impact was run before symbol edits. Recorded observer impact was LOW with one direct worker-main caller; constructor/renderer/store/queue analyses and actual source were also reviewed. New unindexed harness/policy symbols returned UNKNOWN, so their CLI/CI callers were reviewed manually. Index freshness is not proof of complete Rust dispatch/caller coverage.

The aggregate precommit `detect_changes --scope all` reported CRITICAL, mapping 81 symbols to 63 flows through shared metrics. After refreshing the index, the final monitoring staged analysis mapped 124 symbols to 55 flows and still reported CRITICAL. Both warnings were reported to the user before proceeding. Manual zero-context diff review confirmed `process_metrics`, existing stage/counter observers and API handlers were unchanged despite appearing in the earlier graph's mapped scope. Actual runtime edits are the additive cache/renderer, local pool counters, queue snapshot tuple, bounded observer and worker startup wiring. The new queue tuple is consumed by that observer and its regression test. The broad graph warning remains part of the evidence; it is not relabeled LOW. Full native/Linux checks and isolated packaged fault recovery cover the changed implementation within their stated limits.

## Verification record

Private evidence lives under ignored `artifacts/o01-alerting-2026-10-16/`. Only synthetic fixtures were submitted. Raw logs, JSON reports, generated test credentials and source/image hashes remain outside Git.

| Check | Observed result |
|---|---|
| Rust format and strict workspace Clippy | Passed |
| Native ordinary workspace tests |284 passed, 90 explicit integration tests ignored by ordinary run |
| Native explicit serial integration tests |90 passed |
| Linux ARM64 parser verification image |121 passed, 2 ignored |
| Observer regression tests |8 passed; subset of ordinary count |
| Metrics regression tests |10 passed; subset of ordinary count |
| Pinned Prometheus/Alertmanager configuration validation | Passed |
| Alert rule fixtures |39 cases, 124 alert assertions, 3 PromQL assertions passed |

The first complete monitoring drill passed 15 checks. It verified the packaged course-layout/extraction pipeline with 12-second mock-model latency, parser-outage intake gating, restart recovery, four healthy scrape targets, unauthorized scrape rejection, actual Redis limit, dependency/collector/worker-down alert ingestion and resolution, and a fresh successful job after recovery. Worker clean shutdown took 0.4115 seconds. Forty cache scrapes with at most four clients measured median 2.871 ms and p95 6.106 ms. This tiny local probe is not OCR throughput or capacity evidence.

The strengthened repeat in `drill-final/verification.json` passed all 18 checks. Selected Docker metadata matched pinned image IDs, nonroot users, read-only roots/configs, capability/privilege restrictions, exact memory/CPU/PID limits, loopback ports and intended mount destinations/types/permissions. These assertions do not inspect environment values or secret contents. Three original succeeded fixture jobs totaled four attempts; hashes covered their complete `jobs` rows and stayed identical through faults and a fresh job. The final state was four succeeded jobs, five attempts, with exactly one new latest job succeeding on attempt one. Other tables are not attested by these row hashes.

The repeat observed clean worker shutdown in 0.4595 seconds and cache-scrape median 1.886 ms / p95 2.547 ms for 40 requests with at most four clients. Runtime timestamps progressed while PostgreSQL was stopped. Four targets recovered, all exercised alerts resolved, and the unique project was removed with volumes retained. Both runs used synthetic model/layout responses and made no real-model or external notification calls. The first evidence directory is retained unchanged.

The no-new-explanatory-comments rule is tracked in `.github/copilot-instructions.md`, a fixed full-commit adoption baseline, a lexical checker and CI. Existing comments are grandfathered per source path; new/changed/duplicated comments are rejected in Rust/SQL/Python/shell beneath maintained source roots. Licenses, shebangs and exact supported tool directives are exempt. Review found and fixed here-string/arithmetic-shift/heredoc partial matching and cross-language/prose directive exemptions. There are 21 focused guard regression tests; the complete Python suite passed 32 tests. The new-comment checker scanned 127 files with zero violations; the retained block checker scanned 104 with zero violations. Embedded executable shell/SQL still needs manual review, and unknown syntax/directives can fail conservatively. No mass deletion or migration rewriting was performed.

Application, parser, Linux parser-verification and matching backup-operations images built successfully with a single Cargo build job locally. The candidate inventory verified seven image identities, 11 locked services, 141 input hashes and all four monitoring configuration hashes; Compose accepted its monitoring lock merge. The first inventory correctly records a dirty precommit worktree and operator-declared source binding. An inventory is not a signed release attestation. Final syntax checks covered monitoring YAML, workflow shell steps, 18 documentation shell blocks and 29 new/trace local links. All six migration hashes matched their original snapshot.

At the end of both drills, all 13 previously running OCR/test container IDs were still running unchanged. Neither drill namespace retained any container; their volumes were retained. The policy commit's `detect_changes --scope staged` had no indexed symbols to map; its five exact staged files and CI hunks were inspected manually before commit. The index was then refreshed successfully for the final monitoring review. Private evidence and handoff exports were not staged.

## Failures and limitations retained

An initial rule-tool run failed because its read-only container lacked writable temporary space; a bounded `/tmp` tmpfs fixed the execution environment. A pool-rule fixture incorrectly expected a label that PromQL binary arithmetic removes; expected semantics were corrected and rerun. These failures remain in private logs.

An initial native integration run overlapping image builds failed two enqueue deadline tests: one could not establish the initial connection within 40 ms, and another 200 ms-budget test did not observe the intended COMMIT path. Both original tests subsequently passed unchanged in individual reruns, followed by all 90 explicit tests passing serially before the next image builds. Host memory/swap pressure supports timing sensitivity but does not establish its cause. Failed tests may have left schemas in the preexisting disposable verification database; exact schema IDs were not captured, so no broad schema deletion or complete-schema-cleanup claim is made.

Local Trivy 0.72.0 reported 8 HIGH / 0 CRITICAL for the pinned Prometheus image and 10 HIGH / 0 CRITICAL for Alertmanager. Reports remain visible in private evidence. CI specifies Trivy 0.76.0; local results do not establish that version's results. Passing the configured fixable-CRITICAL threshold does not mean zero vulnerabilities.

Independent Rust source review found no new regression. It recorded a preexisting defense-in-depth limit: parser health reads generic IPC responses with the shared 128 MiB ceiling before rejecting an unexpected operation. Legitimate `HealthReady` frames must be empty, but a compromised parser could send an allowed image frame during a health exchange. This remains bounded but deserves a narrower health-specific limit in a separate change. Runtime observations also publish only after the concurrent operational collection completes, adding up to its five-second budget; they do not have a separate detached task.

Remaining gates include three successful hosted CI runs, verified registry/source provenance, real-model staging and 24-hour observation, independently labeled quality/calibration, measured 1,000-plus-documents/day capacity, reviewer identity, off-host backup/RPO/RTO/retention decisions, external alert delivery and a monitoring watchdog. Host swap/free-disk and backup-success exporters, distinct multi-worker discovery and storage/soak sizing remain open. A 1 GB Prometheus retention target is not a filesystem quota. Trusted loopback/internal monitoring APIs are not an authenticated public service.

The original live application containers and volumes are preserved. Only each drill's unique disposable project is stopped; its volumes remain. No inference concurrency, live credentials, migration bytes, private OCR data, remote branch or registry was changed by this milestone.
