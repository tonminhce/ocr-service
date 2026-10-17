# Executor harness and copy-ready handoff

Date: 2026-10-17. Companion files: [deep review](production-deep-review-2026-10-17.md), [H00–H17 execution plan](production-agent-execution-plan-2026-10-17.md), [repository production plan](production-execution-plan.md).

## 1. Prompt to give the implementation agent

Copy the following prompt, or supply this file with those three documents:

> Continue `/Users/tonminh-mac/Documents/GitHub/ocr-service` on its existing `codex/rust-ocr-service` branch. Read AGENTS.md, .github/copilot-instructions.md, docs/production-execution-plan.md and the three 2026-10-17 deep-review/execution-plan/executor-harness documents before editing. Inspect actual HEAD/dirty state; never reset to a historical checkpoint.
>
> Execute every READY H00–H17 packet in dependency order. Start with baseline/documentation reconciliation, targeted parser/outbox reproduction, bounded arrival-driver correction and complete release-scan coverage. Continue independent READY work when a packet needs real data/owner input. Keep a task ledger with implementation, verification and production-adoption status separately. Do not stop after status/preflight, but do not manufacture a full production-ready label either.
>
> Use the existing Rust architecture: bounded Tokio orchestration, process-shared model admission, bounded blocking CPU work and persistent layout helpers. “Multithreaded/high performance” requires measured useful overlap and headroom, not more threads/pools/replicas. Apply YAGNI: no platform rewrite, speculative plugins/Kafka/cache/tenancy, arbitrary limits increase or simultaneous model experiments. Reproduce suspected defects before surgical fixes; measure actual cost before optimization. A rejected experiment is valid evidence.
>
> Before every existing function/class/method edit, use GitNexus query/context and upstream impact; report HIGH/CRITICAL before proceeding. UNKNOWN is not low risk: inspect actual callers and document graph limits. Keep protocol/schema/public contracts, raw OCR/evidence, lease/outbox/CAS invariants and all published migration bytes. Add no explanatory comments in maintained code. Preserve unchanged old comments and the fixed comment-policy baseline.
>
> Test only synthetic documents and fresh disposable services unless a separately approved corpus task authorizes private data. No secrets, OCR values, labels, private reports or logs in Git. Never push, dispatch hosted jobs, publish, rotate credentials, modify/restart live services, delete volumes, enable destructive retention, increase real-model concurrency or spend cloud money without explicit authority. Existing images/containers/volumes and unrelated dirty/untracked harness files are not cleanup targets.
>
> Use finite CPU/memory/PID/byte/job/task/output/time budgets, host pressure guards and cleanup reserve. No implicit pulls, unbounded helpers, indefinite polling or unlimited retries. Capture strict machine stdout separately from bounded stderr. Owner-check cleanup of only newly created resources; never use down -v/prune/git clean. Preserve failed attempts and original service identities.
>
> Verify targeted regressions first, then the relevant workspace/explicit integration/strict-Clippy gates. Do not count ignored tests as passed. Before each commit run both comment guards, formatting/whitespace checks, GitNexus compare against codex/rust-ocr-service and staged change detection. Update a package execution trace and commit focused intended source/docs locally; do not push. Report exact evidence, limits, cleanup and remaining data/identity/capacity/provenance/operations gates.
>
> Application baseline 1f5c6c4 already has native315, Linux316+90 explicit,42Python and strict Clippy evidence; current eleven-binary overlay passes seven boundary controls and all three synthetic pipeline scenarios. Cargo was compacted42.24→2.49GiB, with defaults retained after A/B. These are prior scoped receipts, not permission to reuse old binaries after source changes, independent human gold, real-model capacity or a canonical signed release. See the committed traces, and regenerate missing private receipts honestly when needed.

## 2. Mandatory preflight

Run from the intended checkout, not a guessed terminal directory:

```bash
pwd
git branch --show-current
git status --short
git log -n 3 --oneline
git rev-parse HEAD
git diff --stat
```

Then:

1. Read the rule files and applicable nested AGENTS instructions. If `.codegraph/` is absent, skip it; do not install/index it automatically. Use GitNexus first for unfamiliar code.
2. Record expected versus actual checkout/branch. Preserve unrelated changes and the preexisting untracked `.claude/`, `AGENTS.md`, `CLAUDE.md`. Do not add them just because they exist.
3. Check native Cargo/rustc/rust-analyzer/model processes and Docker resource identities without printing environment, expanded Compose or secret-bearing configuration. No killing processes by generic names.
4. Record source/lock/migration hashes, toolchain/target and allowlisted effective settings. Include actual W/R/L/S, replicas, ONNX threads/arena, runtime/model/layout/profile/prompt/preprocess identities and byte/time limits.
5. Record free disk, memory pressure/available reserve, swap trend and original container IDs. A large free-disk figure is not memory headroom. High historical swap alone is not proof of an emergency.
6. Declare packet scope and resource/time/output/cleanup budget before starting expensive work. Do not stop a live model/stack to create headroom. If no headroom, continue another READY read-only/tooling packet.
7. Never assume prior private artifacts are available in another checkout. Mark absent evidence NOT_AVAILABLE; committed summaries are not a replacement for new acceptance receipts.

## 3. Exploration and blast-radius harness

CLI examples for this repository:

```bash
gitnexus query -r ocr-service 'parser session cancellation response allowance'
gitnexus context -r ocr-service request -f crates/ocr-worker/src/parser_client.rs --limit 4
gitnexus impact -r ocr-service request -f crates/ocr-worker/src/parser_client.rs --direction upstream --summary-only
```

Use the actual symbol/file, not a generic duplicate name. Read direct callers first, then transitive flows and real source. A graph entry may include tests/unchanged containers and miss dynamic dispatch; neither zero processes nor UNKNOWN proves safety.

Before edit, record target, direct callers, upstream symbols/processes, risk and manual coverage. Warn HIGH/CRITICAL before touching the symbol. Rerun when scope expands. New/unindexed files need explicit manual scope and staged review, not a fake low-risk result. Prefer the smallest local seam over CRITICAL shared codecs.

Index freshness is source-sensitive. A documentation-only HEAD advance need not justify repeatedly rebuilding the graph. If stale/broken, use repository GitNexus guides; bound workers/memory/time and preserve tool-failure evidence. Prior private tool-only memory workarounds are version-specific, not application optimizations or commands to copy blindly. Do not bypass required impact just because indexing is inconvenient.

## 4. Per-packet contract

Before editing, write the following in the trace/ledger:

| Field | Required content |
| --- | --- |
| Identity | Packet/review finding IDs; branch/HEAD/dirty scope; source/lock/model/config/image/platform hashes |
| Hypothesis | Concrete defect/cost; confirmed versus suspected; what would disprove it |
| Invariants | Compatibility/raw/source/role/conflict/confidence/lease/outbox/CAS/retention rules |
| Scope | Exact symbols/files, upstream impact, what is deliberately untouched |
| Baseline | Existing tests/evidence; failing control required for a fix; not borrowed counts |
| Budget | CPU/threads/memory+swap/PIDs, tasks/jobs/pages/bytes/disk/log/time, cleanup reserve |
| Experiment | One changed factor, warm-up/input/order/repeats, primary metric and predeclared tolerances |
| Stop conditions | OOM/sustained pressure, lease/data failure, guard violation, deadline, unexpected resource/identity |
| Rollback | Code/flag/image reversal, compatibility/data constraints; no destructive rollback |
| Acceptance | Exact positive/negative tests and limits; separate code, measured and adoption gates |

A source-supported hypothesis is not a reproduced incident. If a control disproves it, document that and do not patch to make the plan appear productive.

## 5. Disposable resource and privacy harness

- New UUID owner/project/output for each fresh experiment; refuse collisions with containers, networks, volumes and retained evidence. Do not run a prepared old coordinator in place: even failure cleanup could overwrite its old report.
- Use explicit cached/pinned image identities/platforms. Declared canonical builds may retrieve reviewed public artifacts; no implicit mutable pull or private-data egress.
- Validate effective ports, networks, extra hosts, mounts, users, read-only root, capabilities, restart policy, CPU/PID/memory/swap/tmpfs and endpoints before jobs. Normalize known representation differences, not arbitrary unexpected values.
- Isolated tests have no host ingress/real model binds unless specifically approved. Only the socket initializer may need CHOWN in the reviewed topology; do not turn on privileged mode to fix permissions.
- Use fresh synthetic credentials. Respect actual bind-mounted secret UID/mode behavior: repository fixtures use0444 under a0700 host parent; other evidence files0600. Never print values, argv-expanded secrets, complete environment or rendered secret configuration.
- Capture stdout/stderr separately, cap both and combined size, scan both for secret diagnostics before persistence/reporting. Parse complete JSON, not a suffix selected by regex in the live protocol.
- Bound each command and whole task. A native watchdog is sampled soft control, not cgroup hard enforcement. Started blocking tasks/native processes still own permits until work stops/kill-reap completes.
- Capture actual source/artifact identities, runtime policy before rejecting it, cgroup current/peak/events and service/fixture state before cleanup. Do not leave the only diagnostic inside a container about to be removed.
- Cleanup only exact recorded owner-verified new containers/networks/process groups. Never remove volumes, prune images/build cache, reset Git, or delete PDF/labels/OCR/backups/evidence. Retained fixture resources need name/owner/size/purpose inventory.
- Confirm original resource IDs/states and required fixture postflight. Cleanup failure is a failed/incomplete gate, not a successful job hidden behind best-effort cleanup.

## 6. Test and verification harness

Start specific, widen only for the changed boundary, then run full milestone gates. These templates are not permission to run ignored tests against whatever database URL happens to exist:

```bash
cargo test --frozen -j 1 -p ocr-worker --lib parser_client
cargo test --frozen -j 1 -p ocr-worker --test inference_contract
cargo test --frozen -j 1 -p ocr-worker --test layout_pipeline
cargo test --frozen -j 1 -p ocr-eval --test benchmark
```

For H03, use a verified fresh disposable PostgreSQL URL and the existing database fixture guards before selecting the core store test. Do not run admin-capable integration suites on original services. Some explicit tests mutate Redis maxmemory or test roles; isolate and serialise them.

Milestone templates, after resource/fixture gates:

```bash
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/check_no_new_comments.py
python3 scripts/check_no_block_comments.py
cargo fmt --all -- --check
cargo clippy --frozen -j 1 --workspace --all-targets -- -D warnings
cargo test --frozen -j 1 --workspace
cargo test --frozen -j 1 --workspace -- --ignored --test-threads=1
git diff --check
```

Use explicit target directories/configuration consistently. Frozen dependency failure is NOT_RUN, not a reason to silently update Cargo.lock or report a pass. Predeclare any public dependency retrieval. Do not mix debug/release/platform/feature test counts; after source changes report newly observed counts and ignored lists, not315/316 copied forever.

Check the supported Rust/target matrix deliberately. Native passing is not Linux packaging; Linux overlay passing is not fresh canonical runtime; in-process HTTP tests are not transport-pressure tests; allocation probes are not RSS/capacity; fake-model outputs are not OCR quality.

No explanatory comments in `crates/`, `scripts/`, `tools/`, `ops/`, including new tests/examples/doc comments. Keep old unchanged comments, migrations/licenses/shebangs/tool directives. The lexical guards have embedded-language limits: manually inspect SQL bodies/heredocs/quoted executable content and never use those limits to evade policy.

## 7. Commit harness

Only the coordinator stages/commits. Use an explicit path list, never `git add .` over private/unrelated files. Inspect staged filenames/stat/diff, whitespace, secrets/private markers, generated/binary paths, local documentation links and unchanged published migration hashes.

Required before committing:

```bash
git diff --cached --check
git diff --cached --stat
gitnexus detect_changes -r ocr-service --scope compare --base-ref codex/rust-ocr-service
gitnexus detect_changes -r ocr-service --scope staged
```

Both comment guards and relevant tests must already pass after the last source edit. Document graph limitations/new unindexed files and warnings. Compare against the requested branch covers pending work; add a packet baseline comparison when needed rather than claiming it audits all historical changes.

Commit focused source/tests/docs locally when the packet's actual gates pass; do not amend unrelated history/create branches or worktrees without authorization, and do not push. A documentation-only planning checkpoint requires link/scope/style checks, not a needless cold rebuild.

## 8. Trace and completion-report template

Create `docs/<packet>-execution-trace.md` with:

1. Date/packet/status and precise scope; implementation/measurement/adoption states separately.
2. Baseline source/lock/image/config/model/platform and source inventory identity.
3. Finding/hypothesis, impact risk/direct callers/flows, actual invariant and non-goals.
4. Reproduction/control, command argv without secrets, exit code, expected versus observed.
5. All attempts, including failures/timeouts/preflight refusals; original evidence never overwritten.
6. Tests/counts/ignored cases and exact source binding. Private artifact paths/hashes, not raw OCR/labels/token logs.
7. Resource limits versus measured values, sampling gaps and what memory/timing metric means.
8. Fixture pre/postflight, actual cleanup/retained resources and original identities preserved.
9. Rollback, remaining owner/data/provenance/headroom gates and what was not run.
10. Local commit/scope/guard results; no remote/live adoption implied.

Use PASS / FAIL / PARTIAL / NOT_RUN / NOT_AVAILABLE / WAITING_FOR_DATA / WAITING_FOR_AUTHORITY explicitly. Code written is not tested; tested is not representative; representative is not independently quality-accepted; accepted locally is not deployed. Never change a report flag to close a gate.

## 9. Continuing without unbounded retries

After a failed prerequisite, preserve observations, identify the exact cause, review the minimal change and declare a finite next attempt. A new namespace is required if any retained resource exists. Do not relax capabilities, memory, timeouts, acceptance labels or independence requirements merely to obtain green output. Set an attempt/time ceiling per experiment and continue another READY packet when it is exhausted. Extending it requires an explicit reviewed new hypothesis/scope, not silent looping.

End the execution phase only when all authorized READY packets have completion evidence or a specific genuine blocker. Report the owner decisions needed for the remainder. Do not schedule background/recurring work, create new chats or send tasks to unrelated conversations unless the user explicitly requests that separate action.
