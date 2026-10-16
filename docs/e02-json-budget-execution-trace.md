# E02 bounded JSON size checks

Date: 2026-10-16. Status: implemented and verified locally. Scope: remove full serialized buffers used only to enforce existing worker byte budgets. This is a measured allocation correction, not a production memory envelope, OCR speedup or capacity result.

## Finding, research and implementation

The [E02 allocation audit](e02-memory-audit.md) identified three full-result `serde_json::to_vec` calls used only for their length: full-page recognition, course-layout recognition and post-processing. They allocated and copied the complete compact JSON before rejecting output larger than 16 MiB. Layout-metadata accounting already had a bounded counter but could not share it with these paths.

The pinned [serde_json 1.0.154 serializer source](https://github.com/serde-rs/json/blob/v1.0.154/src/ser.rs) shows that `to_vec` constructs a vector and delegates to the writer serializer. The [Rust Write contract](https://doc.rust-lang.org/std/io/trait.Write.html#tymethod.write) requires an error to consume no bytes. The implementation uses the same compact serializer with a counter that checks remaining capacity before advancing. It keeps no serialized output buffer and distinguishes budget exhaustion from another serialization failure.

One private worker module, [`json_budget.rs`](../crates/ocr-worker/src/json_budget.rs), supplies this behavior to the three result checks and existing layout-metadata accounting. All original 16 MiB/4 MiB limits, inclusive boundary behavior, fixed error text and permanent classification remain. Raw JSON values, OCR text, evidence, output shapes and profile semantics are unchanged. The formatting/post-processing closures retain their CPU permits and existing cancellation checks. There are no new dependencies, settings, queues, threads or migrations.

## Pre-edit impact and review

GitNexus query/context and upstream impacts preceded edits. Reports are private under ignored `artifacts/e02-json-budget-2026-10-16/`.

| Existing symbol | Reported risk | Direct callers / indexed processes |
|---|---|---|
| `Recognizer::recognize_document` | LOW; 11 impacted symbols | 2 / 0 |
| `Recognizer::recognize_layout` | LOW; 12 impacted symbols | 1 / 0 |
| `post_processing::apply` | LOW; 6 impacted symbols | 4 / 1 summarized root |
| `layout_metadata_bytes` and its nested counter | LOW; 6 impacted symbols each | 1 / 1 summarized root |
| Nested counter `write` / `flush` | LOW | No indexed callers |

Actual recognition/profile callers and blocking closure ownership were inspected because the graph omits Rust dispatch paths. New helper/test symbols were initially unindexed; their local callsites and test scope were reviewed directly. Independent reviewers found no blocking correctness or compatibility regression. Aggregate change detection also includes the concurrent A06b/B02 work and retains its CRITICAL warning in the [combined continuation evidence](session-resume-2026-10-16.md).

## Verification

| Check | Observed result |
|---|---|
| Initial serial worker library suite | 96 passed, 1 Redis integration test ignored |
| Added extraction-budget regression | 1 passed |
| Inference-contract and layout-pipeline targets | 19 + 14 passed; one Poppler case ignored by the ordinary command |
| Final serial ordinary workspace suite | 297 passed, 90 explicit integration tests ignored |
| Explicit synthetic Poppler ordering/page-limit pipeline | 1 passed after the combined source changes |
| Strict workspace/all-targets Clippy | Passed |
| Workspace format and whitespace checks | Passed |
| No-new-comments / block-comment guards | 128 / 105 source files checked; zero violations |

The targeted checks are subsets/repeats of the final workspace suite and must not be added to its count. New helper tests compare bytes with compact serialization across nested values, Vietnamese/Unicode, escapes, controls, numbers and null; accept exact bounds; reject one byte below/zero; verify overflow cannot advance/wrap the counter; stop before a custom serializer's later element; and distinguish a genuine serializer failure. A post-processing test proves preserved raw fields still count toward the final extraction result budget. Existing course-layout tests still reject aggregate audit/Markdown copies over 16 MiB, and raw-layout/extraction fixtures remain unchanged.

## Bounded synthetic allocation measurement

The ignored `allocation-probe.rs` imports the actual helper source. It runs on one thread with two prebuilt synthetic values below 1 MiB, six iterations per method/case, no network/model/database and no retained fixture allocations in the measured interval. A private allocator wrapper records successful allocation requests and live requested bytes only while a size check runs; a separate timing phase disables tracking. Every measured call checks the exact expected byte count or limit error. Both methods release all measured allocations.

| Fixture | Compact JSON bytes | `to_vec` peak outstanding requested bytes | Counter, successful check | Counter, one-byte-over-limit check |
|---|---:|---:|---:|---:|
| Ordinary synthetic text | 327,770 | 524,288 | 0 | 143 |
| Escape-heavy synthetic text | 286,810 | 524,288 | 0 | 143 |

Successful counter cases made zero measured heap allocations/reallocations. Error cases allocated small serializer/I/O errors: maximum single request 40 bytes and peak outstanding requested bytes 143. The reported peak excludes fixture construction, allocator metadata and transient internals of realloc; it is not process RSS, cgroup peak or whole-worker memory. Source hashes, toolchain, raw reports and probe source are private alongside the test logs.

Timing was mixed: ordinary accepted checks averaged about 126.5 microseconds buffered versus 98.8 counted; escape-heavy accepted checks averaged about 100.9 versus 131.5. These six-iteration observations use an optimized probe with existing debug dependencies, not a release end-to-end benchmark or a statistically established latency change. The verified benefit is removal of an output-sized temporary allocation. No speedup, memory-pressure resolution or concurrency promotion is claimed.

## Limits, next work and rollback

Serialization still traverses and escapes input; a byte counter does not guarantee CPU preemption or immediate cancellation while a string is scanned. Existing `Value`, raw/formatted copies, SQL serialization and request buffers remain. Full-page result construction/size traversal still runs on the async path; this change does not establish heartbeat latency under stress.

The E02 allocator A/B and representative fresh-cgroup memory envelope remain open. W/R must not increase from this microprobe. The existing audit also records region/page/JPEG retention before model admission and overlapping base64/request JSON copies; those require focused changes and measurement. Quality, arrival/burst/soak, hosted release, backup and live-adoption gates remain in the [production execution plan](production-execution-plan.md).

Rollback is a source/image revert with unchanged schemas and output contracts; it restores the size-only temporary allocations. No live stack or real-model settings were changed.
