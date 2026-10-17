# Production deep review — 2026-10-17

## Purpose and evidence standard

This is a source/evidence review for a subsequent execution agent, not another implementation pass. Read it with the [execution packets](production-agent-execution-plan-2026-10-17.md), [executor harness](production-executor-harness-2026-10-17.md) and existing [production plan](production-execution-plan.md). Repository root at review time is `/Users/tonminh-mac/Documents/GitHub/ocr-service`, branch `codex/rust-ocr-service`.

Application baseline: `1f5c6c4`. Documentation/evidence checkpoint: `acb8958`. Later documentation-only commits do not change the compiled baseline. Reconfirm source identity before execution; line numbers below are navigation anchors, not permanent APIs.

Finding classes:

- **CONFIRMED GAP:** directly visible missing coverage/capability or bounded measurement limitation. Not automatically an exploitable vulnerability.
- **REPRODUCE FIRST:** a source-supported correctness/security hypothesis without a new runtime reproduction. Do not claim production corruption or patch blindly.
- **MEASURE FIRST:** finite architecture trade-off; optimization depends on representative cost/headroom evidence.
- **OWNER GATE:** requires real business/data/identity/operations decisions; synthetic tests cannot close it.
- **PRESERVE:** reviewed working invariant; do not replace it to manufacture architectural activity.

Priority is execution sequencing, not a CVSS score. No new application defect below has been promoted from hypothesis to reproduced production incident. This was not an exhaustive line-by-line audit, penetration test, fresh vulnerability scan or production workload certification.

## Verified baseline, not work to redo blindly

| Evidence | Accepted scope | Important limit |
| --- | --- | --- |
| Native Rust/Cargo 1.98.1 | 315 ordinary workspace tests; strict all-targets Clippy; three profile variants | 90 ignored cases were not passed by ordinary commands |
| Linux ARM64 Rust/Cargo 1.99.0 | 316 ordinary +90 explicit tests; strict release Clippy; 120 compiled source hashes; 12 executable exports | One extra Linux filesystem-failure test explains the count difference |
| Python/style | 42 Python tests, formatting, whitespace and both comment guards | Not a substitute for behavioral tests |
| Current-binary overlay | Eleven installed executable hashes and two image identities verified | Historical runtime bases; backup export not in the selected eleven paths |
| Parser boundary | Seven controls passed on those image identities | Not a penetration test or real OCR accuracy result |
| Synthetic full pipeline | Slow-model profile, parser outage and mid-job restart passed; three durable jobs/four attempts; roles/raw/state/cgroup postflight | Mock layout/model, not real throughput or representative RSS |
| Cargo disk | 42.24 →2.49 GiB allocated; release bytes and retained debug closure preserved | Next canonical build is cold; sustained future cache growth is not yet characterized |
| Post-processing reuse | Complete-output parity; seven-round substep allocation comparison | About 70% less allocation traffic in that substep, not whole-worker RSS/goodput |

Receipts and limitations: [Cargo execution](cargo-target-optimization-execution-trace.md), [reuse](e02-postprocessing-reuse-execution-trace.md), [overlay/boundary](o02-current-binary-boundary-execution-trace.md), [pipeline including failed attempts](o02-current-binary-pipeline-execution-trace.md). Private ignored receipts may not travel with a new checkout. Their absence means NOT_AVAILABLE; never recreate imaginary successful logs from these summaries.

## Coverage and existing architecture

| Area inspected | Source anchors | Invariants to retain |
| --- | --- | --- |
| API/auth/response/transport | `crates/ocr-api/src/auth.rs:12`, `response.rs:35`, `server.rs`; jobs/views routes | Separate service/review credentials, bounded serialization/body reservations, connection/write budgets, response compatibility |
| Durable store/queue | `crates/ocr-core/src/store.rs:444`, `store.rs:500`, `store.rs:617`; worker queue/outbox/lifecycle tests | PostgreSQL truth, Redis hints/replay, lease-token fencing, fresh checks after job-row locks, idempotent durable intake |
| Review/retention/backup | Core review/retention/administration/backup modules, six published migrations | Raw observations immutable, corrections separate, CAS, holds/deletion reconciliation, restricted runtime roles |
| Worker/inference | `worker.rs:103`, `inference.rs:86`, `lib.rs:68` in `crates/ocr-worker/src/` | W-bounded jobs, process-shared R model semaphore, permits retained by actual blocking work |
| Layout/parser | `layout_runner.rs:118`, `parser_service.rs:128`, `parser_client.rs:239`; `ocr-layout/src/model.rs:36` | Persistent bounded helpers, kill/reap ownership, separate parser health/admission, process/decode/write bounds |
| Evaluation/quality | `ocr-eval/src/arrival.rs:261`, `benchmark.rs:375`, `v2.rs:261` | Explicit uncertain acceptance, source/provenance validation, no invented human gold or confidence |
| Release/operations | Dockerfile, CI, release inventory, Compose, alerts, install/export scripts | Immutable image identities, visible scan exceptions, no source-binding/provenance overclaim |

The source already implements bounded asynchronous orchestration and multithreaded CPU/native work. Production Compose is **W1/R1/layout pool1**, while standalone settings default W2/R4. ONNX defaults are intra-op2/inter-op1, arena on. Freeze effective deployment values, not convenient source defaults. No evidence justifies a rewrite into Rayon, Kafka, Kubernetes, an extra cache service or a generic plugin architecture.

## Review register

### DR01 — Retained parser session after direct future cancellation

**P2 / REPRODUCE FIRST.** `crates/ocr-worker/src/parser_client.rs:239` borrows a socket from `Mutex<Option<UnixStream>>` across exchange and clears it only in a processed error branch at `:270`. Dropping/aborting that future while retaining the session can bypass the branch. `parser_protocol.rs:72` responses have no request/page correlation ID. A delayed valid response could be consumed by the next retained-session request.

The main worker normally drops the document/session on job cancellation. This is therefore a conditional API robustness candidate, **not observed production page corruption**. First create a synthetic Unix-socket barrier test: abort page1 after its request arrives, retain the session, release response1, request page2, and forbid accepting response1 as page2. If reproduced, prefer owned-socket-across-await/drop invalidation and restore only after a valid complete exchange. No new protocol framework is needed.

Reviewed GitNexus `request` scope: LOW, three direct callers/two mapped processes. Recompute before editing; manual preparation/native/layout caller coverage is wider than the graph.

### DR02 — Established-session response allowance exceeds requested envelope

**P1 / CONFIRMED CODE-LEVEL BUDGET GAP; regression required before repair.** `parser_client.rs:256` passes the global128 MiB allowance for established-session requests. The codec already rejects a typed NativeTextResult above4 MiB and nonempty typed no-data responses before allocation (`parser_protocol.rs:337`). The remaining gap is requested RGB edge/pixel rejection after payload reading at client `:182`, and an otherwise valid large RgbResult arriving in the wrong PageInfo/NativeText response phase. A compromised peer can consume more than that requested operation needs while remaining below the real global/type-specific bounds. This is finite amplification, **not unbounded allocation** or an absent NativeTextResult limit.

Test a header/metadata-only oversized response without sending its body; rejection must occur before body waiting/allocation. Cover PageInfo zero data, NativeText4 MiB, both render profiles and exact valid boundaries. Prefer a local per-operation allowance using checked arithmetic and the existing global ceiling. Health/admission/upload acknowledgements are **already fixed** with zero response-data allowance at `parser_client.rs:382`; do not reopen them.

Shared `parser_protocol::read_frame` has a reviewed CRITICAL scope: eight direct callers,36 upstream symbols/20 mapped processes. `exchange` is also CRITICAL. Avoid broad codec edits when a local request allowance suffices; warn and rerun impact if they become necessary.

### DR03 — Outbox expiry during unchanged-row lock waiting

**P2 / REPRODUCE FIRST.** `crates/ocr-core/src/store.rs:477` checks dispatch-token/clock expiry in the same UPDATE that may wait for a row lock. The job-write path at `:617` explicitly uses lock-then-fresh-statement to avoid qualifying before an unchanged-row wait. Existing outbox tests cover expired/replaced publishers, but the unchanged-row wait crossing expiry needs a dedicated disposable PostgreSQL barrier regression.

Possible impact is stale dispatch bookkeeping/postponed notification replay, not proven durable job loss or stale job completion. Preserve XADD confirmation before marking and all job fencing. Add lock/statement plus client-total budget tests only after establishing the intended contract; server timeouts alone do not prove transport-blackhole completion bounds.

### DR04 — Arrival load generator holds its four slots through OCR completion

**P1 / CONFIRMED MEASUREMENT LIMITATION.** `crates/ocr-eval/src/arrival.rs:12` caps pending tasks at four; `:261` skips when full; `:310` retains `benchmark::execute_document`, which polls to terminal at `benchmark.rs:375`. It cannot independently offer a high arrival rate while long-running accepted jobs remain pending.

Illustration only: four lifetime slots with178.43-second jobs imply roughly80.7 completions/hour of generator turnover, below the planning150 arrivals/hour scenario. That is **not service capacity**. Split bounded submission from a separately bounded outstanding registry/poll/drain lifecycle before drawing E04 conclusions. Do not merely raise a concurrency constant or create one unbounded task per accepted job.

### DR05 — Runtime scan set differs from inventoried deployment set

**P1 / CONFIRMED COVERAGE GAP.** `.github/workflows/ci.yml:143` scans application/parser/backup and monitoring images; PostgreSQL/Redis are pulled only at `:153`. `scripts/release-manifest.py:65` inventories them too. No matching scan/SBOM is produced for those two runtime dependencies in that loop.

Use one reviewed image/platform set for inventory, scans, SBOMs and locks. This is not a finding that either image is vulnerable. Preserve the narrowly guarded inactive-RSA exception in `scripts/audit-dependencies.sh:7`; do not hide advisories to pass a release.

### DR06 — Build/runtime provenance and declared MSRV coverage

**P1/P2 / CONFIRMED INPUT/COVERAGE GAPS plus OWNER GATE.** CI uses mutable action tags/stable toolchain at `.github/workflows/ci.yml:42`; its PostgreSQL service uses a tag. All six crates inherit workspace Rust1.89, but reviewed evidence covers1.98.1/1.99.0, not that declared minimum. This is an untested compatibility promise, not proof of an MSRV build failure.

Docker bases and ONNX archives already have digests/checksums. Runtime apt updates intentionally mean a pinned base alone does not promise bit-for-bit rebuilt images. Freeze actual toolchain/platform/build/package inventory and review action SHAs. Test the declared MSRV or explicitly revise its contract based on evidence. Canonical source-bound app/parser/backup/monitoring builds, current scans and exact-source hosted provenance remain distinct from the successful local overlay.

### DR07 — Host/disk/backup monitoring and human paging are incomplete

**P1 / CONFIRMED CAPABILITY GAPS plus OWNER GATE.** `ops/monitoring/alertmanager.yaml:1` routes to local-only with no real receiver. `prometheus.yaml:27` and `:35` have one static API/worker target. Existing observer probes are bounded/cached (`ocr-worker/src/observer.rs:17`, `:44`, `:75`), but host/VM/native-model pressure, disk/inodes, verified backup freshness and out-of-band monitoring failure need their own collectors/receipts.

Missing/stale/error must not become healthy zero. Keep fixed-cardinality labels and no document IDs/text. Monitoring cannot reliably detect its own total failure using only its own rules. Choose a real owner/receiver/watchdog and verify firing/recovery/acknowledgement. Replica discovery is conditional on actually deploying replicas, not an excuse to add a platform now.

### DR08 — Production resource/log policy is not fully explicit

**P2 / CONFIRMED TEMPLATE GAP, actual live defaults NOT INSPECTED.** Production Compose has selected worker/parser/Redis limits but no complete API/PostgreSQL CPU/memory/PID policy or logging rotation stanza. Do not conclude live logs are unlimited: daemon/proxy/deployment policy was not inspected.

Measure DB pools, backend memory, source/result growth, WAL, backups and log volume; then define operational caps/redaction/rotation. Do not copy the synthetic512 MiB worker cap or86 MiB mock observation into production sizing. Cargo's disk problem is substantially fixed, but Docker/build/model/log/backup/evidence growth are separate stores with separate owners.

### DR09 — Independent gold and machine-sidecar bridge remain missing

**P1 / OWNER GATE and IMPLEMENTATION GAP.** `ocr-eval/src/v2.rs:261`, `:306`, `:320`, `:413` enforce grouping/splits/hashes/provenance declarations. `docs/annotation-guide.md:51` correctly warns those declarations do not prove real independence; `:70` documents the missing live v2 adapter.

The assistant-labelled private PDF is development-only, never independent human gold. Establish blind human transcription/adjudication and frozen grouping/splits. Build an opt-in machine-only exporter for supported scope without reading gold IDs/corrections. Real transform/geometry/UTF-8 spans must be truthful; block-level geometry must not be relabelled word-level. No scorer defect was established by this review.

### DR10 — Human reviewer identity is not service-principal identity

**P1 / OWNER GATE.** `ocr-api/src/auth.rs:34` intentionally separates privileged review bearer auth from the ordinary OCR token. This protects an internal service boundary, not an organization-authenticated human workflow. Preserve its existing service-principal semantics. A browser must not receive the privileged token or supply a trusted actor name/header.

A concrete issuer/session/role decision precedes human UI integration. Preserve correction/approval separation, CAS, immutable machine evidence and explicit omission annotations. Add tenant isolation only if unrelated tenants are actually required; do not silently claim the current shared service token is a multitenant boundary.

The configured principal is server-owned (`ocr-api/src/routes/views.rs:173`), while existing review events store a principal string (`ocr-core/src/review.rs:73`), not verified issuer/subject. External ingress also needs an explicitly reviewed TLS/proxy boundary: the bounded HTTP/1 server and loopback production binding are not themselves native TLS. Supported API immutability is not tamper-proof storage against a compromised worker/DB administrator; runtime write privileges exist intentionally.

### DR11 — Off-host disaster recovery is not closed by local restore success

**P1 / OWNER GATE.** `ocr-core/src/bin/ocr-backup.rs:218`, `:268`, `:316`, `:422`, `:468` use a consistent bounded encrypted backup, seal-last validation, preconnection verification and quarantine readiness. Production/PITR certification remains false and deletion reconciliation remains required.

Choose approved RPO/RTO, destination, key escrow, restore authority and deletion/hold source. Prove loss-host/key-recovery restoration from the off-host copy, not a convenient local volume. RTO ends at authorized readiness, not merely `pg_restore`. WAL/PITR is conditional on RPO; do not build it speculatively.

### DR12 — Bounded crop prefetch is not the same as a memory envelope

**P1 measurement priority / MEASURE FIRST.** `ocr-worker/src/inference.rs:360`, `:539`, `:682` create crops before the process-shared model permit. Each job's `buffer_unordered(R)` permits O(W×R) queued JPEGs plus nominal W active RGB pages even though HTTP in-flight remains≤R. Started blocking closures/helper cleanup can retain older page ownership during cancellation, so nominal W pages is not a complete peak-memory proof. This finite prefetch was already recognized; it is not a new leak.

Measure prepared bytes/page lifetime/HTTP activity first. Moving the model permit before crop may serialize CPU/network overlap and delay checkpoint hits. Preserve the existing design unless measured byte pressure warrants a separately bounded admission change.

### DR13 — Native model/arena, copy and formatting costs need attribution

**P2 / MEASURE FIRST.** Existing `ocr-layout/src/model.rs:19`/`main.rs:33` already provide CPU arena on/off. Historical retained helper memory is about1.58 GiB; arena causality is not proven. Mock pipeline86 MiB does not load that real graph.

`preparation.rs:391`/`:414` resize even same-size crops; its reviewed impact is HIGH. `inference.rs:462`/`:477` retain raw/formatted copies; a16 MiB serialized cap is not a16 MiB peak-heap guarantee. Profile occurrence/cost before either change. Preserve exact masks, rounding, EXIF, JPEG, raw fields and schema. Arena control experiments require no speculative source edit.

### DR14 — Replica/checkpoint scaling has global resource costs

**P2 / MEASURE FIRST.** `ocr-worker/src/main.rs:25` checks local W against parser session capacity. Each replica has its own R semaphore and layout sessions; cluster-wide HTTP can reach replicas×R, and parser sessions/pools sum across processes. More cores do not make another approximately1.6 GiB ONNX session free.

`ocr-core/src/checkpoint.rs:8` bounds entries128 KiB/job16 MiB/4096 entries and keeps caching opt-in. That is not a global retention budget:1200 jobs/day×16 MiB is an illustrative18.75 GiB/day before indexes/WAL. Measure saved model calls, heartbeat contention and retention cost before enabling; do not invent a cross-job cache.

### DR15 — Dependency/model installation identity is partially pinned

**P2 / CONFIRMED REPRODUCIBILITY GAP.** `scripts/install-vllm-metal-macos.sh:13` pins wheel URLs/version without complete hashes/transitive lock. `tools/layout-export-requirements.txt:2` pins top-level versions only. Retain existing model revision pins and ONNX checksum discipline; add platform-specific verified artifacts/locks without silently upgrading models/frameworks.

### DR16 — Business-review documentation drifts from current controls

**P2 / CONFIRMED DOCUMENTATION DEFECT.** `docs/business-review.md:30` describes shared eight-slot serialization and unbounded buffered slow-client bytes. Current `ocr-api/src/response.rs:35` separates eight large handlers, default four large serializers and two small serializers; `:156` transfers reservations into the body, and `:248` uses copied16 KiB transport chunks. Kernel buffers/DB projections still have separate budgets.

The same document at `:75` says sample review is disabled; production Compose at `:146` configures service-principal review credentials, whereas environment-only defaults remain disabled. Correct the documentation/mode distinction in a small future task; do not change deployed behavior merely to match stale prose.

### DR17 — Combined API memory/transport envelope remains empirical work

**P1 measurement priority / MEASURE FIRST.** API upload/body/CPU/connection limits already exist (`ocr-api/src/routes/jobs.rs:12`, `:103`, `response.rs:74`, `server.rs:29`). What is missing is a joint measured envelope under uploads, large result projections, serialization, slow readers, status polling and DB pool contention. Do not add another generic limiter before measuring the current ones.

Use real socket tests, not only Axum in-process requests; retain cancellation/byte ownership and compact status availability during pressure. Targeted regression anchors include API `tests/http.rs:513`, `:657`, `:713` and `tests/transport.rs:106`, `:142`, `:160`. Driver memory and service/cgroup memory are different observations.

### DR18 — Database growth/query scaling needs plans, not a new broker

**P2 / MEASURE FIRST.** Intake uses an advisory admission lock/count, outbox uses due-time ordering/skip-locked, operational views aggregate metadata, and review history uses bounded snapshot/keyset pagination. These are not demonstrated bottlenecks at the stated workload. Study synthetic row/byte populations, lock waits, query plans/buffers, source/result/checkpoint/WAL growth and backup overlap before changing indexes/storage.

Preserve replay-before-capacity rejection (`ocr-core/src/store.rs:303`), total enqueue budget (`:275`), fresh-clock lease writes (`:617`), review CAS/idempotent replay (`review.rs:404`) and one-snapshot history (`:342`). Add indexes only through a new reviewed migration with write/storage cost and rollback compatibility. Do not rewrite published0001–0006, introduce a counter cache, partitioning, object storage or another queue merely from a1000/day headline.

## Decisions and non-goals

First execute H00–H06 ready correctness/measurement/release-tooling packets. Start independent annotation and owner decisions in parallel. Close observable resource/headroom gaps before real W/R/L/replica sweeps. Choose one performance candidate from measured attribution; preserve a rejected experiment as a valid outcome.

Do not perform all model/router/KIE/storage/platform ideas at once. No automatic acceptance, false calibrated confidence, guessed geometry, new threads to hide queueing, arbitrary memory-cap increases, shared-cache pruning or replacement of durable PostgreSQL/lease/outbox semantics.

## Primary references checked for the harness

- Tokio documents that started blocking tasks are not abortable; CPU parallelism needs explicit admission. Preserve ownership until actual work finishes, rather than releasing permits when the waiter disappears. [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
- Compose secrets are service-granted file mounts; host/filesystem permission behavior must be tested for the actual deployment, not inferred from a desired YAML UID/mode. [Docker Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/)
- Queue locking/clock behavior requires transaction-level regression, not an informal exactly-once claim. [PostgreSQL SELECT/locking](https://www.postgresql.org/docs/18/sql-select.html)
- Redis pending/group delivery needs explicit acknowledgement/reclaim semantics. Check the deployed8.2 command version: latest documentation includes newer options not available there. [XREADGROUP](https://redis.io/docs/latest/commands/xreadgroup/), [XAUTOCLAIM](https://redis.io/docs/latest/commands/xautoclaim/)
- Immutable action revisions reduce mutable CI inputs; pinning still requires reviewed updates and verification. [GitHub Actions secure use](https://docs.github.com/en/actions/reference/security/secure-use)

These sources constrain design; they do not attest this deployment. Execution and final review must refresh version-sensitive facts.
