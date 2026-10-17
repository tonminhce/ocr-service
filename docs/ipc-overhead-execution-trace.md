# Round-6 execution trace — parser IPC overhead measurement (A06b residual)

Date: 2026-10-18. Measurement packet; no production-code change. New
`#[ignore]` probe `parser_ipc_round_trip_overhead_probe` in
`ocr-worker` parser_service tests (real production IPC path: same Unix
socket, protocol, admission and Poppler subprocess).

## Measured (10 samples after 2 warm-up cycles, synthetic two-page PDF,
ARM64 host, in-process service)

| Operation | p50 | p95 |
| --- | --- | --- |
| page_info (metadata round trip) | 7.7 ms | 8.5 ms |
| render_rgb FullPage | 119.6 ms | 120.6 ms |
| render_rgb CourseLayout | 149.7 ms | 158.9 ms |
| full cycle (info + 2 renders) | 277.0 ms | 285.9 ms |

## Reading (evidence, not a change mandate)

- `page_info` ≈ 8 ms bounds the pure IPC+framing+admission overhead of a
  minimal operation.
- Render operations are dominated by the actual Poppler rasterization +
  decode work (~110–150 ms), not by IPC: overhead is ~5–7% of a render.
- Against pipeline context (12 s mock region latency; ~45 s historical
  real-model page latency), parser IPC is negligible. The persistent
  bounded-session design is confirmed non-dominant; no structural change
  is justified by this measurement.
- Scope: synthetic fixture, in-process service, single-session serial
  probe — NOT representative-page-size OCR capacity and NOT a production
  p95.

## Verification

- Probe run: 1 passed / 0 failed (exit 0) with strict single-line JSON
  on stderr.
- Ordinary worker lib suite: 112 passed / 0 failed / 2 ignored (probe +
  preexisting cgroup case — both run-and-passed in round 5's sweep; the
  probe itself passed above).
- fmt clean, strict Clippy clean, comment guards 134/0 and 107/0.

## Rollback

Delete the probe test; nothing else changed.
