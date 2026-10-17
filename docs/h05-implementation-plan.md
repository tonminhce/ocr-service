# H05 implementation plan — minimal safe verification controls

Status: plan reviewed before edits. Targets: new
`scripts/verify_capture.py` (small reusable strict-capture helper) +
bounded extension of `scripts/monitoring-smoke.py` `run()` + Python tests.
No new orchestration platform; no change to `production-smoke.sh`'s manual
contract (its loopback defaults and scenario steps stay).

## Problem (from H05 packet spec)

The existing harnesses already enforce: fresh UUID project refusal on
retained resources (`production-smoke.sh` guards), new-output refusal,
umask 077, loopback-only ports, pinned images, mock-only model, finite
delays, scoped cleanup, and postflight checks. What they lack per the
packet's minimal promotion list:

1. Machine stdout must contain exactly one valid complete JSON result;
   stderr captured separately and bounded — currently
   `monitoring-smoke.py::run()` merges both into one log and discards
   stdout for machine use.
2. Both streams hashed and secret-scanned before persistence/reporting.
3. Per-command byte caps on captured streams (output flood bound).

## Minimal change

New `scripts/verify_capture.py` (stdlib-only, ~120 lines):

- `run_captured(command, *, cwd, env, timeout, stdout_bytes_cap=1 MiB,
  stderr_bytes_cap=256 KiB)`: runs a subprocess with stdout/stderr piped
  through bounded readers that truncate-and-flag at the cap (never buffer
  unbounded), then returns a `Capture` dataclass: stdout bytes, stderr
  bytes, truncated flags, returncode, sha256 of each stream, and elapsed.
- `parse_single_json(stdout_bytes)`: strict parse requiring the entire
  stdout be exactly one JSON document (no trailing garbage, no prefix);
  failure is a distinct `CaptureError` — the harness never regex-strips a
  live stream.
- `scan_secrets(stream)`: fixed-pattern scan for the synthetic secret
  format this harness family generates (64-lowercase-hex tokens, known
  fixture secret names in values) plus generic
  `Bearer <32+ printable>` occurrences; findings raise before any
  persistence.
- `preexisting_snapshot(project)`: records (without touching) Compose
  project container/volume/network IDs before a run, returned for the
  postflight owner-check.

Wire into `monitoring-smoke.py`: `run()` uses `run_captured` and keeps its
behavior (writes the bounded, scanned, hashed streams to the private log
dir; machine steps that need JSON use `parse_single_json`). A postflight
helper re-reads the snapshot and asserts originals unchanged. Existing
18-check drill semantics untouched; report gains per-command capture
metadata (hashes only — never raw streams — in the committed-trace
aggregate).

Alternatives rejected: porting the 340-line private coordinator into
maintained source (explicitly forbidden); a new top-level runner; changing
production-smoke.sh's manual flow.

## Invariants

- Manual smoke contract unchanged; loopback-only; no `down -v`/prune; owner
  -checked cleanup of only new resources; failed attempts preserved.
- Streams persisted only under the private 0700 output dir; committed traces
  contain only hashes/counts, never stream contents.
- No new dependencies (stdlib only); both comment guards must stay green.

## Tests (`scripts/test_verify_capture.py`)

1. Exact-single-JSON stdout parses; trailing garbage/newline-prefix fails.
2. Output flood: stdout > cap → truncated flag set, no unbounded growth.
3. Secret scan: synthetic 64-hex token and Bearer value flagged; clean
   output passes.
4. Hash stability: same stream → same sha256; different → different.
5. Fake-command regressions: nonzero exit propagates; timeout raises
   `TimeoutExpired` bounded behavior.
6. Preexisting snapshot: fake docker CLI returns fixed IDs; snapshot parses
   and postflight equality check passes/fails correctly.

Run: `python3 -m unittest discover -s scripts -p 'test_verify_capture.py'`
plus the full scripts discovery; both comment guards; yaml/shell untouched.

## Budget

Unit tests only (no containers, no network); ≤5 s each; helper ≤1 MiB
default caps. Wiring change in monitoring-smoke.py ≤30 lines.

## Acceptance / rollback

All tests green; monitoring-smoke import surface unchanged for CI callers
(argument names identical); comment guards green. Rollback: delete the
helper + revert the wiring; no data impact. A bounded actual synthetic
replay of the monitoring drill is NOT run in this packet (image build
budget); it remains part of H07's canonical candidate rehearsal.
