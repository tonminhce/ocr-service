# Round-8 implementation plan — H14 ladder rung 2: 60-minute sustained arrival

Status: plan reviewed before execution. Continues the H14 mechanism
ladder after rung 1 (12/12). Long-run scope is now concrete from rung-1
receipts; host stable (24% free); execution window = this round, declared
finite.

## Rung 2 design (60–120 min target arrival, mechanism-only)

- Stack: fresh UUID project, canonical `h07-local`/`parser-h07-local`
  images, W1/R1/L1, mock-vllm 12 s/region (rung-1 identity), synthetic
  two-page fixture, loopback ports 8583/9595/9596, SMOKE_KEEP=1 via the
  packaged smoke (proves stack health before the run).
- Rate: 0.025/s fixed (1 job/40 s) — at rung-1's measured terminal
  latency (~100 s median under W1 with queue), this is the near-steady
  mechanism rate: offered ≈ drain, bounded backlog. Duration: 3,600 s →
  90 jobs. Driver: `--max-pending-tasks 1`, registry 200, drain 1,800 s,
  deadline 900 s, byte budget 90×874.
- Observed: all H04 partitions, terminal p50/p95, queue depth (worker
  metrics: queued/eligible/oldest age), outbox/lease health, per-job
  attempts (expect 1), host memory guard (abort if free <12% or swap
  grows >500 MiB).
- Stop conditions: OOM/swap spiral, lease loss, container restart,
  non-drain within budget, driver accounting mismatch. Any stop →
  record FAIL state with cause; no mid-run config change.
- Budget: schedule 60 min + drain ≤30 min expected (worst-case bound
  90 min drain declared). Cleanup: stack down (no -v); volumes retained
  + inventoried as before.

## Acceptance

All scheduled jobs terminal (succeeded, attempt 1); partitions exact;
backlog bounded (report + server cross-check); worker metrics captured
pre/post. MECHANISM goodput receipt only — mock model, synthetic
fixture; no OCR-quality or capacity claim. The next rung (2× burst)
waits for this receipt + remaining host headroom.

## Rollback

`compose down` (no -v); delete private run artifacts. No live state.
