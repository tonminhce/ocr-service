# Round-6b implementation plan — DB growth/query scaling study (DR18)

Status: plan reviewed before execution. Disposable PostgreSQL only; no
production-code change; no index/storage changes (evidence first).

## Question (DR18, MEASURE FIRST)

At synthetic populations of 10k / 50k / 100k jobs (with outbox rows,
checkpoints and review history at proportional volume), how do the hot
operational queries behave — duration, plan shape, buffer usage, lock
waits? The stated workload is ~1,000 jobs/day; 100k rows ≈ 100 days. Are
any of the indexed hot paths (claim, outbox-due, admission, operational
snapshot, review keyset, retention preview) degrading in a way that
justifies a new reviewed migration?

## Design (bounded probe)

New `#[ignore]` test `store_hot_query_scaling_probe` in
`ocr-core/tests/store.rs` (uses the existing disposable-schema fixture):

1. Per population step (10k, 50k, 100k): bulk-insert synthetic jobs
   (batched INSERT via unnest, bounded 8 KiB result JSON per job, mixed
   queued/processing/succeeded states, staggered timestamps) + outbox
   rows for queued jobs + review events for a sample subset.
2. At each step, time (Instant, 5 repeats, take median) and EXPLAIN
   (ANALYZE, BUFFERS — read-plan capture, no writes beyond the
   population itself):
   - claim_outbox(1, 30) batch query
   - claim (jobs due) query
   - operational snapshot aggregate (the observer's durable query)
   - review history keyset page (LIMIT 100)
   - retention plan preview (LIMIT 100)
3. Emit strict single-line JSON to stderr: per-step populations, medians,
   plan node types + buffers read, row/byte totals, scope disclaimer.
4. Structural asserts only (queries succeed, medians finite); NO
   threshold promoted. Budget: ≤10 min test wall, ≤400 MiB fixture
   growth, single disposable schema (dropped on cleanup).

## Acceptance

Receipt in the trace: medians per population step with plan shapes. If
the hot paths stay index-scan-shaped with flat-ish growth, record
"no migration justified at this scale" as evidence; any degradation is
reported honestly with its plan — a new index would be a separate
reviewed migration proposal, NOT added here.

## Rollback

Delete the probe; fixture schema dropped by the existing cleanup.
