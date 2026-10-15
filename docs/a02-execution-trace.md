# A02 — Enqueue, SQL và request deadlines

Trạng thái: core đã triển khai và kiểm chứng. API integration được thực hiện cùng A01; kết quả HTTP ghi trong báo cáo của gói API. Đây là kiểm chứng cơ chế reliability; không phải chứng nhận năng lực xử lý 1.000 tài liệu/ngày.

## Vấn đề và đường thực thi

Trước thay đổi, upload có deadline riêng nhưng `Store::submit_inner` đợi pool, idempotency row lock, admission advisory lock và COMMIT mà không có enqueue deadline tổng. `Store::connect` có pool acquire timeout 10 giây, nhưng timeout đó không giới hạn một câu SQL hoặc toàn bộ transaction.

Đường mới:

```text
multipart upload deadline (API; riêng)
  → submit_bounded_with_deadlines(NewJob, max_pending, SubmitDeadlines)
  → tokio::time::timeout(total)
    → pool.begin()                         [nằm trong total]
    → SET LOCAL lock_timeout/statement_timeout
    → SELECT existing idempotency key FOR SHARE
    → existing? verify fingerprint → COMMIT → replay
    → pg_advisory_xact_lock(admission)
    → recheck idempotency key
    → pending count → quota check
    → INSERT jobs + atomic outbox trigger
    → COMMIT                              [nằm trong total]
  → typed timeout / unchanged success outcome
```

`crates/ocr-core/src/store.rs` thêm `SubmitDeadlines` có constructor kiểm tra ba budget theo millisecond nguyên dương, nằm trong giới hạn PostgreSQL int32, và `lock < statement < total`. Mặc định: total 10 giây, lock 2 giây, statement 5 giây. Các method cũ `submit` và `submit_bounded` được giữ và dùng mặc định này. Caller muốn budget khác gọi `submit_bounded_with_deadlines`.

Timeout transaction-local dùng `set_config(..., true)` và bind tham số, không sửa session timeout vĩnh viễn. SQLSTATE `55P03` chuyển thành `EnqueueTimeoutKind::Lock`; `57014` thành `Statement`; pool acquire timeout thành `Pool`; deadline ngoài cùng thành `Deadline`. Conflict nội dung/profile, queue quota và lỗi DB khác giữ semantics cũ.

## Trace kiểm chứng

Database test dùng container riêng `ocr-m1-test-postgres`, PostgreSQL 17 Alpine, loopback port 55432, dữ liệu tmpfs; mỗi test tạo schema UUID và xóa schema của chính nó. Không dùng `.env`, không chạm dữ liệu đang chạy.

| Trace | Thiết lập / bước quan sát | Kết quả kiểm tra |
|---|---|---|
| Admission lock | Giữ đúng advisory lock; enqueue lock budget 80 ms; đồng thời renew một lease đang sống | Enqueue trả typed `Lock` trước allowance 2 s; renew thành công; thả lock rồi retry tạo đúng một job mới/outbox |
| Idempotency row lock | Khóa job tồn tại bằng `FOR UPDATE`; request cùng key phải đợi `FOR SHARE` | Typed `Lock`; thả row lock rồi replay đúng job cũ, `created=false`, outbox vẫn một row |
| Pool exhaustion | Giữ cả 10 connections; total 100 ms | Typed `Deadline`; chưa có outbox; thả connections rồi retry thành công |
| Pool acquire timeout | Pool riêng một connection; acquire 40 ms; total 600 ms | Typed `Pool`; connection hồi phục và không tạo outbox |
| Statement timeout | Trigger `BEFORE INSERT` sleep 200 ms; statement 100 ms | Typed `Statement`; transaction rollback cả job/outbox; kiểm tra cả 10 sessions có `lock_timeout=0`, `statement_timeout=0`; bỏ trigger rồi retry tạo một outbox |
| Caller cancellation | Quan sát `pg_stat_activity` theo application name của schema: INSERT đang thực sự `PgSleep`; sau đó abort caller task | Transaction rollback; retry cùng key tạo một job/outbox; replay trả cùng ID; cả 10 connections dùng lại được, không còn idle transaction |
| Deadline khi COMMIT | Trigger INSERT sleep 100 ms, deferred commit trigger sleep 150 ms; total 200 ms; quan sát server đã vào `COMMIT` | Typed `Deadline`; lần chạy đo được **transaction gốc đã commit**. Retry cùng key trả job đã có. Sau retry/replay: jobs=1, outbox=1, 10 connections hồi phục |
| Budget validation | Zero, fractional ms, quá int32, thứ tự bằng/sai | Constructor từ chối; getters cho budget hợp lệ giữ đúng giá trị |
| Stored JSON allocation guard (A01 integration) | `get_bounded` và replay có SQL `CASE` trước SQLx decode; thử NULL, thiếu ID, đúng byte boundary và vượt một byte | Dưới cap giữ nguyên `Job`; quá cap trả `ResponseTooLarge`; fingerprint conflict ưu tiên hơn response cap; không sửa job/outbox |
| API read SQL deadline | `begin_response_read` đặt local lock 2 s/statement 5 s; giữ `LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE`, rồi gọi `get_bounded` | SQL kết thúc bằng `55P03` trước allowance 4 s; bỏ lock thì đọc đúng job; cả 10 connections hồi phục |

Trace COMMIT thật trong test log:

```text
enqueue commit trace: observed_server_commit=true total_deadline=true original_committed=true jobs=1 outbox=1 pool_connections_recovered=10
```

Điểm này có ý nghĩa trực tiếp với caller: **timeout không có nghĩa là chưa lưu**. Caller phải retry cùng Idempotency-Key để đối soát kết quả. Khi caller dùng method legacy không có key, hệ thống không thể đảm bảo dedup một lần gửi mới sau kết quả không rõ. Không tự bật retry nội bộ cho một request có thể đã commit.

## Review cancellation, lock và compatibility

- Deadline ngoài cùng bao trọn pool acquire, BEGIN, mọi SQL và COMMIT. Statement timeout là per-statement; nó không thay thế total deadline khi một request chạy nhiều câu SQL.
- Outer timeout/caller cancellation drop future và transaction. Đã kiểm tra SQLx 0.8.6 hiện có trong workspace: PostgreSQL BEGIN có rollback guard; `Transaction::drop` xếp rollback; đường trả connection về pool flush rollback trước khi tái sử dụng. Đây là cleanup bất đồng bộ, không phải hủy tức thời câu SQL trên server.
- Query đang chạy có thể tiếp tục ngắn hạn sau khi caller nhận timeout. Statement timeout vẫn giới hạn phần SQL còn chạy; tests xác minh connection hồi phục trong 2 giây ở workload test. Không khẳng định một network partition hay một COMMIT đang chờ I/O kernel có hard wall-clock cleanup 2 giây.
- Khi COMMIT đã gửi đi, rollback của dropped future không thể hoàn tác transaction đã commit. Test thực tế đã tái hiện trường hợp này; idempotency key, unique constraint và outbox cùng transaction giữ một job/outbox.
- Admission và fingerprint retry behavior cũ giữ nguyên: replay vẫn làm được khi queue đầy; conflict vẫn lỗi; row lock/fresh snapshot loop vẫn giữ; optional key legacy vẫn chấp nhận.
- Phối hợp A01: thêm `get_bounded` và `submit_bounded_with_deadlines_and_response_limit`. PostgreSQL trả `result=NULL` và flag khi `octet_length(result::text)` vượt budget, nên SQLx không nhận và deserialize JSON quá lớn. Không có preflight/read race. Stored-result cap này bảo thủ, đo PostgreSQL JSON text bytes; wire serializer vẫn có cap riêng. Các methods cũ giữ nguyên giới hạn output trước đây.
- `begin_response_read` là helper crate-local cho các đường đọc phục vụ API: local lock timeout 2 giây, statement timeout 5 giây; `get_bounded` dùng helper và commit sau decode. Các projection API khác có thể dùng cùng helper. API bao ngoài bằng total deadline để tính cả pool wait. Timeout của read là lỗi storage, không bị gắn nhãn enqueue.
- Lease fencing không sửa trong gói này. Toàn bộ regression về clock sau row lock, renew/complete/fail, retry/outbox, claim và retention đã chạy lại.
- Tests khóa advisory chạy sequential vì lock identity ứng dụng cố định, dùng chung giữa các schema trong cùng database. Đây là yêu cầu cô lập test, không phải thay đổi throughput production.

## Lệnh và kết quả

Lệnh dưới đây sử dụng URL test được cấp bằng environment; không dùng credentials production:

```bash
TEST_DATABASE_URL=<disposable-test-db> cargo test -p ocr-core --test store -- --include-ignored --test-threads=1 --nocapture
cargo clippy -p ocr-core --all-targets -- -D warnings
```

Lần kiểm chứng deadline ban đầu: **31 passed, 0 failed** trong 11.85 giây, gồm 30 tests database và 1 test budget thông thường. Sau khi bổ sung guard JSON phối hợp A01, chạy lại toàn bộ store suite: **32 passed, 0 failed** trong 11.98 giây. Sau đó thêm helper read SQL và test giữ exclusive table lock: **1 passed, 0 failed** trong 2.50 giây. Strict core/all-target Clippy đã qua sau khi thêm helper.

Store suite hiện có **33 tests**: 32 tests database (9 mới, 23 regression cũ) và 1 budget test thông thường. Lần chạy gộp 33 tests trên code cuối nằm trong kiểm chứng toàn workspace của root; không dùng số 32 cũ để khẳng định lần chạy gộp mới đã hoàn tất. Log từng bước: `/tmp/ocr-m1-a02-store.log`, `/tmp/ocr-m1-a02-clippy.log`, `/tmp/ocr-m1-a02-result-guard.log`, `/tmp/ocr-m1-a02-read-timeout.log`.

## Giới hạn còn lại

- Chưa thay runtime roles, pool partition, retention, heartbeat budget hoặc migration mode. Các phần đó thuộc milestone riêng.
- Test cancellation và deferred COMMIT dùng PostgreSQL cục bộ; chưa mô phỏng network partition, server crash hoặc replication synchronous commit.
- Budget mặc định là giới hạn tài nguyên ban đầu, chưa được chứng minh tối ưu cho peak traffic. Cần đo tỷ lệ timeout, queue wait và tail latency cùng hardware/inference workload thật.
- Không tự thay concurrency, model hay production configuration; thay đổi này không sửa OCR text/layout và không nâng confidence giả.
