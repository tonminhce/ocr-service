# Enterprise OCR: execution plan for the next agent

Date: 2026-10-17. **Planning/handoff only; not authorization for live deployment.** Application baseline `1f5c6c4`; verified documentation checkpoint `acb8958`. The receiving agent must discover the actual current HEAD/dirty state rather than reset to these commits.

Read the [deep review/evidence register](production-deep-review-2026-10-17.md), then follow the [executor harness and copy-ready prompt](production-executor-harness-2026-10-17.md). This is an actionable continuation of [production-execution-plan](production-execution-plan.md), not a replacement for its invariants or a direction to redo completed milestones.

## 1. Outcome and authority

Build an OCR service that is correct under retries/faults, bounded in memory/processes/bytes/time, fast on measured representative workloads, operable by real owners, and releasable from traceable artifacts. “Enterprise-grade” means demonstrated controls and recovery, not more services, abstraction layers or threads.

The implementation agent should complete every **READY** packet, keep working on independent packets when another needs input, and leave genuine external gates visible. It must not declare the entire service production-ready from synthetic tests. A packet has separate code/test, measured-quality/capacity and production-adoption statuses.

Allowed work: reviewed repository edits, synthetic/disposable tests, bounded local builds/experiments, documentation and focused local commits. Not implicitly allowed: push/hosted dispatch/registry publication, real private data transfer, live cutover/restart, secret rotation, destructive retention, volume deletion, real-model concurrency increase, cloud spending or replacing original services. Public pinned artifact retrieval must be declared in the relevant packet with source/hash/license/size/time limits; never allow an implicit mutable pull or send private data to obtain it.

## 2. What not to redo or destabilize

- Retain transactional PostgreSQL intake/outbox, Redis notification/replay and fresh lease fencing. Do not promise exactly-once model execution.
- Keep separate raw OCR/native/recovery observations, candidates, corrections and approvals. Conflicts remain unselected, confidence null, automatic acceptance off.
- Keep bounded W jobs, process-shared R HTTP calls, CPU permit ownership, L persistent layout helpers and isolated parser admission/process/decode/write budgets.
- Keep production candidate W1/R1/L1 until measured gates say otherwise. Standalone defaults W2/R4 are not the deployed baseline.
- Keep release thin LTO/strip and local full-debug/incremental defaults. The measured challengers did not justify promotion. Cargo is currently2.49 GiB; do not clean before each build or claim smaller cache improves OCR throughput.
- Preserve all published migrations0001–0006, schema compatibility and historical evidence. Add a new migration only when a demonstrated change needs one.
- Keep existing code comments; add no explanatory comments in maintained code. Do not mass-delete historical material or change the policy baseline.

## 3. Work queue and dependencies

| Packet | Priority / initial state | Depends on | Completion evidence |
| --- | --- | --- | --- |
| H00 Baseline/contracts/docs | P1 READY | None | Fresh scope/identities, corrected documented defaults, owner-question register |
| H01 Parser future-drop regression | P2 READY synthetic | H00 | Reproduction/disproof; narrow fix only if demonstrated |
| H02 Parser operation byte budgets | P1 READY synthetic | H00; serialize edits with H01 | Header-only preallocation tests and local bounded response contract |
| H03 Outbox expiry-lock regression | P2 READY disposable DB | H00 | Deterministic expiry-during-lock proof; conditional fenced/bounded fix |
| H04 Independent bounded arrivals | P1 READY fake HTTP | H00 | Separate submit/outstanding/poll caps and truthful accounting |
| H05 Maintained verification harness | P1 READY tooling | H00 | Safe isolated mode, strict streams, portable policy guards and postflight |
| H06 Complete scan/CI identity | P1 READY tooling | H00 | Inventory/scan/SBOM set equality, reviewed toolchain/actions/MSRV policy |
| H07 Canonical release candidate | P1 LOCAL READY when resources permit; EXTERNAL GATED | H01–H06 relevant changes frozen | Canonical exact-image tests/scans/lock; hosted provenance separately |
| H08 Resource/log/alert operations | P1 READY fixtures; ADOPTION GATED | H00 | Collectors/rules/limits tested; real receiver/watchdog owner approval |
| H09 API/worker/DB memory envelope | P1 READY synthetic; REAL-BACKEND GATED | H05/H08 measurement readiness | Fresh measured plateau, thread/process/byte envelope, one-variable A/B |
| H10 Independent corpus process | P1 OWNER/DATA GATED; tooling READY | H00 | Blind human provenance, adjudication, immutable grouping/splits |
| H11 Machine v2 export/geometry | P1 READY supported-scope design | H00 and reviewed source geometry contract | Machine-only sidecar passes v2 negative controls without gold access |
| H12 Human review/identity | P1 OWNER GATED; contract tests READY | Issuer/roles; H11 for new geometry/scope | Verified actor, bounded UI, CAS/audit/omission workflow |
| H13 Off-host recovery acceptance | P1 OWNER GATED; local controls READY | RPO/RTO/destination/escrow/deletion policy | Off-host loss-host restore, trusted receipt, reconciliation and readiness |
| H14 Real capacity | P1 WORKLOAD/HEADROOM GATED | H04/H07/H08/H09; H10/H11 for quality acceptance, H12 for human-review SLA | Actual arrival/drain/fault/soak receipts; machine vs quality goodput |
| H15 Targeted engine/router work | P2 CONDITIONAL, not blanket implementation | H09/H10/H11 error/cost taxonomy | One challenger capability/parity/quality/benefit gate or explicit rejection |
| H16 Calibration/selective acceptance | P2 DATA/POLICY GATED | H10/H11/H12; adequate independent samples | Frozen risk–coverage/uncertainty and human-approved policy |
| H17 Controlled rollout | P1 ADOPTION GATED | Required release/quality/capacity/ops gates | Reviewed cutover/rollback/config/roles, monitoring and business acceptance |

Parallelize H04 evaluation, H06 release tooling, H08 monitoring and H10 human-data preparation when files/resources do not conflict. H01/H02 share parser client: one writer, separate reviewable increments. One coordinator owns Git staging/commits and shared build resources. Do not run concurrent native compilers merely because review agents can run concurrently.

## 4. Packet specifications

### H00 — Freeze the actual baseline and reconcile contracts

**Why:** DR16 and every subsequent measurement depend on correct effective settings.

1. Read repository rules, the three handoff files, prior execution plan and relevant traces. Record HEAD/branch/dirty paths, Cargo.lock/migration/source inventories, toolchains/platforms and safe configuration names/values. Do not dump environment/secrets.
2. Inventory original container IDs/states, existing volumes/networks/images, native model/Cargo/IDE consumers and disk/memory pressure. Read-only metadata only; do not stop them.
3. Recheck current runtime settings independently of standalone defaults: W/R/L/S, ONNX intra/inter/arena, output/pixel/token limits, API budgets, SQL/Redis pools, actual replica count.
4. Correct `docs/business-review.md` descriptions from `ocr-api/src/{config,response,server}.rs` and production Compose: eight large handlers/default four large serializers plus two small serializers; body-owned byte reservation/16 KiB transport chunks; environment-disabled versus configured service-principal review. No behavior/config change to match stale prose.
5. Document API-supported immutability versus compromised DB/worker credentials, single trust domain and reviewed external TLS/proxy boundary. Do not invent tenant isolation.
6. Open the owner-input register below. Continue READY tasks while decisions are pending.

**Tests/gate:** documentation claims checked against code; local links, both comment guards, whitespace and GitNexus compare/staged scope. Do not run a cold full Cargo build for documentation alone. **Rollback:** docs only.

### H01 — Prove or disprove parser retained-session cancellation risk

**Evidence:** DR01; `parser_client.rs::ParserSession::request`, `render_rgb`, `page_info`; existing parser/inference/layout cancellation tests.

1. GitNexus context/upstream impact before modifying any existing symbol. Use an owned synthetic Unix server, barriers/oneshots and bounded deadlines, no DB/model.
2. Open two-page session; receive render1; abort its caller while retaining `Arc<ParserSession>`; release delayed RGB1; request render2. Assert fail-closed, never RGB1 as page2.
3. Add partial-header/body abort, cancellation while waiting for the socket lock, and successful complete sequential requests. Keep token cancellation and direct future drop distinct.
4. If baseline does not reproduce, record exact conditions and stop the proposed production fix. Do not manufacture a failing oracle.
5. If reproduced, prefer taking ownership of the socket out of `Option` across await and restoring only after the complete exchange under the existing error contract. Dropping the future must close/invalidate an incomplete stream. Do not add request IDs/version changes unless a smaller repair cannot satisfy tests.

**Gate:** baseline-red/candidate-green receipt; typed permanent/transient errors, valid reuse and normal job cancellation unchanged; targeted parser, inference-contract and layout tests pass. **Rollback:** source/image revert, no migration.

### H02 — Enforce per-operation parser response data allowance

**Evidence:** DR02. Local `ParserSession::request` reviewed LOW; shared codec/exchange reviewed CRITICAL. These are hints to recheck, not permanent approvals.

1. Header-only malicious-peer regressions for requested RGB overrun/wrong response phase must fail on the current too-large allowance without sending a giant body or allocating hundreds of MiB. Typed NativeTextResult above4 MiB and typed no-data bodies are already rejected before allocation; retain those controls rather than falsely claiming a new fix for them.
2. Derive each response allowance before reading: PageInfo/no-data control zero, NativeText4 MiB, RGB at most `3 * min(max_pixels, requested_max_edge²)` with checked arithmetic, representability checks and existing global ceiling. Respect full-page versus course-layout settings.
3. Cover exact boundary/one-byte over, unexpected operation, zero/overflow dimensions, malformed metadata, partial frame, timeout and cancellation. Preserve existing codec magic/version/kind/64 KiB metadata/global/payload checks.
4. Keep health/admission/upload-ack zero-data fix intact; do not touch CRITICAL shared `read_frame`/`exchange` unnecessarily. Warn/report blast radius first if unavoidable.

**Gate:** oversized header rejected before body waiting; accepted exact-limit frames byte/pixel equivalent; unchanged protocol2, valid native blocks and canonical errors. **Rollback:** local source/image only.

### H03 — Reproduce outbox lease expiry while an unchanged row is locked

**Evidence:** DR03; `ocr-core/src/store.rs::{mark_dispatched,claim_outbox,lock_job_write}`; tests at `tests/store.rs:1278` and unchanged-job-lock pattern at `:1577`; worker `dispatch_outbox` caller.

1. New disposable PostgreSQL only; synthetic job and short controlled dispatch lease. A second connection locks its outbox row without modifying it.
2. Start mark while token is live; verify expected lock dependency using PostgreSQL observation. Cross expiry using database time, release the blocker, assert expired publisher returns false and cannot alter token/deadline/next-dispatch timestamp.
3. Prove a current dispatcher can then claim/mark; cover changed/replaced token, terminal job, cancellation, lock timeout and pool recovery. Use barriers, not unverified timing sleeps.
4. Only if baseline demonstrates the issue: lock outbox row in a bounded transaction, then use a fresh subsequent statement for token/clock predicate. Preserve confirmed XADD-before-mark and retained replay records. No global dispatch mutex/new broker.
5. Separate SQL lock/statement timeouts from caller-total pool/transport budget. Add a blackhole/timeout test before promising an absolute completion bound.

**Gate:** reproduction receipt, stale mark has zero effect, outbox/Redis-loss/job-lease regressions pass with restricted credentials. No extra inference for duplicate terminal notification. **Rollback:** expected source-only change; any schema proposal requires a separate migration review.

### H04 — Make arrival-rate measurement independent of OCR lifetime

**Evidence:** DR04; `ocr-eval/src/arrival.rs`, `benchmark.rs`, `tests/benchmark.rs`.

1. Keep bounded source hashing/upload submissions, initially at most the existing four. Separate accepted-job tracking from upload lifetime; release upload buffers before long tracking. Store bounded identities/metadata rather than another copy of document bytes.
2. Add a finite outstanding registry independent of submit slots, finite polling concurrency/rate/response bytes and a finite drain deadline. Avoid an unbounded per-job Tokio task; a bounded scheduler/registry is sufficient.
3. Track intended schedule, actual HTTP handoff time, known acceptance/rejection/local failure/unknown acceptance, terminal observations and unresolved reconciliation keys separately. Preserve per-occurrence idempotency and total source-byte/job/time caps.
4. On any client cap, record a specific skip; do not silently delay the offer or relabel skipped occurrences as offered. Do not auto-retry POST to fake target attainment. Source invalidation/local failure must not become HTTP rejection.
5. Keep old report semantics compatible or version the new schema explicitly. Preserve existing POST-window timing and acceptance-unknown controls.

**Tests:** fake fast-POST/slow-terminal server accepts more than four lifetime jobs when registry budget permits; maximum upload/poll/buffer/registry occupancy observed under cap; full registry; 429/503; malformed202; response lost after commit; polling failure; out-of-window POST; cancellation; drain timeout; exact accounting partitions. Reconcile unknowns before another run.

**Gate:** fake-server handoffs match the frozen intended workload when budgets permit; no lost/duplicate occurrence keys, no unbounded tasks/bytes. This proves the generator, not OCR capacity. **Rollback:** old mode/versioned report retained; service configuration unchanged.

### H05 — Promote only the minimal safe verification controls

**Evidence:** current boundary and full-pipeline traces, including private preflight/stream-format failures. Do not blindly copy the340-line private coordinator into maintained source or depend on ignored artifacts being available elsewhere.

1. Inspect existing `production-smoke.sh`, `monitoring-smoke.py`, `parser-boundary-smoke.py` and their Python tests before adding a helper. Add a small reviewed opt-in isolated mode or bounded existing-harness extension, not a new orchestration platform.
2. Preserve the manual smoke contract/default loopback use. For isolated automation: new UUID namespace/output, preexisting-resource snapshot, immutable cached image IDs/explicit pull policy, no host ports, internal network, no inherited host aliases/real model binds, mock-only endpoints and finite CPU/memory+swap/PIDs/restart/tmpfs budgets.
3. Validate effective Compose JSON without printing it. Handle documented numeric/string forms, explicit list overrides and Docker `CAP_` normalization while enforcing exactly the allowed capability set. Fail closed for unknown service, extra mount/network/capability or changed image.
4. Machine stdout must contain exactly one valid complete JSON result. Capture bounded stderr separately, apply credential scans to both, hash both; never strip a random prefix or grep a brace in live output. Preserve original failed files/exit codes.
5. Bound every command, overall work, log bytes and cleanup time. Owner-check every created container/network before removal; never `down -v`, prune or remove volumes. Retain only explicitly inventoried fixture volumes/evidence.
6. Preserve all three scenarios and direct durable-state/roles/session/raw/cgroup postflight from the accepted replay. Bind source→exports→installed paths→image IDs→test receipts. A phase marker/running container is not acceptance.

**Tests:** fake-command regressions for invalid namespace, retained resources, stream contamination, wrong image/platform, CAP_CHOWN spelling versus extra capabilities, mixed Compose types, timeout, output flood, signal/cleanup failure and secret diagnostics. Then one bounded actual synthetic replay. **Rollback:** tooling mode off; no live state change.

### H06 — Make release inventory, scans and build inputs agree

**Evidence:** DR05/DR06/DR15; workflow, `release-manifest.py`, dependency audit, Dockerfile/install/export tools.

1. Define one reviewed runtime image/platform inventory; derive SBOM/scan/lock coverage from it. Include PostgreSQL/Redis and enabled monitoring, plus app/parser/backup. Record native model/runtime/layout components outside container inventory separately.
2. Reject missing/substituted digest, architecture mismatch, missing/stale receipt and set mismatch. Review HIGH/unfixed CRITICAL with advisory/component/reachability/owner/expiry, not silent ignore. Preserve guarded inactive-RSA exception and visible reports.
3. Pin reviewed action revisions and exact CI toolchain; define an update process. Verify Rust1.89 against the frozen lock/targets or consciously revise the declared minimum. Do not assert it works merely because current stable passes.
4. Preserve pinned Docker/ONNX/model identities. Capture apt package versions/SBOM and distinguish traceability from bit-for-bit rebuilds. Add hash-verified platform/transitive model/export environments without upgrading versions just for tidiness.
5. Test workflow shell/YAML syntax without dispatch; add an inventory/scan-set regression alongside existing release-manifest tests.

**Gate:** every deployed component has identity/platform/SBOM/scan disposition; MSRV/toolchain contract explicitly verified or corrected; no secrets/PII in release artifacts. **Rollback:** workflow/tooling/docs only; no runtime/data rollback.

### H07 — Build and accept a canonical candidate, then external provenance

1. Freeze the combined reviewed source/lock/migrations/config/harness/model/runtime identities after relevant code packets. Build actual Dockerfile application/parser/backup targets for the intended production platform; use a declared one-job/finite memory/disk/time budget and verified public artifact retrieval. Do not substitute selected-binary overlays for canonical builds.
2. Run parser/backup verification targets, fresh scan/inventory lock and exact-image pipeline/boundary/monitoring/backup rehearsal. All reports must name the same immutable IDs/platform; reject mixed candidates.
3. Verify model/layout/preprocess/prompt/profile/runtime fingerprints. Distinguish operator declaration, observed runtime value and independently verified artifact; `/v1/models` does not attest weights or scheduler settings.
4. Local candidate can complete without claiming hosted success. Push, hosted dispatch/registry publication and signing/provenance need explicit authority. Verify trusted issuer/repository/workflow/ref/source/platform/digest; reject wrong-source or replayed evidence.
5. Production-only validation should reject missing approved fingerprints without breaking development contracts; keep startup/rollback compatibility explicit.

**Gate:** canonical candidate receipts, current scans/lock and exact-image rehearsals locally; external gate separately true only when genuine hosted/provenance evidence exists. **Rollback:** previous immutable image/config pair, not mutable tag mutation or schema down-migration.

### H08 — Operational resource/log/monitoring controls

1. Choose the smallest read-only collectors for macOS host, Docker VM, service cgroups and native Metal process; measure them separately. Include memory/pressure/swap trend, CPU throttling/PIDs/OOM, disk free/inodes and DB/Redis/log/model/build/backup growth. Do not sum overlapping RSS/unified-memory categories.
2. Add collector success/freshness/error semantics and missing-target alerts, backup freshness from verified off-host/restore receipts, rule-evaluation/notification failures and an out-of-band watchdog.
3. Measure combined resource use before choosing template API/PG CPU/memory/PID limits, connection pools and logging budgets. Record daemon/proxy defaults safely. Do not copy fake-model caps into production or mount a broad Docker socket/rootfs merely for convenience.
4. Add explicit log byte/file/retention limits with redaction; separate operational logs from required audit history. Never log document text, tokens, full URLs or high-cardinality IDs in metrics.
5. Real paging requires an owner-approved destination/on-call schedule/escalation/recovery policy. Verify actual delivery/acknowledgement; local-only Alertmanager is not human paging. Replica discovery only when replicas are adopted.

**Tests/gate:** synthetic collector-stale/error/OOM/disk-age/backup-age/rule/receiver failures produce correct distinct signals; baseline exporters remain bounded. Real receiver/watchdog adoption remains gated until exercised. **Rollback:** disable the new collector/rule/integration only, not all monitoring.

### H09 — Measure complete memory, API and database envelopes before tuning

**Worker/backend:** freeze effective W/R/L/S, ONNX2/1/arena, model/config SHA and actual OS threads. Observe source bytes, page RGB lifetime, queued JPEG/base64/JSON, active/queued model calls, helper memory, formatting/raw copies, DB buffers and tmpfs/cgroup charges. O(W×R) prefetch is bounded but can still consume the envelope.

**API:** exact-limit synthetic uploads, large results, concurrent slow socket readers, compact status during saturation, review/history, cancelled CPU waiters and DB pool contention. Record service versus driver memory, active slots/reservations, timeouts/rejections/recovery. Preserve body-owned reservation and the fixed transport chunks.

**Database:** stepped synthetic row/byte populations; sanitized plans/buffers/query duration/lock wait for admission, outbox, leases, operational snapshots, review keysets and retention. Keep enqueue replay when full, lease renewals and CAS concurrent. Never run large generators or write-bearing EXPLAIN on live DB.

**Real ONNX A/B only when headroom permits:** existing arena-on/off control, fresh isolated cgroup per variant, same weights/input/order/threads/limits, equal warm-up, after-warm/after-idle plateaus, complete output hashes. Capture anonymous/file/kernel memory, RSS/PSS, CPU/PIDs/swap; do not infer arena causality from one retained-memory number.

**Conditional changes:** select one measured cost at a time: same-size resize, copy ownership, crop byte admission, database query/index. Predeclare benefit/regression tolerance; preserve exact pixels/JPEG/masks/EXIF/raw/schema and checkpoint hits. No pre-crop model permit move or second pool without proving overlap/cost trade-off. HIGH impact on `compute_crop_bitmap` must be reported before edit.

**Gate:** bounded plateaus and representative headroom, baseline/candidate source/config/output parity and at least three paired repeats for viable candidates. An early clearly losing pilot may be rejected with its limitations rather than prolonged. Memory savings alone need not imply goodput gains. **Rollback:** original flags/code/images, no data rewrite.

### H10 — Establish independent corpus and annotation process

1. Data owner approves purpose/access/storage/retention/sampling. Keep the existing assistant draft development-only; never flip provenance booleans or invent humans/adjudication.
2. Pilot10–20 cases to validate instructions, ambiguity/blank/unreadable handling, effort and two independent critical-field annotators plus a distinct adjudicator. These pilot counts are planning budgets, not statistical certification.
3. Freeze source/render hashes, logical contract/card/person associations, revision/near-duplicate groups, template families and the supported dev/calibration/test splits before opening test predictions. Record external training-data provenance separately if relevant; do not add a new training enum/schema for this plan. Exact hashes alone do not catch near duplicates.
4. Keep signed/access-controlled human provenance privately. Measure disagreement/adjudication/review minutes and error taxonomy; source truth may legitimately conflict.
5. Expand a justified representative corpus only after pilot/process review. A200–300-case pilot is not proof of a tiny unsafe-acceptance risk. Keep development examples used to tune rules out of frozen assessment.

**Gate:** auditable blind independent labels/grouping/splits, frozen denominators and source permissions. Synthetic schemas or assistant labels cannot close it. **Rollback:** version dataset/labels; never overwrite provenance/history.

### H11 — Machine-only v2 sidecar and truthful geometry

1. Implement the smallest opt-in exporter for currently supported scope, without reading gold files/IDs or human corrections. Use `ocr-eval/src/v2.rs`, annotation contract and actual worker output as authority.
2. Bind original source/page render identity, actual CropBox/rotation/render/crop transforms and stable machine document/party/card IDs. Preserve UTF-8 offset/hash rules and real geometry granularity; unsupported scope abstains explicitly.
3. Never invent word polygons by splitting a block. Keep recovery-only observations unselected and conflicts complete. Multi-party business approval needs its own policy; do not force arrays into legacy landlord/tenant aliases.
4. Test swapped people/cards/pages, transform changes, clipped/whole-page geometry, missing conflict candidates, forged spans/hashes, Unicode offsets and no-gold-access execution. Reuse current scorer negative controls.

**Gate:** source-bound machine sidecars pass loader/scorer with truthful precision and no gold leakage; v1 remains compatible. Does not itself prove OCR accuracy. **Rollback:** opt-in exporter/profile off, old results untouched.

### H12 — Verified human review and omission workflow

1. Obtain issuer/audience/deployment URL/roles/session/approval-separation decisions. Decide whether unrelated tenants exist; do not add speculative tenancy.
2. Server produces verified ActorContext, not trusted request fields/headers. Store issuer/subject/authorization context additively; old events remain service-only. Browser never receives the privileged service review token.
3. If browser sessions: CSRF/origin/secure-cookie policy, bounded trusted issuer/JWKS lookup/cache, approved algorithms/audience/time validation and key-rotation failure behavior. Ingress TLS/proxy boundary is explicit.
4. Bounded source/page rendering and UI escaping/CSP; source/OCR text is untrusted data. Show genuine region granularity and source alternatives, stale-CAS reload, correction→separate approval→reopen. Decide whether policy requires distinct humans, not merely distinct requests.
5. Add versioned human-origin source annotations for OCR omissions: original SHA/page/polygon/manual transcript/verified actor/time. Do not fabricate machine spans/confidence. Stable field/person targets and reassociation invalidate affected approvals without rewriting machine assignments.

**Tests/gate:** forged actor/issuer/audience/algorithm, expiry/role/scope/CSRF, key failure, XSS payloads, concurrent revision, ambiguous retries, same request/different actor, omitted text and reassociation. Raw/source and existing history entries remain unchanged; new decisions append auditable events. Human attribution and review-throughput acceptance require real users. **Rollback:** opt-in ingress off; additive history retained, no destructive schema revert.

### H13 — Off-host backup, key recovery and deletion reconciliation

1. Owner approves RPO/RTO, destination, credentials, encryption/key escrow, retention/holds and authoritative post-snapshot deletion source. No external destination or secret may be invented.
2. Bounded encrypted transfer with immutable bundle/trusted receipt, partial-upload failure and idempotent retry; custody/receipt separate from archive. Local dump existence is not off-host success.
3. Simulate source-host/cache unavailability by denying the disposable restore rehearsal access to them; never stop, wipe or delete the actual source host, original volumes or caches. Recover keys independently and restore only from the approved off-host copy into a fresh quarantined target. Verify schema/functions/content/ACLs and empty-Redis recovery.
4. Reconcile deletions/holds before runtime grant/readiness; prevent resurrection. Measure full ready-to-serve RTO and actual recoverable data-loss window, including human actions.
5. Measure backup overlap with snapshots/WAL/disk/latency. Introduce WAL/PITR only if required RPO cannot be met by the reviewed logical approach. Keep production/PITR certification false until genuinely achieved.

**Tests/gate:** corruption/wrong key/missing trusted receipt/partial transfer/incompatible schema/unauthorized target fail closed; owner-signed loss-host recovery evidence. Destructive retention remains off until policy/recovery obligations pass. **Rollback:** restored target isolated, no deletion of current DB/volumes.

### H14 — Prove actual capacity with a corrected driver

1. Obtain actual document/page/region/pixel/token mix, arrival window/burst, intake/completion/business-review deadlines and acceptable risk/cost. Until then,1000/day floor and1200/day growth scenarios are assumptions, not requirements.
2. After H04/H08/H09, freeze corpus/model/source/config and warm-up; run W1/R1/L1 reference. Observe intended/offered/accepted/terminal/outstanding/unknown separately, backlog/oldest age, lease/retry, model usage, host/native/cgroup pressure and quality-qualified goodput.
3. Change one factor at a time: R2 only after verified server max-sequences/config/headroom; R4 only if R2 viable; W2/R1 only after page/helper memory proof; W2/R2 only if needed. Each replica multiplies process-level R/L and consumes parser capacity. L>1 is not free CPU parallelism.
4. Follow bounded ladder: synthetic smoke →20–30 jobs below saturation →60–120 minute arrival run →bounded2× burst≤15 minutes +drain →4-hour soak →approved8–24-hour final workload. Long phases need explicit execution window/work/byte/backlog/resource limits; do not start them automatically from a planning prompt.
5. Reconcile/drain accepted AND unknown outcomes before another config. Stop on OOM, sustained swap/pressure, lease loss, growing backlog outside budget, non-plateau memory or quality regression. No arbitrary memory/timeout increase to rescue a candidate.
6. For viable configs, at least three comparable repeats after equal warm-up. Report sample counts/ranges; a few documents do not establish a production p95. Count human review backlog/effort separately.

Raw/mechanism capacity experiments can proceed without independent gold when their other gates pass. Final quality-qualified machine capacity requires H10's frozen independent corpus and H11's reviewed supported sidecar/scorer; business-review SLA additionally requires H12 and actual human workload evidence. Do not close those gates using machine completions alone.

**Gate:** agreed deadlines/arrival/backlog/headroom/quality and fault recovery on representative workload. If absent, report mechanism/experiment results with acceptance OPEN. GPU migration is a measured cost/capability decision, not a linear-scaling promise.

### H15 — Conditional OCR engines, routing and mapping

1. Use H10/H11 error taxonomy and H09/H14 cost attribution to select one dominant problem. Do not implement every C/D roadmap branch.
2. If a second recognizer is justified, add the smallest baseline-parity seam; pin official artifacts/license/runtime/export/config/dictionary/hash. Start a finite Linux ARM64 CPU capability test, not a full platform/framework.
3. Cheap text OCR candidate: limited Vietnamese line/block cases and a few page cases; exact normalization/padding/CTC/token semantics and real geometry parity against reference. Failure/unsupported export ends the spike with evidence.
4. Native early routing starts shadow; current native extraction follows OCR and does not save calls. Hidden/incorrect/partial native text, scans/cards/tables require fallback. OCR agreement cannot justify a claimed pre-OCR bypass.
5. Alternative VLM/runtime/GPU capability is verified, not assumed from model size or marketing benchmarks. No mosaic batches or new concurrency policy without quality/memory evidence.
6. Expand logical document/party scope only when observed cases need it. KIE is conditional on demonstrated mapping error that simpler rules cannot solve; model instructions remain untrusted data, with no DB/tools/network authority.

**Gate:** same-source/crop comparison, frozen independent quality/noninferiority/critical-source-role checks and repeatable meaningful measured benefit within resources. The old20% goodput example is a proposed threshold to agree before testing, not an existing success. **Rollback:** challenger/router off; baseline available, historical outputs untouched.

### H16 — Calibration and selective acceptance only after data gates

Fit/report offline only after independent labels and versioned policy. Correctness includes value+party+source and document-critical fields, not model self-rating. Freeze calibration/test separation and predeclare risk targets, one-sided bounds, subgroup coverage and drift/audit plan. Recovery-only, conflicts, missing criticals, truncation and failed provenance always abstain.

Zero observed errors in a small/enriched sample is not zero risk; the roadmap's illustrative2995 independent accepted cases for a0.1% one-sided95% bound is not a universal sample-size guarantee. Keep confidence null and autoaccept off until owner-approved statistical/policy gates genuinely pass. Roll out shadow first, retain random audits and immediate disable/rollback without rewriting history.

### H17 — Reviewable controlled production adoption

Require a signed-off gate matrix: source/artifact/model/config, restricted roles/migrations, restore/deletion policy, real quality, workload deadlines/headroom, human review, alarms/on-call/watchdog and rollback compatibility. Rehearse old→new→old→new with immutable pairs and unchanged durable rows in a disposable environment. Publish/activate only with explicit authority and a concrete diff/window/owner. Never change live env then restart and call that approval.

Unmet gate means implementation/candidate verified but adoption blocked. It must not be converted into production-ready by editing a status label.

## 5. Measurement and acceptance contract

Before each performance run write a one-page experiment record: hypothesis, factor changed, paired baseline/candidate, exact source/config/model/input identity, warm-up/order/repeat count, CPU/thread/memory/bytes/PID/disk/time limits, stop/cleanup triggers, primary metric and regression tolerances. Thresholds are proposed until accepted before observing the result.

Report cold Cargo versus cold OS cache separately; allocation requests versus requested bytes versus live heap/RSS/cgroup separately; HTTP calls versus model tokens and real usage separately; machine completions versus quality-qualified completions and human approvals separately. No summed overlapping memory, synthetic latency extrapolation or fabricated provenance.

### Planning equations, not benchmark results

- Arrival target is `documents / active_window_seconds`, not daily total alone. At1000/day,24-hour and8-hour windows mean41.67/hour and125/hour; the1200/day growth scenarios mean50/hour and150/hour.
- The historical single development PDF took178.43 seconds, approximately20.2 sequential documents/hour. If every document were identical,1000 would need about49.6 serial hours, approximately2.07× the24-hour rate or6.20× the8-hour rate before safety margin. This is an illustrative feasibility warning, **not representative corpus capacity or a prediction that concurrency yields those speedups**.
- Little's-law planning `outstanding ≈ offered_rate × accepted_to_terminal_time` explains the driver issue:150/hour×178.43 seconds is about7.43 outstanding jobs on average, already above four lifetime slots before burst/backlog margin. Registry sizing still needs a finite cap and measured tail/queue/drain behavior.
- Memory accounting must include compressed source, nominal active RGB pages, prefetched JPEGs, base64 expansion near4/3, JSON/raw/formatted copies, native sessions and cancellation/cleanup tails. A request-byte cap or semaphore count alone is not a process-memory proof.
- Approximate logical growth is `jobs/day × measured(source + result + checkpoint + review bytes/job) × effective_retention_days`, with holds, indexes/TOAST, WAL, vacuum, replicas and backup copies assessed separately. Measure physical growth rather than adding overlapping size categories. Never treat maximum allowed job bytes as an observed average.
- Budget human review separately: `documents × measured review minutes` and actual reviewer availability/approval policy. Faster machine completion does not establish the business approval deadline.

Freeze a worksheet with these inputs and assumptions before selecting hardware, pool sizes or retention. Stage-demand and server batching behavior must be measured at the selected concurrency; multiplying single-request throughput by R is not proof of scaling.

## 6. Owner-input register

| Needed decision | Blocks | Work that can continue meanwhile |
| --- | --- | --- |
| Approved representative corpus/access and real independent annotators/adjudicator | H10 quality acceptance, H15/H16 promotion | Synthetic contracts, adapter, negative tests |
| Peak window/burst/document mix and intake/OCR/business deadlines | H14 capacity acceptance | H04 driver, bounded profiling/tooling |
| Identity issuer/roles/session/separation-of-duties/tenant requirement | H12 real integration | Fail-closed actor contract/CAS/forgery tests |
| Backup destination/escrow/RPO/RTO/holds/deletion source | H13 operational acceptance | Local corruption/quarantine controls |
| On-call/receiver/watchdog/notification authority | H08 adoption | Collector/rule/receiver-failure fixtures |
| Hosted push/registry/signing/publication permissions | H07 external provenance | Local canonical builds/locks/replay |
| Execution window/headroom/model/backend/budget | H09 real session, H14 long runs | Synthetic memory/cancellation tests |
| Live deployment/retention/rotation/cutover authority | H17 | Reviewable dry-run/config/rollback evidence |

Ask concise questions once, record answers, continue independent READY work. Missing inputs are not invitations to fabricate data or reduce protections.

## 7. Completion checklist

Every packet produces a focused local commit when authorized, a trace with exact expected/observed controls and failed attempts, source/artifact identity, test counts including ignored cases, private receipt paths/hashes, resource/cleanup postflight, rollback and explicit open gates. Use the [harness](production-executor-harness-2026-10-17.md); do not stop after git status, nor keep starting stacks without a reviewed new hypothesis/budget after a failed prerequisite.
