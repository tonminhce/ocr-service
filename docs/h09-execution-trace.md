# H09 execution trace — combined API transport memory envelope (READY slice)

Date: 2026-10-18. Packet H09 (READY portion). Scope: one new
`combined_pressure_rss_envelope` test in `ocr-api` transport tests. No
production-code change.

## Status

- Implementation: PASS.
- Verification: PASS (transport suite 4/0/1; workspace 329/0/91; clippy/
  fmt/guards green).
- Measurement: PASS at single-process synthetic scope — RSS plateau
  (steady-tail spread < 64 MiB allowance across 12 combined cycles) with
  explicit slot/429/status-availability accounting. NOT whole-deployment
  capacity; driver-vs-service memory distinction preserved in the
  diagnostic payload.
- Adoption: N/A (diagnostic test).

## What the combined envelope verifies (DR17 invariants, together)

Per cycle (12 cycles, real `server::serve` HTTP/1 stack, loopback,
ephemeral port, 16 MiB large payloads):

1. 4 × 2 MiB upload-shaped POSTs succeed while slots are free, each
   handing off to a large-response admission.
2. 2 slow-reader connections hold both large slots with unread 16 MiB
   bodies; a further upload receives a clean 429 (bounded coupling of the
   upload→serialize handoff to the large budget — observed, not assumed).
3. 8 × `/status` polls all return 200 during large saturation (small
   budget independence — polling is never starved by artifact reads).
4. 4 admitted-but-unawaited `budget.json()` waiters are dropped mid-queue
   (CPU cancellation tail); slots are not leaked.
5. After the slow readers disconnect, both large slots return (2
   concurrent admissions acquirable within 3 s) — reservation trim and
   permit ownership hold under combined load.
6. RSS held/drained samples plateau in the steady tail.

First attempt preserved: uploads initially placed AFTER slot saturation
asserted 200 and correctly failed with 429 — the test design error was
mine, not the server's; the truthful invariant (uploads 429 while both
large slots are held) is now asserted explicitly.

## Explicitly not claimed

Whole-worker RSS, ONNX arena A/B, representative headroom and any
capacity conclusion — those remain gated on host headroom (23% free,
11.95/13.31 GiB swap) and an execution-window decision per the ledger.

## Receipts

- `cargo test --frozen -j 1 -p ocr-api --test transport` → 4 passed /
  0 failed / 1 ignored.
- `cargo test --frozen -j 1 --workspace` → 329 passed / 0 failed / 91
  ignored.
- `cargo clippy ... -D warnings` clean; fmt clean; comment guards 134/0,
  107/0.

## Rollback

Delete the test function; no production impact.
