# H09 implementation plan — combined API transport memory envelope (READY slice)

Status: plan reviewed before edits. Target: one new test in
`crates/ocr-api/tests/transport.rs`. No production-code change.

## Scope (honest)

Real ONNX arena A/B and worker/backend representative RSS remain gated on
host headroom (23% free, 11.95 GiB swap used) and an execution-window
decision. This packet delivers the DR17 READY slice: a combined
synthetic pressure diagnostic of the API transport under simultaneous
large-response backpressure, status polling, upload-shaped POST bodies and
cancelled response-CPU waiters, verifying the invariants that already
exist and measuring the plateau.

## Design

New `combined_pressure_rss_envelope` test in the existing transport
fixture (real `server::serve` stack, loopback, ephemeral port):

1. Add an `/upload` POST route to the fixture that reads a bounded body
   (reusing `DefaultBodyLimit`-style cap via an explicit length check)
   and then requests a large response admission — simulating
   submit→serialize handoff without a DB.
2. Cycle count bounded (12): hold 2 unread `/large` bodies (slow readers),
   POST 2 max-size uploads, poll `/status` 8× (must stay 200 even while
   large slots are saturated — small budget independence), drop 4
   admitted-but-unawaited `budget.json()` waiters mid-queue (CPU
   cancellation tail), then drain: unread bodies dropped, wait for both
   large admissions to be acquirable again (slots returned).
3. Sample RSS at held and drained points (existing `rss_kib` pattern);
   assert steady-tail spread < 64 MiB (same diagnostic allowance as the
   existing test) and that every cycle returns to full capacity
   (2 concurrent `budget.json()` admissions acquirable).
4. Emit a strict single-line JSON diagnostic to stderr with
   held/drained samples, spread, cycle count and scope disclaimer.

Invariants verified: reservation trim returns unused bytes; cancelled
CPU waiters don't leak slots; small-status budget independence under
large saturation; upload bodies don't couple to response slots.

## Tests/acceptance

The new test plus the 4 existing transport tests green; no production
code touched; workspace gates; comment guards. Rollback: delete the test.

## Budget

In-process only; ≤30 s; payload 2 MiB × 2 bodies + 2 MiB × 2 uploads per
cycle × 12 cycles (transient); no DB/model/containers.

## Research

No unresolved external fact: hyper/axum behavior is exercised directly by
the real serve stack in-test; the plateau allowance matches the existing
accepted diagnostic.
