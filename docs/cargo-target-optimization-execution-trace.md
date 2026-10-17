# Cargo target optimization: execution and verification

Current checkpoint, 17/10/2026: cache compaction, native workspace verification and full isolated Linux release/integration acceptance passed; neither profile challenger was promoted. Target allocation decreased from 42.24 to 2.49 GiB. The 16/10 preflight below is historical, not the final outcome. Final Linux results and owned-lab cleanup are recorded below.

Resource preflight ngày 16/10/2026, 23:23–23:30 Asia/Saigon. Source checkpoint: `43dde3e79b1c5b188953ca3b404ad7dfda54e5a3`. Người dùng đã yêu cầu cung cấp kế hoạch chi tiết trước rồi thực thi; [kế hoạch đã commit](cargo-target-optimization-plan.md) là phạm vi thực hiện.

## Kết quả và completion boundary

**P2 preflight NO-GO; chưa chạy compilation pilot hoặc full workspace A/B.** Đã thực thi kiểm tra source/resource và đánh giá điều kiện an toàn. Không áp dụng dev profile mới khi chưa có benchmark/tests/debugger evidence. Không suy từ disk cache sang OCR runtime performance.

| Pha | Kết quả hiện tại |
| --- | --- |
| P0 | Audit disk/profile/hardlinks đã có; source/toolchain/lock identity được kiểm lại |
| P1 | Giữ nguyên target hiện tại; không chọn cleanup trong lượt thực thi này |
| P2 | Native full-workspace A/B chưa đủ headroom; conditional leaf pilot cũng bị idle gate chặn |
| P3 | Incremental on/off chưa thử |
| P4 | Chưa có config promotion; chỉ commit execution documentation |
| P5 | Cache-growth observations sau adoption chưa có |

## Pre-flight và source identity

- Tracked worktree ban đầu sạch. `.claude/`, `AGENTS.md`, `CLAUDE.md` là untracked harness đã có, không đưa vào commit.
- Cargo và rustc đều `1.98.1`; native host `aarch64-apple-darwin`. Cargo.lock SHA-256: `ff19b368f40c4c2c324c27c70ed951e331c1d434b5c3bb78df15cfaadca1e765`.
- `git diff 69c55d3 HEAD -- Cargo.toml Cargo.lock crates scripts tools ops` và working-tree diff tương ứng đều rỗng. Các application/build files không đổi từ source checkpoint đã kiểm trước; không tái sử dụng test counts cũ làm kết quả mới.
- GitNexus index ở `69c55d3`, stale so với HEAD chỉ có documentation changes. Không sửa application symbol hoặc chạy reindex gây thêm tải trong lượt resource-gated này. Change detection vẫn chạy trước commit; giới hạn index được giữ rõ thay vì giả định đã refresh.
- Không tạo frozen source copy hoặc experiment target directories sau gate failure. Chỉ tạo ignored evidence directory nhỏ; không đọc lại PDF/labels/OCR values.

## Vì sao không bắt đầu full native workspace build?

Snapshot đầu lúc 23:23:58 ghi nhận RAM 16 GiB, swap dùng 14,588.56 MiB trên tổng 15,360 MiB, `memory_pressure -Q` báo free 19%, khoảng 8 GiB wired và 4.5 GiB compressor. Disk còn khoảng 207 GiB; disk không phải điều kiện chặn tại thời điểm đó. Các snapshot tiếp theo có reserve tốt hơn, nhưng vẫn cần idle gate trước build.

Swap đã dùng lớn **một mình không chứng minh thiếu headroom hiện tại**. Quyết định không chạy full workspace dựa trên tổng hợp reserve/pressure chưa ổn định, pageout activity, đang có live workload và không có native hard memory ceiling được thiết lập. `-j 1` chỉ giới hạn Cargo jobs, không chặn toàn bộ compiler threads hoặc đặt hard memory limit. Independent review đồng ý không bắt đầu full matrix trong điều kiện này.

Read-only process inventory có native `VLLM::EngineCore` khoảng 3.40 GiB RSS tại một snapshot. Đây không phải phép đo toàn bộ model/unified memory, không cộng với container/host metrics để quy tổng RAM. Không dừng model hoặc service để lấy chỗ build. Mười ba containers đang chạy vẫn được giữ; không thực hiện Docker lifecycle/configuration operation.

## Conditional leaf pilot và ngưỡng khóa trước khi đo

Để tránh triển khai full matrix khi chưa an toàn, chỉ cân nhắc `ocr-extraction --lib --no-run` trước: dependencies không gồm SQLx/reqwest/image/ONNX. Candidate vẫn chỉ đổi dev debuginfo; source/features/toolchain/jobs phải giống baseline. Đây chỉ là pilot measurement mechanism/leaf crate, không đủ chứng minh workspace savings hoặc cho phép adoption.

Trước khi chạy bất kỳ compiler nào, idle gate dùng bốn samples cách nhau 10 giây, với các điều kiện sau được khóa trong command trước khi có kết quả:

| Chỉ số | Điều kiện GO cho leaf pilot |
| --- | ---: |
| `memory_pressure -Q` reported free | Tối thiểu 20% ở mọi sample |
| Swap used tăng so với sample đầu | Không quá 32 MiB |
| Pageout growth toàn cửa sổ khoảng 30 giây | Không quá 16 MiB |
| Swapout growth toàn cửa sổ | Không quá 32 MiB |

Đây là guard bảo thủ cho thí nghiệm local, **không phải tiêu chuẩn OS health hoặc production SLO**. Nếu được phép bắt đầu sau một preflight mới, pilot còn phải có deadline năm phút, target budget khoảng 2 GiB, bounded logs, largest-compiler-RSS soft stop 512 MiB và pressure/swap/service watchdog. Watchdog native chỉ là soft stop; không được gọi là cgroup memory enforcement. Khi cần dừng, chỉ terminate process group do lượt pilot tạo, không kill theo tên chung.

### Kết quả idle gate thực tế

Cửa sổ 23:28:05–23:28:35 Asia/Saigon:

| Sample | Reported free | Swap used MiB | Pageouts | Swapouts |
| --- | ---: | ---: | ---: | ---: |
| 1 | 21% | 14,482.69 | 3,403,994 | 72,154,695 |
| 2 | 21% | 14,466.69 | 3,404,657 | 72,154,695 |
| 3 | 20% | 14,442.69 | 3,404,840 | 72,154,695 |
| 4 | 23% | 14,434.69 | 3,405,169 | 72,154,695 |

Page size từ `vm_stat` là 16,384 bytes. Pageout delta 1,175 pages = **19,251,200 bytes = 18.36 MiB**, vượt 16 MiB. Swapout delta bằng zero; swap used giảm 48 MiB. Các tín hiệu cải thiện được ghi nhận đầy đủ, không gọi đây là sustained swap growth. Gate vẫn **FAIL vì pageout**, nên không nới ngưỡng hoặc lấy thêm samples chọn lọc để cho pilot chạy trong cùng lượt.

## Những gì chưa chạy, bước tiếp theo và rollback

1. Cần giải phóng/điều phối host workload do người dùng quản lý, hoặc chọn thời điểm máy ít tải hơn; không tự dừng model/live stack hoặc giảm Docker VM resources.
2. Sau khi host state thay đổi, chạy lại cùng preflight; không chỉ nhìn free disk hay swap-used snapshot. Nếu vẫn FAIL thì giữ NO-GO.
3. Khi GO, tạo disposable frozen source với source/lock hashes đã ghi và fresh owned cache; pilot baseline `cargo test -p ocr-extraction --lib --no-run --frozen -j 1`, sau đó candidate tuần tự khi headroom trở lại baseline. Cả hai có explicit isolated target paths; kiểm effective dev/test flags.
4. Chỉ mở rộng sang same-workflow workspace cold/unchanged/edit suite sau pilot và headroom gate. Không promote profile từ một leaf result; backtrace/LLDB, tests và acceptance thresholds trong kế hoạch vẫn bắt buộc.
5. Existing target, release profile, runtime/model concurrency, binaries/evidence và containers/volumes giữ nguyên. Không có config change cần rollback; ignored preflight records được giữ làm evidence. Không chạy cleanup hoặc remote push.

## Evidence và verification

Ignored evidence ở `artifacts/cargo-target-pilot-2026-10-16/`: `idle-preflight.json`, `execution-preflight.json` và read-only `container-status.txt`. Metadata/resource samples không chứa private OCR hoặc credential values.

Đây là documentation-only checkpoint. Hai source comment guards PASS với 130/105 files và zero violations; whitespace check và bốn local links đều qua. Private JSON arithmetic/state checks xác nhận pageout delta và không có build/adoption. Hai lệnh GitNexus compare/staged đều trả `No changes detected` dù Git có hai documentation files staged: hai tài liệu này chưa có trong index cũ, nên đây không phải bằng chứng Git diff rỗng. Manual staged-scope review xác nhận chỉ plan/trace, không source/config/artifacts. Không chạy Cargo build/test/Clippy sau idle gate failure. Kết quả runtime/tests/profile savings còn **NOT_MEASURED**, không phải PASS. Independent review bao phủ conditional pilot và quyết định không bỏ qua safety gate.

## Continuation on 17/10: actual cleanup and bounded execution

The user explicitly requested goal-driven execution, deep review, performance work and fixing the oversized cache rather than stopping after status/preflight. The initial pageout-only veto was retained as historical evidence, then replaced by a declared active-watchdog policy. High swap use or pageouts alone are not proof of critical host pressure. Source/build identity, free-memory reserve, swap growth, process/group RSS, disk reserve, deadlines and running-service identities were checked during commands.

Native Cargo commands used one job and owned process groups, sampled process RSS with a 750 MiB soft stop and group RSS with a 1.25 GiB soft stop. Host pressure/swap limits required sustained observations, with a fixed stage-wide swap budget and finite resume/deadline limits. These are sampled watchdog stops, not native hard memory enforcement. Two earlier tool/build attempts stopped with partial cache retained; their incomplete cold timing was not used as a successful A/B result. Later fresh-source/fresh-cache runs completed after host conditions improved.

### Cleanup scope and preserved developer tools

| Stage | Target allocated bytes | Approximate GiB |
| --- | ---: | ---: |
| Original audited cache | 45,356,523,520 | 42.24 |
| After incremental-only removal | 30,942,855,168 | 28.82 |
| After in-place debug-cache compaction | 2,668,896,256 | 2.49 |

Incremental removal reduced unique allocation by exactly 14,413,668,352 bytes, matching the earlier last-link estimate. Subsequent compaction removed another 28,273,958,912 bytes. The combined target-allocation reduction is approximately 94.1%. Filesystem `df` deltas were recorded but are not attributed solely to this work; APFS/background activity differ from allocated-file accounting.

The retained allowlist contains 2,595 unique files/inodes: eight native CLI binaries, ten canonical examples, 47 dylibs, three Cargo lock files and 2,527 mapped debug objects/archives. Sixty-five Mach-O debug maps were streamed without linking DWARF. Seven external toolchain identities remain unchanged. All retained hashes, modes, mtimes and inodes matched before/after; link counts may decrease. An offline LLDB lookup on a retained canonical example still resolves its source line.

Cleanup held exclusive `flock` locks on Cargo 1.98.1's build, compatibility and artifact lock files. A separately owned Cargo check was observed blocking on those locks, then terminated before mutation. The lock-file inodes and retained files stayed in place throughout. Compiler fingerprints were removed first, then only unretained generated files under `target/debug` were removed. This avoids a nested Cargo-clean deadlock and the clean/restore race. Release allocation remained exactly 1,066,602,496 bytes; source, private documents/labels/OCR, logs, frozen Linux exports and volumes were outside the removal scope.

This is cache reset with preserved developer tools, not proof that every removed hash was stale. Generated test executables/variants can be rebuilt. Fingerprints are intentionally absent, so the next canonical build is cold; do not clean before every build. Retained historical binaries are not claimed to attest the new source merely because their hashes/mtime were preserved. Rebuild when current-source tooling/debugging is required.

### Fresh native profile comparison

Both variants used the same frozen 120 compiled-source paths, lockfile, native Rust/Cargo 1.98.1, features, one job and isolated targets with empty dev build caches. Earlier timing reports/compiler-version metadata were retained, not dependency/codegen artifacts. The workflow includes all-targets check, workspace test compilation, examples, ordinary tests, strict all-targets Clippy and restored test compilation. Cargo build cache was fresh, not necessarily the OS filesystem cache. Source identity was verified again after the synthetic edit probe was removed.

| Configuration | Allocated bytes after the same workflow | Ordinary tests | Decision |
| --- | ---: | ---: | --- |
| Existing full debug + incremental | 5,535,707,136 | 315 passed, 90 ignored | Retained |
| Line tables + incremental | 4,905,578,496 | 315 passed, 90 ignored | Not promoted: only 11.38% smaller, below proposed 25% gate |
| Full debug + nonincremental | 3,946,147,840 | 315 passed, 90 ignored | Not promoted: edit pilot regressed materially |

Strict Clippy passes for all three configurations. The baseline/line-tables unchanged build medians were both 0.14 s across three alternating repeats. Actual compiler invocations verify both library and test debuginfo flags. LLDB resolves the extraction function's file/line for both profiles. A separate synthetic panic probe verifies function/file/line in backtraces for both; variable/type debugging is deliberately not promised by line tables.

Nonincremental saved 28.71% allocation, but the same synthetic public-constant edit in a disposable source snapshot took 63.7 s versus 24.9 s with incremental, approximately 2.56×. This is one exploratory paired edit, not a three-repeat median or production percentile. It was rejected without spending more runs on a clearly unviable local edit-loop candidate. Default codegen units also change from 256 to 16 when incremental is disabled; the result describes the practical configuration, not an isolated pure-cache effect. The synthetic constant never entered maintained source and all 120 original source hashes were restored.

Therefore `Cargo.toml`, `Cargo.lock`, release settings and runtime concurrency remain unchanged. No custom profile hierarchy, cache daemon or speculative configuration was added. The dominant 42 GiB problem was accumulated generated artifacts, not evidence that production binaries require 42 GiB. A fresh development workflow is much smaller. Keep one stable normal workflow; review growth above the proposed 20/30 GiB warning/review levels rather than deleting before every build. Long-term growth and a final policy budget are not established by this one local experiment.

### Source optimization and review

The [post-processing reuse increment](e02-postprocessing-reuse-execution-trace.md) removes a redundant extraction only when block equivalence is proven. Full-output parity covers ordering, native/recovery paths, budget failures and empty input. Seven synthetic allocation-probe rounds reduce raw-issue allocation requests from 3,012 to 903 and requested bytes from 191,600 to 55,716, with equal serialized issues. This is not whole-worker peak memory or OCR goodput evidence.

### GitNexus recovery and limits

An interrupted analyzer/FTS repair left shadow-page and orphan-FTS state; failed attempts and a database backup were retained. A read-write recovery was not falsely reported as a source refresh. The final successful full index rebuild uses the installed CLI algorithms, one parser worker and index-only mode. A scoped private preload supplies the public native Database constructor with a 256 MiB buffer pool; V8 heap is 256 MiB and the checkpoint threshold is 64 MiB. No installed tool or application algorithm was edited.

The 128 MiB trial exhausted its internal buffer pool; the default/native startup exceeded the original 750 MiB tool stop. With stable host reserve, one explicitly reviewed tool-only budget allowed 1.25 GiB per process and 1.75 GiB sampled group RSS; compiler limits were not raised. The successful run completed in 9.66 s, reported a maximum RSS of 973,619,200 bytes and no swap-used increase. Source graph/keyword query and impact analysis were checked afterward. This is a local tooling resource workaround, not an official GitNexus buffer-pool environment flag or a production-memory optimization.

### Final verification boundary and private evidence

Native formatting, both comment guards, 42 Python tests, ordinary workspace tests and strict Clippy pass for the code/configuration tested. Fresh Linux release verification and all 90 explicit cases have also passed independent final acceptance in an owned, internally networked disposable lab with no published ports or Docker volumes. Existing verify-cluster objects caused the stricter empty-cluster guard to fail, so they were left alone rather than deleted or used as fixtures.

Private evidence is under `artifacts/cargo-target-execute-2026-10-16/` and `artifacts/cargo-performance-2026-10-17/linux-evidence/`. It includes preservation manifests, lock-interoperability logs, actual allocation reports, failed/successful commands, source hashes, profile decisions, debugger/backtrace results and cgroup/lab state. Both experimental native target caches were retired after small timing reports were copied to the evidence directory: their final pre-clean allocations were 6,351,364,096 and 3,945,291,776 bytes, and both are now zero. These later totals include additional probes and differ from the frozen comparison rows above. No giant cache was relocated and called reclaimed space. No push, live cutover, real-model call, W/R increase or volume deletion is authorized by these results. Real quality, representative RSS/capacity, reviewer identity and operational production gates remain open.

The native checkpoint stages exactly five files: one worker source file and four documentation files. Both required GitNexus compare/staged checks report LOW, eighteen mapped symbols and zero mapped execution flows. This does not negate the separate HIGH upstream warning or prove absence of dynamic worker callers; actual source/callers and full-output tests were reviewed. The new trace was separately link-checked because it has no indexed symbols. Both comment guards pass with 130/105 files and zero violations; 42 Python tests, formatting/whitespace and all 120 compiled-source hashes pass. Private fixtures, binaries, cache scripts, logs and the preexisting untracked harness files are excluded from the checkpoint commit.

### Completed Linux release acceptance at source commit `1f5c6c4`

The full source increment above is committed locally as `1f5c6c4`. Linux ARM64 verification uses the immutable cached Rust/Cargo 1.99.0 builder, read-only compiled source mounts, one CPU, 1 GiB memory with no container swap and 256 PIDs. No profile/Rust flags override was present. Release thin LTO and stripping remain exactly the repository defaults.

| Accepted check | Result |
| --- | --- |
| Compiled source identity | All 120 repository paths match; all 238 frozen snapshot paths match their own manifest |
| Ordinary workspace tests | 316 passed, zero failed, 90 ignored, across 22 expected source-keyed target groups |
| Explicit disposable tests | All 90 passed; zero failed or ignored |
| Strict release Clippy | Workspace/all-targets with `-D warnings` passed |
| Export inventory | Exactly 12 executable ELF64 little-endian AArch64 binaries, individual SHA-256 recorded |
| Successful-phase cgroup peaks | Workspace tests 597,921,792 bytes; Clippy/exports 810,995,712 bytes (773.43 MiB) |
| Successful-phase OOM deltas | Zero OOM, OOM-kill and OOM-group-kill events |
| Fixture postflight | No extra PostgreSQL database/test schema/backup-role dependencies; Redis maxmemory zero/noeviction and zero observed remaining keys |
| Cleanup | Only the four owner-labelled builder/fixture containers and their internal network removed; no volume deletion; all 13 original running IDs preserved |

Native ordinary count remains 315. Linux adds the platform-specific `process::tests::enospc_and_sync_failure_never_return_successful_digest`; neither the 90 ignored ordinary cases nor the earlier narrow 14-test run is double-counted. The explicit cases use only fresh synthetic PostgreSQL/Redis fixtures plus the existing synthetic RSS and Poppler cases.

An earlier aggregate workspace compilation reached its 900-second command deadline (exit 124, no OOM, approximately 922.73 MiB peak). Continuation split the existing workspace target selection into bounded, checkpointed groups without changing source, features, release profile or hard limits. The completed test phase then stopped because the base image did not include Clippy. Its successful tests and failure log were preserved separately. The exact Rust 1.99.0 ARM64 Clippy component was obtained from the official distribution, checked against the manifest-pinned SHA-256, then installed offline only in this disposable builder. Strict Clippy and all binary exports completed with exit zero. This is not a signed-image provenance claim. A counter-copy permission error on the first continuation launch was retained and fixed in private orchestration; it did not run a compiler or change application source.

The independent acceptance audit validates the exact group inventory, source-keyed checkpoints and log hashes, rather than subtracting a fixed native count from a resumable aggregate log. It binds the successful Clippy command to the ordered continuation, terminal exit zero and complete export phase. Initial failed fixtures, timed-out compilation and missing-component attempts remain explicit historical evidence, not completed passes. Pre-cleanup inspections and `cleanup-audit.json` retain the owner/limit/source binding after the disposable containers are retired.

The canonical native `target` is still 2,668,896,256 allocated bytes (2.49 GiB) after verification. The Linux build cache was retired with its owned builder, not moved into another permanent target. This closes the Cargo cleanup and guarded reuse package; canonical runtime packaging, current-image replay/provenance and the execution plan's real quality/capacity/operations gates remain separate.
