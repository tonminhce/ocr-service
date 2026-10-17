# Round-6b execution trace — DB hot-query scaling study (DR18)

Date: 2026-10-18. Measurement packet; no production-code change. New
`#[ignore]` probe `store_hot_query_scaling_probe` in `ocr-core/tests/store.rs`
(disposable UUID schema; population via bulk inserts; dropped on cleanup).

## Measured (disposable postgres:18, 1 GiB, ARM64; 5 repeats, medians)

| Scale (jobs) | jobs table | claim (median) | outbox claim (median) | operational snapshot (median) |
| --- | --- | --- | --- | --- |
| 10k | 3.5 MB | 3.5–4.1 ms | 6.2 ms | 14.6 ms |
| 50k | 16.5 MB | 18.4–29.7 ms | 5.6 ms | 71.1 ms |
| 100k | 33.0 MB | 25.6–27.3 ms | 6.2 ms | 147.4 ms |

Plan evidence at 100k rows (EXPLAIN ANALYZE, BUFFERS, direct psql on an
identically-shaped population): the claim query is an **Index Scan using
`jobs_queued_due_idx`** (LockRows → Limit), 3510 shared buffer hits —
index-shaped exactly as designed, no sequential scan.

## Reading (evidence, no change mandate)

- **Outbox dispatch claim is flat ~6 ms from 10k to 100k** — the
  `next_dispatch_at` index fully covers the dispatcher's hot path. No
  action needed.
- **Operational snapshot grows linearly** (count(*) aggregates) but is
  147 ms at ~100 days-equivalent volume — far inside its 3 s statement
  budget, cached every 30 s by the O01 observer, and off the request hot
  path. Acceptable at this scale; not a bottleneck.
- **Job claim is index-shaped** with modest growth; the probe's
  synthetic population makes nearly all rows simultaneously eligible
  (worst-ish case), so the median still bounded ~30 ms at 100k. No
  migration justified by this evidence.
- No index/storage proposal follows: DR18's rule (new indexes only via a
  separately reviewed migration with cost/rollback) remains, and this
  study found no degradation that would justify one at the stated
  1,000/day workload scale (100k rows ≈ 100 days).

## Failed attempts (preserved)

1. Two in-probe EXPLAIN-capture attempts returned empty — EXPLAIN is not
   a row-producing expression in PostgreSQL; plan evidence was captured
   via direct psql on an identically populated schema instead.
2. Direct psql population first violated `jobs_check2` (succeeded rows
   need a result) — corrected with a small JSONB result, mirroring the
   probe's own data shape.
3. A stale partial outbox insert from the failed attempt produced a
   duplicate-key error on the outbox insert — harmless for plan capture
   (outbox claim timing was already measured by the probe itself).

## Cleanup / postflight

Probe schema dropped by its own cleanup; the direct psql `plancheck`
schema dropped CASCADE; fixture container `db-scale-30EBC5BB-postgres`
removed by exact name; zero fixture volumes; all 13 original live
containers intact.

## Verification

Probe passes (1/0); ordinary store suite 1/0/35-ignored (all ignored
suites already verified in round 5); fmt/Clippy/comment guards green.
