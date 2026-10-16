# Cargo target optimization: execution preflight

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
