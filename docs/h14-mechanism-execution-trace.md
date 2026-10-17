# H14 execution trace — mechanism capacity ladder, first rung

Date: 2026-10-18. Packet H14 (MECHANISM slice only). Quality-qualified
capacity, real-model goodput, gold and business deadlines remain GATED.

## Status

- Implementation: PASS (first ladder rung executed with the corrected
  driver).
- Verification: PASS (client and durable-server accounting match
  exactly).
- Measurement: MECHANISM goodput only — mock-model 12 s/region latency,
  synthetic two-page fixture. NOT OCR capacity, NOT quality-qualified.
- Adoption: N/A (disposable stack; nothing activated).

## Setup

- Canonical images `ocr-service:h07-local` / `parser-h07-local` (H07
  receipts) in fresh project `rust-ocr-prod-smoke-1792274598-96103`,
  retained via SMOKE_KEEP after the 3-scenario packaged smoke passed
  (stack-up smoke receipts in the private artifact dir).
- W1/R1/L1 production settings, mock-vllm `MOCK_DELAY_MS=12000`
  (the smoke contract's parser-idle-gap floor), synthetic secrets,
  loopback ports 8583/9595/9596.
- Driver: `ocr-eval arrival` fixed-rate 0.05/s, 12 occurrences of the
  synthetic two-page fixture, `--max-pending-tasks 1`, registry 100,
  drain 1200 s, per-document deadline 900 s.

## Results (client report + durable server cross-check)

| Partition | Value |
| --- | --- |
| scheduled/offered/accepted/completed/succeeded | 12/12/12/12/12 |
| rejected/not_submitted/unknown/outstanding/skips | 0/0/0/0/0 |
| offered_per_second | 0.05 (target achieved exactly) |
| maximum_in_flight (submits) | 1 |
| maximum_registry_occupancy | 12 |
| submit latency p50/p95 | 7.1 / 15.2 ms |
| accepted→terminal p50/p95 | 100.0 / 220.0 s |
| schedule / drain | 220.0 / 74.8 s |

Server side (disposable PostgreSQL): 14 jobs `succeeded attempts=1`
(12 arrival + 2 smoke scenario jobs) + 1 `succeeded attempts=2` (the
smoke's deliberate parser-restart scenario — not from this run).
Outbox drained to 0; active leases 0; worker metrics: 15 succeeded,
0 failed, 1 retry_scheduled (smoke scenario), 0 lease_lost/commit
failures/aborts. Every arrival job terminal on attempt 1; no occurrence
key lost or duplicated; the H04 registry tracked 12 jobs through W1
queueing while submit in-flight stayed at 1.

## Reading (mechanism only)

At 0.05 arrivals/s with ~100 s median terminal time under W1/R1/L1 and
12 s mock region latency, the pipeline admitted, queued, processed and
drained 12/12 jobs cleanly — admission, outbox dispatch, leasing,
parser IPC and completion accounting all behaved correctly under
bounded backlog. This is mechanism goodput: it says nothing about OCR
quality, real-model latency/cost, or higher arrival rates. The next
ladder rungs (60–120 min run, 2× burst, soak) need an explicit execution
window decision; real-model runs additionally need headroom + corpus
authority.

## Failed attempts (preserved)

1. First stack-up attempt: `SMOKE_MOCK_DELAY_MS=2000` refused by the
   script's own 10001..60000 idle-gap contract — correct refusal;
   switched to the contract's standard 12000 ms.
2. Second attempt refused on reused `SMOKE_OUTPUT_DIR` — evidence-
   preservation guard working as designed; rerun in a fresh directory.
3. First driver run: duplicate fixture hashes rejected by the dataset
   loader ("duplicate documents cannot enter evaluation") — the driver's
   corpus contract; switched to a single document with 12 occurrences
   (documented workload repetitions, not independent quality samples).

## Resources / cleanup / postflight

- Retained-stack containers/network removed by exact project name
  (`compose down`, NO `-v`); the project's 3 fixture volumes
  (postgres/redis/parser-socket, created 2026-10-17T22:03Z) are
  intentionally RETAINED per the no-volume-deletion rule and inventoried
  above for the owner.
- All 13 original live containers verified intact postflight; zero
  leftover project containers.
- Private receipts under `artifacts/h14-mechanism-<ts>/` (ignored):
  arrival report/stderr, stack-up logs, worker metrics snapshot.

## Remaining gates

Quality-qualified capacity (H10 gold + H11 sidecar review), real-model
runs (headroom window), agreed workload profile/deadlines, burst/soak
rungs, and adoption — all owner/data gated per the ledger.
