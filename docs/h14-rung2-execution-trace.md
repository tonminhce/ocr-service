# H14 rung-2 execution trace — 60-minute sustained arrival (mechanism)

Date: 2026-10-18. Packet H14 ladder rung 2 (60–120 min target arrival).
MECHANISM-ONLY receipt: mock model (12 s/region), synthetic two-page
fixture, W1/R1/L1 canonical `h07` images in a fresh disposable project.
NOT OCR quality, NOT capacity acceptance.

## Run parameters

Fixed-rate 0.025/s (1 job/40 s) × 3,600 s → 90 scheduled; driver
`--max-pending-tasks 1`, registry 200, per-document deadline 900 s,
drain deadline 1,800 s. Stack health proven by the packaged smoke first
(same identity as rung 1).

## Results

**Client report** (private artifact):
- scheduled/offered/accepted: 90/90/90; zero rejects/skips/unknowns;
  registry occupancy 90 with submit in-flight 1; offered rate exact.
- completed/succeeded (client view): 34; "failed": 56 (client-side
  per-document deadline expiry while jobs were server-queued);
  outstanding 56; drain stopped at its 1,800 s budget.
- submit p50/p95: 14.1/21.2 ms. Terminal (client-observed) p50/p95:
  2,320/3,520 s. Schedule 59.3 min; total elapsed 59.7 min.

**Server cross-check (durable PostgreSQL + worker metrics)**:
- jobs: `succeeded: 93` (90 arrival + 3 smoke), 0 queued, 0 failed,
  0 processing — EVERY offered job reached terminal success.
- `ocr_job_attempts_total{succeeded}=93`, `failed=0`, `lease_lost=0`,
  `commit_failed=0` (1 retry_scheduled = smoke scenario).

## Honest reading

1. **The mechanism held under 60 minutes of sustained load**: all 90
   accepted jobs completed server-side with attempt 1, zero lease loss,
   zero restarts, bounded outbox (drained), memory stable (20–24% free
   across checkpoints).
2. **Little's law confirmed**: at 0.025/s offered with ~40 s service,
   the W1 queue holds a growing backlog through the window; median
   accepted→terminal ≈ 38.7 min matches the predicted queue wait for
   near-full utilization. The system drained the entire backlog AFTER
   the client's tracking budget expired.
3. **Driver-budget lesson (recorded, not a service defect)**: the
   per-document 900 s deadline and 1,800 s drain are CLIENT tracking
   budgets. At sustained rates near mechanism capacity, later jobs
   legitimately queue longer than either budget; the correct driver
   configuration for such rungs is a per-document deadline exceeding
   the expected queue wait (Little's law) or accepting outstanding
   (not failed) classification. The report's own limitation text
   predicted this: client "failed" ≠ server outcome; job IDs remain
   for reconciliation — and reconciliation here proves 90/90 success.
4. The 56 client-"failed" jobs are therefore recorded as
   **driver-budget artifacts with confirmed server success** — the
   accounting partitions remain exact and reconcilable, which is the
   H04 contract working as designed.

## Stop conditions

None triggered: no OOM/swap spiral, no lease loss, no container
restart, host free memory 20–24% throughout.

## Cleanup / postflight

Stack removed by exact project name (`down`, no `-v`); the project's 3
fixture volumes (postgres/redis/parser-socket) retained per the
no-volume-deletion rule and inventoried; all 13 original live containers
intact; final worker metrics snapshot captured in the private artifact.

## Next rungs (gated as before)

2× burst ≤15 min + drain (needs this receipt + headroom — both now
satisfied, but the burst rung should use a corrected per-document
deadline), then 4-hour soak and the approved long workload — each with
explicit windows. Real-model runs, quality-qualified capacity and
business deadlines remain owner/data gated.
