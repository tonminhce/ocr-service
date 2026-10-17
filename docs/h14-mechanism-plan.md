# H14 implementation plan — mechanism capacity ladder, first rung (READY slice)

Status: plan reviewed before execution. This is the H14 ladder step
"synthetic smoke → 20–30 jobs below saturation" with the CORRECTED H04
driver. Quality-qualified capacity, gold and business deadlines remain
gated; this run measures MECHANISM goodput only.

## Why this is READY now

- H04 driver corrected (submit/track separated, bounded registry).
- H07 canonical images exist (`h07-local`/`parser-h07-local`) and passed
  pipeline+boundary+monitoring drills.
- The smoke overlay provides a disposable production-topology stack with
  a mock vLLM whose delay imitates model latency, plus the real parser,
  PostgreSQL, Redis, outbox/lease/admission machinery — the true
  mechanism under W1/R1/L1.
- Host: 21% free memory, stable; the disposable stack adds ≤1.6 GiB
  bounded containers (postgres 512 MiB… capped per compose), within
  headroom for a ≤30-job run.

## Experiment design (one factor, finite budget)

- Stack: fresh UUID project, canonical images, `MOCK_DELAY_MS=2000`
  (imitates per-region model latency ≈ modest real-model cost; pages
  with ~4-8 regions → ~8–16 s/document), synthetic two-page fixture PDF
  reused (public, 874-byte synthetic).
- Driver: `ocr-eval arrival` fixed-rate against the disposable API:
  - Run A (primary): 24 jobs, rate 0.05/s (1 per 20 s, below saturation),
    `--max-pending-tasks 1`, registry 100, drain 300 s, byte budget
    24 × fixture size. Expect: offered=scheduled, all accepted,
    completed=scheduled, outstanding=0, no skips.
- Observed metrics: report partitions (offered/accepted/completed/
  outstanding/skips), submission + accepted-to-terminal p50/p95,
  scheduler lag, per-job server attempts (expect 1), worker metrics
  (parser/model/queue events) captured post-run.
- Stop conditions: OOM/swap growth >500 MiB, lease failures, any
  unexpected container restart, drain deadline exceeded → record FAIL
  state honestly, no retry-loops.
- Cleanup: stack down (no -v), volumes retained per smoke contract;
  originals untouched; loopback ports overridden (8583/9595/9596).

## Acceptance

- Mechanism receipt: all 24 jobs terminal (succeeded) on attempt 1;
  driver accounting exact (no lost/duplicate occurrence keys); worker
  metrics show bounded parser/model usage; the run's goodput recorded as
  MECHANISM goodput with the mock-model disclaimer (NOT OCR capacity,
  NOT quality-qualified).
- Any deviation → recorded with cause; no config change mid-run.

## Rollback

`docker compose -p <project> down` (no -v); delete run artifacts under
ignored `artifacts/`. No live state involved.
