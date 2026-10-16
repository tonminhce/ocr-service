# A06b control-response allocation hardening

Date: 2026-10-16. Status: implemented and verified locally with synthetic fixtures. This closes the parser-health allocation finding in the [O01 trace](o01-alerting-execution-trace.md). It is a bounded engineering increment toward production readiness, not production acceptance or a throughput measurement.

## Starting state and scope

The resumed checkout was `codex/rust-ocr-service` at `aa88d07`, following `4f8e8bd` and `6e4e851`. Tracked files were clean; `.claude/`, `AGENTS.md` and `CLAUDE.md` were already untracked harness inputs. The rules, [execution plan](production-execution-plan.md), [session handoff](session-resume-2026-10-16.md), O01 trace and release runbook were read before implementation. No `.codegraph/` directory was present. GitNexus was refreshed from `4f8e8bd` to the current source before exploration and impact analysis.

The health, admission and upload-acknowledgement paths shared `exchange`, which read responses with the generic 128 MiB binary limit. A protocol-valid but unexpected `RgbResult` could therefore allocate a large buffer before the caller rejected its operation. Nonempty `HealthReady` was already rejected by protocol validation; testing that alone would not expose this gap.

## Implementation and impact

The only production behavior change is in [`parser_client.rs`](../crates/ocr-worker/src/parser_client.rs): `exchange` now passes a zero binary-payload limit to the existing frame reader. All three production callers require zero-payload responses: `HealthReady`/`Error`, `Admitted`/`Error`, or `Opened`/`Error`. The frame reader checks lengths from the fixed 20-byte header before reading or allocating metadata/body. Metadata remains subject to its existing 64 KiB ceiling. RGB/native document operations use the separate session request path.

There are no schema, configuration, protocol-version, default-concurrency or dependency changes. No new worker/thread pool, queue or abstraction was added.

Upstream impact for `exchange`, including tests, reported **CRITICAL**: three direct callers (`ParserSession::open`, `health_check`, `admit`), 42 affected symbols and 19 processes. This warning was reported before editing; actual callers and protocol validation were inspected. New regression-test symbols initially returned UNKNOWN because they did not yet exist in the index. An independent source review found no blocking regression.

Both final `detect_changes --scope compare --base-ref codex/rust-ocr-service` and `--scope all` report **CRITICAL**, mapping the test module, `exchange` and the two new tests to 23 execution flows. The graph includes broad cross-module paths; it is not a complete Rust dispatch proof. Manual zero-context diff review confirms one production-line change plus test imports and tests in one source file. Database, review, lease, queue and existing metric implementations are unchanged.

## Verification record

Private logs and graph reports are under ignored `artifacts/parser-control-hardening-2026-10-16/`.

| Check | Result |
|---|---|
| Regression before the production fix | Failed as intended: header-only response timed out instead of being rejected immediately as IPC failure |
| `cargo test --locked -p ocr-worker --lib parser_ -- --test-threads=1` | 27 passed |
| `cargo test --locked -p ocr-worker --lib observer::tests:: -- --test-threads=1` | 8 passed; one overlaps the parser-filter run |
| Inference-contract target | 19 passed; one Poppler test ignored by the ordinary command |
| Layout-pipeline target | 14 passed |
| Explicit ignored synthetic PDF ordering/page-limit test | 1 passed using installed Poppler |
| `cargo clippy --locked -p ocr-worker --all-targets -- -D warnings` | Passed |
| `cargo fmt --all -- --check` and `git diff --check` | Passed |
| `python3 scripts/check_no_new_comments.py` | 127 source files checked; zero violations |
| `python3 scripts/check_no_block_comments.py` | 104 source files checked; zero violations |
| GitNexus compare and all scopes | Completed; CRITICAL scope retained and reviewed |

The two new tests contain 12 header-only control-exchange cases and eight public health cases. Header-only cases cover health/admission/upload with advertised lengths 1, 3, 128 MiB and `u64::MAX`, while the peer holds the socket open and sends neither metadata nor body. They require the canonical retryable IPC failure, so EOF and timeout cannot satisfy the assertion. Health cases cover idle/full valid capacity, invalid capacity, unexpected control/native operations, a protocol-valid 1×1 RGB payload and canonical Busy handling.

Existing parser tests cover binary render round trips, PDF/native parity, full-parser health, admission recovery, typed errors, stalled frames and session cleanup. Pipeline fixtures use mock HTTP/model/layout responses and synthetic image/PDF input. Builds used one Cargo job and tests ran serially. No real inference or database-backed live workload was invoked. These commands exercise 68 distinct passing tests; the overlapping parser/observer case was run twice. Full workspace, packaged Linux/image checks and production load runs were not repeated for this local increment.

Incremental GitNexus refresh after the patch failed with the previously observed inconsistent FTS delete error. A forced index-only rebuild with one worker and a larger WAL checkpoint budget succeeded, producing 3,858 nodes, 10,766 edges and 300 flows. Final scope checks ran after that rebuild. The partial graph's temporary LOW/no-flow result was not accepted as evidence; the original failure log is retained.

## Architecture and performance review

Independent source review checked the actual worker, inference, parser, layout and subprocess paths. The worker already uses Tokio's multithread runtime, W-bounded job admission, a shared process-wide R semaphore, finite per-job region buffering, and CPU `spawn_blocking` closures that retain their permits through cancellation. Parser decode and native subprocess work have separate finite admission. Layout process cleanup retains ownership through kill/reap. Another thread pool is not justified by this finding.

The [E02 audit](e02-memory-audit.md) still identifies the relevant performance follow-ups: JPEG-bearing region futures are prepared before global model admission, so retained page/crop memory can grow with W×R; full-result serialization used only for size checks allocates a complete temporary buffer; base64/data-URL/JSON copies overlap inference waits. These are candidates for small, measured changes before increasing W/R. This control-response fix removes an avoidable malformed-frame allocation; it establishes no improvement in normal OCR latency or goodput.

The read-only preflight reported swap usage of 16,500 MiB out of 17,408 MiB, with 908 MiB free. That snapshot does not measure sustained memory pressure, but provides no new headroom proof for real-model/concurrency experiments. No W/R, model or live resource settings were changed. All 13 previously running application/test container IDs remained running; no Docker service, volume, credential or remote branch was modified.

## Remaining gates and rollback

- The subsequent [phase/deadline increment](a06b-phase-deadline-execution-trace.md) adds one total health deadline across connect/exchange/validation for all callers.
- That increment also restricts upload bodies to the admitted upload phase and verifies header-only rejection/recovery in initial and established phases. The broader malformed-frame/egress/mount matrix remains A06b work.
- Representative memory measurements, real-model staging and arrival/burst/soak capacity evidence remain E02/E04 gates. No 1,000-plus-documents/day claim follows from these fixtures.
- Hosted CI, trusted registry/provenance, production configuration/model approval, independent labels, reviewer identity, off-host backup decisions and operational monitoring/adoption retain the gates in the execution plan.

There is no data/schema migration to roll back. Binary rollback follows the [release runbook](release-operations.md); a previous client restores the broader control-response allocation bound, which remains part of its rollback risk assessment. This checkout has not been deployed or published.
