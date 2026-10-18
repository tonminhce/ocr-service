# Round-9 implementation plan — H14 ladder rung 3: bounded 2× burst + drain

Status: plan reviewed before execution. Continues the mechanism ladder
after rung 2's sustained receipt. MECHANISM-ONLY: mock model, synthetic
fixture, W1/R1/L1 canonical images, fresh disposable project.

## Design

- Burst: fixed-rate 0.05/s (2× the sustained mechanism rate) for 900 s
  → up to 45 scheduled; driver registry 200; submit slots 1.
- Corrected budgets from rung-2's lesson: per-document deadline 2,700 s
  (exceeds worst-case Little's-law queue wait: 45 jobs × ~40 s service
  ≈ 30 min + burst window); drain deadline 3,600 s (driver maximum).
- Expected behavior: the W1 queue absorbs the burst (~40-job backlog),
  drain completes within budget; every job terminal attempt 1; client
  accounting should now show completed = scheduled (no deadline
  artifacts) or exactly-reconcilable outstanding.
- Stop conditions: OOM/swap spiral (>500 MiB swap growth), lease loss,
  container restart, drain beyond 3,600 s. Any stop → record with cause.
- Observe: H04 partitions, terminal p50/p95, server cross-check (all
  jobs, attempts, outbox, leases), worker metrics pre/post, host memory
  at burst end and drain end.

## Acceptance

All scheduled jobs terminal server-side (attempt 1); client completed ≈
scheduled with the corrected budget (the rung-2 lesson verified); burst
absorbed without restart/failure; backlog drained within budget.
MECHANISM receipt only.

## Rollback

`compose down` (no -v); volumes retained + inventoried; delete private
artifacts. No live state.
