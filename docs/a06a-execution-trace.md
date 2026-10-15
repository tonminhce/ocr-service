# A06a — Giám sát native subprocess và vòng đời scratch

Ngày 15/10/2026. **A06a IMPLEMENTED / VERIFIED LOCAL + Linux ARM64. A06b parser isolation và O01 observer được hoàn tất sau đó; xem [follow-up trace](a06b-execution-trace.md).** Tài liệu này giữ bằng chứng/lịch sử riêng của A06a. Không đổi bốn container live, native Qwen, credentials, W/R hoặc layout pool đang chạy trong lần kiểm A06a.

## Kết quả đã kiểm

| Phạm vi | Bằng chứng |
|---|---|
| Format + strict Clippy toàn workspace/all targets | PASS |
| Native workspace ordinary suite | **240 passed**,86 integration tests được đánh dấu ignored riêng |
| Native explicit integration suite, serial | **86 passed**, PostgreSQL/Redis dùng riêng; không dùng live DB |
| Worker unit tests trong hai suite trên | **54 passed**; không cộng thêm vào240 |
| Linux ARM64 Docker release verification | **86 passed** =54 worker unit +18 inference contract +14 layout pipeline;1 ignored nằm ngoài lần Docker này |
| Frozen build/contract inputs |119 hashes; không đổi trong verification |
| Independent source review | Hai finding bắt buộc sửa đã được xử lý; review cuối không còn finding bắt buộc sửa trong phạm vi A06a |

Linux verification dùng stage `parser-verification` trong [Dockerfile](../Dockerfile); stage này không đi vào application image. Image local `ocr-service:parser-verify-a06a` có ID `sha256:810f3a61a75238384430e412351eea568a5a0b9baf32382412f2ff9313f52803`. Đây là image kiểm thử, không phải deployment mới. Compose production mặc định và image A05 đã có không được cut over trong phiên này.

Các số trên là số tests cơ chế, không phải số tài liệu, accuracy hay năng lực1.000+ tài liệu/ngày. Linux layout pipeline dùng detector/model HTTP giả có contract thật; không gọi Qwen hoặc xác nhận quality/ONNX-runtime deployment mới. Test CropBox/rotation/DPI thực sự gọi Poppler ở cả Mac và Linux.

## Trace xử lý sau thay đổi

1. `Recognizer::recognize`/`recognize_layout` truyền cancellation token vào `preparation::inspect_with_cancellation`. Compatibility entry point `inspect` vẫn còn. PDF bytes được ghi vào thư mục0700 có đường dẫn tuyệt đối; tên đầu vào do Rust sinh, không lấy filename của upload làm executable/argument.
2. [subprocess_policy.rs](../crates/ocr-worker/src/subprocess_policy.rs) dựng typed `PdfTool::Info`, `Render`, `Text`. Chỉ ba executable `pdfinfo`, `pdftoppm`, `pdftotext` được resolve từ các thư mục cài đặt cố định. Không chạy shell, không lấy PATH hay options từ nội dung tài liệu. Page/render parameters được kiểm tra; CropBox/DPI/scale/orientation semantics hiện tại được giữ.
3. [child_process.rs](../crates/ocr-worker/src/child_process.rs) tạo supervisor riêng giữ **một Poppler slot/process group cho mỗi worker process**. Deadline bao gồm admission. Hủy khi chờ slot không spawn child; hết hạn admission trả lỗi transient. Execution timeout của PDF giữ phân loại permanent hiện tại. Các byte của stdout/stderr được đếm riêng, mỗi pipe tối đa256KiB; stderr không đưa vào kết quả/lỗi.
4. Supervisor sở hữu `Arc<TempDir>` và output guard tới khi cleanup. Caller bị abort/drop đóng oneshot reply; supervisor vẫn chạy để gửi SIGKILL tới process group và reap direct child. Không trả slot chỉ vì caller biến mất. Sau cleanup wait2s mà chưa reap được, trả lỗi static nhưng tiếp tục giữ slot/scratch; không tự spawn thêm process để vượt qua lỗi.
5. Normal child exit được quan sát bằng SIGCHLD + `waitid(WEXITED|WNOHANG|WNOWAIT)`: giữ leader PID ở trạng thái waitable, signal group trước, rồi `Child::wait`. Điều này loại cửa sổ dùng lại numeric PGID sau khi leader đã bị reap. [waitid documentation](https://man7.org/linux/man-pages/man2/waitpid.2.html) mô tả `WNOWAIT` giữ trạng thái waitable; đây là cơ sở cho thứ tự cleanup, không phải chứng nhận sandbox.
6. Mac có trường hợp group chỉ còn zombie trả `EPERM`. Fallback chỉ chạy khi leader vẫn là exited direct child chưa reap; libproc enumeration tối đa256 PID phải đầy đủ, có leader, không có live member. Lỗi/truncation/unknown status giữ fail-closed. XNU [signal implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c) lọc zombie khỏi group iteration; [proc_info definitions](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h) định nghĩa thông tin group/status. Hành vi EPERM đã tái hiện bằng child synthetic trên Mac này.
7. [preparation.rs](../crates/ocr-worker/src/preparation.rs) dùng `Arc<ScratchOutput>` chung giữa caller và supervisor. Partial PNG chỉ bị unlink khi cả hai thả guard; error/cancel không xóa sớm file mà child còn có thể đang ghi. Blocking input writes có một slot riêng, deadline/token; closure tiếp tục giữ slot + scratch nếu waiter bị hủy. Decode vẫn giữ CPU permit trong closure như trước.
8. [post_processing.rs](../crates/ocr-worker/src/post_processing.rs) gọi `PdfTool::Text` qua cùng supervisor, không còn native-text subprocess kế thừa env. Native text vẫn là observation riêng, có thể unavailable; không ghi đè raw OCR hoặc tự chọn nguồn conflict.
9. [layout_runner.rs](../crates/ocr-worker/src/layout_runner.rs) giữ warm pool hiện có; scrub env, resolve binary/model/runtime path trước khi đổi cwd, mỗi helper có process group. Teardown signal group trước khi reap. `shutdown()` giữ JoinHandle trong shared vector qua await, nên abort một shutdown caller không làm lần shutdown sau bỏ qua cleanup.
10. [execution manifest](../crates/ocr-worker/src/inference.rs) thêm `document_process_policy`: revision, slot/pipe limits và `execution_boundary=worker_local_not_sandboxed`. Raw text, Markdown, layout geometry, checkpoint preprocessing revision, conflict selection và confidence không đổi. Đây là implementation declaration của worker, không attest container isolation.

## Environment và tương thích native

Child có sáu biến cố định: PATH=`/usr/bin:/bin`, LC_ALL/LANG=`C`, TZ=`UTC`, HOME=`/nonexistent`, TMPDIR=private scratch. Layout chỉ thêm explicit `ORT_DYLIB_PATH` khi có cấu hình absolute regular file. Không forward DB/Redis/VLLM credentials, `_FILE` paths, proxies hoặc LD/DYLD injection variables. Env scrub không ngăn child mở file secrets cùng UID; đó là việc A06b phải giải quyết.

Poppler resolve trên Mac: `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin`; Linux: `/usr/bin`, `/bin`, `/usr/local/bin`. PATH tùy ý không còn được dùng. `LAYOUT_BINARY=layout-detect` tìm sibling của worker trước rồi fixed directories; native build có thể dùng executable absolute. Relative-with-components bị từ chối. `LAYOUT_MODEL_PATH` relative được chuyển absolute trong parent trước khi child cwd đổi sang scratch. Homebrew symlinks vẫn được phép vì installation/config thuộc trusted operator boundary. ORT loader thực tế vẫn cần kiểm lại ở deployment mới trước release; fake-detector tests không thay kiểm tra đó.

## Những lỗi review đã sửa và lịch sử verification

| Finding | Sửa / bằng chứng |
|---|---|
| Caller abort bỏ qua kill/wait, scratch có thể thả sớm | Detached supervisor giữ permit/resources, nhận reply-close; test giữ scratch tới reap rồi next work tiến triển |
| Layout shutdown drain handles trước await | Giữ handle qua await; test abort first shutdown, second shutdown vẫn chờ delayed cleanup task. Test này dùng gate task để tạo delay xác định; process/reap thật có test riêng |
| Reap leader rồi mới signal cached PGID | WNOWAIT observation trước signal/reap; test quan sát lặp vẫn thấy waitable child, rồi xác minh ECHILD sau cleanup |
| Mac zombie-only group bị hiểu là permission failure | Reproduction syscall + bounded libproc fallback; normal fast exits không giữ slot vô hạn |
| Error-path unlink PNG có thể chạy trước pending cleanup | Bỏ unlink error branch; dùng shared guard |
| Input write và child cwd có thể sống lâu hơn waiter | Blocking closure giữ permit/directory; cancellation/deadline; absolute scratch/model paths |

Development history được giữ ở ignored `artifacts/a06a-verification/logs/`: lần compile đầu thiếu shared `read_limited`; test env count dự kiến7/8 được sửa thành6/7; một lỗi `siginfo_t` không Send được xử lý bằng synchronous syscall frame. Lần unit suite đầu treo ở layout teardown; stack sample được giữ và test process đó được dừng có chủ đích. Sau WNOWAIT, Mac zombie-only EPERM lại làm ba tests lỗi và một test treo; lần đó cũng được dừng sau khi kiểm tra handle sống. Không sửa assertions để che lỗi runtime: sửa lifecycle/platform logic rồi chạy lại. Final suite240+86 và Linux86 là các lần hoàn tất thành công trên119 inputs đã freeze.

Tests mới dùng synthetic credentials/processes, stdout/stderr floods, queued timeout/cancel, parent exit giữ descendant, caller drop, actual child environment, same-group descendant termination, direct-child ECHILD, scratch cleanup, next request và cancelled shutdown barrier. Geometry/EXIF/alpha/JPEG/course golden tests trước đó vẫn qua. Kết quả không phải penetration test hoặc proof rằng kernel không thể giữ process ở D-state.

## Follow-up A06b/O01 and remaining gates

A06a remains subprocess hygiene, not a sandbox; its local process-group guarantees still do not contain kernel-level failure or escaped process groups. A06b subsequently moved Poppler and compressed-image decoding into a persistent parser container with bounded Unix IPC, no network, a separate UID, read-only root, init/reaping, and CPU/RAM/PID/scratch limits. Synthetic IPC/parity, restart, ENOSPC, OOM, and next-job recovery evidence is recorded in the [A06b/O01 trace](a06b-execution-trace.md). Compose demo/smoke overrides now preserve the parser socket mount.

O01 subsequently added a cached worker-only PostgreSQL/Redis operational snapshot; see [observability](observability.md). Neither milestone is penetration-tested or production-certified. The ONNX layout helper remains worker-local; IPC overhead/headroom and broader egress/mount-sentinel review remain open. No live model was changed and no private PDF was reprocessed. Independent labels/calibration, capacity, reviewer identity, alert thresholds, production backup/RPO/RTO, release rollback, and live adoption remain gated.

Máy record: [a06a-verification-2026-10-15.json](research/a06a-verification-2026-10-15.json). Raw logs/source hashes và stack sample nằm trong ignored private evidence directory; không đưa document text hay real credentials vào report.
