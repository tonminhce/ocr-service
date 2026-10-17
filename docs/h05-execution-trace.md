# H05 execution trace — minimal safe verification controls

Date: 2026-10-18. Packet H05. Scope: new stdlib-only
`scripts/verify_capture.py` strict-capture helper + bounded wiring into
`monitoring-smoke.py` + 15 unit tests. No new orchestration platform; the
manual `production-smoke.sh` contract is untouched.

## Status

- Implementation: PASS.
- Verification: PASS (62 Python tests total, 15 new; import surface
  checked path-independently and via the exact CI invocation).
- Measurement: N/A (tooling controls).
- Adoption: the strict capture is live for the NEXT monitoring drill run;
  no container drill was executed in this packet (image build budget — the
  bounded actual synthetic replay belongs to H07's canonical candidate
  rehearsal, per plan).

## What was promoted (minimal set)

Existing harnesses already enforced fresh-project refusal, new-output
refusal, 0700 outputs, loopback ports, pinned images, mock-only model and
scoped cleanup. The promoted gaps:

1. `run_captured()`: subprocess with `communicate(timeout)`; the timeout is
   the hard process bound (kill + reaped on expiry), and stdout/stderr are
   capped post-capture at 1 MiB / 256 KiB with explicit `*_truncated`
   flags — an evidence budget, never silent growth. Each stream gets a
   SHA-256 and elapsed time.
2. `parse_single_json()`: strict "exactly one complete JSON document"
   parsing — prefix noise, trailing garbage and multi-document streams all
   fail; the harness never regex-strips live output.
3. `scan_secrets()`: flags synthetic 64-hex tokens (opt-out where fixture
   hashes are legitimate) and `Bearer`-shaped credentials BEFORE any
   persistence; `monitoring-smoke.py` scans both streams with the hex64
   exemption (its client output legitimately contains the synthetic PDF
   SHA-256) while Bearer scanning stays mandatory.
4. `preexisting_snapshot()` / `assert_originals_unchanged()`: read-only
   Compose container/volume/network ID snapshot for postflight owner
   checks.

`monitoring-smoke.py::run()` now routes through `run_captured`, persists
the bounded scanned streams under its private 0700 log dir with an explicit
stdout/stderr separator, and appends per-command capture metadata (hashes,
flags, elapsed — never raw contents) to the report's new `captures` array.
Existing 18-check drill semantics, argument names and CI invocation are
unchanged.

## Explicitly NOT done (per plan/YAGNI)

- The 340-line private coordinator was NOT ported into maintained source.
- No new runner/platform; `production-smoke.sh` manual flow untouched.
- No actual container drill in this packet.

## Tests (`scripts/test_verify_capture.py`, 15 cases)

Single-JSON strictness (exact/trailing/prefix/two-docs/empty); secret scan
(hex64 flagged, Bearer flagged, clean pass, hex exemption still catches
Bearer); capture behavior (stdout/stderr/returncode/hash equality, flood
cap+flag at 1024 bytes, nonzero exit, timeout kill raises `TimeoutExpired`
bounded); snapshot parsing with a fake docker CLI and postflight
original-changed detection.

First attempt found two defects, preserved: (a) my initial
read-then-wait design deadlocked on silent children (the suite hung 60 s
until outer teardown) — replaced with `communicate(timeout)` + post-hoc
caps; (b) a hex64 test fixture embedded `token=` mid-run, breaking the
64-hex sequence — test corrected. Both are recorded as failed attempts.

## Verification receipts

- `python3 -m unittest discover -s scripts -p 'test_*.py'` → 62 passed /
  0 failed (47 pre-H05 + 15 new).
- `python3 scripts/monitoring-smoke.py --help` → OK; path-independent
  import via `importlib` → OK.
- Comment guards: 132 files / 0 violations; block comments 105 / 0.
- Rust untouched — no rebuild required for this packet.

## Rollback

Delete `verify_capture.py` + `test_verify_capture.py`, revert the
`monitoring-smoke.py` wiring. No data/runtime impact.

## Remaining

The strict-capture controls run in the next real drill (H07 candidate
rehearsal). Hosted CI execution of the drill requires push authority
(gated). The report's committed aggregates carry hashes only.
