# A01 — Bounded responses, storage reads và TCP backpressure

Trạng thái: **IMPLEMENTED / VERIFIED LOCAL** ngày 2026-10-15. Phạm vi là API resource controls và contract; đây chưa phải chứng nhận throughput trên 1.000 tài liệu/ngày. Không thay OCR engine, model, extraction rules, confidence, raw Markdown/layout hoặc concurrency inference.

## Trace từ lỗi đến kết quả

| Trace | Trước thay đổi | Đường thực thi sau thay đổi | Bằng chứng |
|---|---|---|---|
| A01-01 | Legacy GET và POST replay trả `Json<Job>` trực tiếp, thiếu admission/CPU/wire cap chung | `routes/jobs.rs` → `ResponseBudget::json` trước đọc DB → guarded Store query → `Admission::json` → bounded writer → admitted body | HTTP DB test so JSON đầy đủ với `serde_json::to_value(Store::get)`; POST vẫn202, Location và replay job/result giữ nguyên |
| A01-02 | Serializer cấp phát toàn bộ Vec trước khi kiểm tra kích thước | `BoundedWriter::write` từ chối lần ghi vượt cap; tăng capacity theo cấp số nhân nhưng chặn ở cap | Serializer synthetic dừng trước100 phần tử thay vì100.000; capacity tại boundary1700 bytes không vượt1700 |
| A01-03 | Hủy async waiter có thể nhả resource trong khi closure vẫn chạy | CPU permit và output admission được move vào `spawn_blocking` closure, sau đó output admission đi vào body | Test dùng serializer bị chặn thật: abort waiter vẫn giữ CPU và byte reservation; request sau không chạy serializer song song; nhả gate trả đủ permits |
| A01-04 | Permit đọc kết thúc khi handler trả response; slow reader còn giữ dữ liệu | Body giữ count/byte permits đến EOF/drop/deadline. Frame copy tối đa16KiB tránh một frame cuối giữ nguyên allocation16MiB bằng shared Bytes slice | Real TCP:2 client chỉ đọc headers rồi dừng, request lớn tiếp theo429,6 lượt status vẫn200; disconnect trả capacity |
| A01-05 | Body EOF không chứng minh Hyper/socket đã gửi hết | HTTP/1-only server: connection cap,64KiB read/write-buffer cap, flatten writes, không pipeline aggregation; copied frame tail tối đa thêm16KiB; write/flush stall timers và absolute connection lifetime | Real TCP write-stall150ms giải phóng2 body slots; connection cap2 đóng socket thứ3 chưa parse; header deadline150ms giải phóng partial requests |
| A01-06 | Stored JSON lớn vẫn có thể được SQLx decode trước khi writer chặn | SQL `CASE` trong cùng statement snapshot kiểm tra `octet_length(result::text)` hoặc artifact/extraction trước chuyển JSON qua SQLx; source dùng exact `octet_length(input)` | Core boundary test: exact boundary qua, boundary−1 bị chặn, source12/13-byte boundary, null/missing không đổi. HTTP oversized GET/replay/artifacts/business/review500 ổn định |
| A01-07 | Đọc DB có thể chờ quá lâu ở pool/lock/query | API total read operation deadline10s; read transaction local lock2s/statement5s. Review history và append cũng có local limits; enqueue có policy A02 riêng | HTTP giữ toàn bộ pool: GET/status/business/artifact503 `operation_timeout`, sau trả pool request404 và slots phục hồi; local settings không leak vào pool |
| A01-08 | Worst-case source reservation50MiB có thể giữ suốt download637KiB | Reserve maximum trước fetch/serialize, trả phần byte permit dư sau khi biết **backing capacity**, giữ body-count permit riêng | Test source100-byte trả phần reservation dư,2 bodies vẫn chạm count cap; Vec len nhỏ nhưng hidden capacity vượt limit bị từ chối |
| A01-09 | Refactor review có thể làm sai idempotency | Guard extraction không decode oversized data; kiểm tra review request_id/fingerprint trước lỗi oversized cho replay không cần extraction | Core test replay cùng request_id với read cap nhỏ hơn vẫn revision1; đổi payload cùng request_id vẫn conflict; new oversized review không tạo event |

## Budgets mặc định

| Resource | Default / cấu hình |
|---|---|
| Upload handlers | `UPLOAD_CONCURRENCY=8`; upload bytes/deadline giữ contract cũ |
| Compact responses | `RESPONSE_SMALL_CONCURRENCY=32`, `RESPONSE_MAX_SMALL_BYTES=65536`; CPU serialization riêng2 slots |
| Large response handlers/bodies | `RESPONSE_LARGE_CONCURRENCY=8` |
| Large serialization CPU | `RESPONSE_SERIALIZATION_CONCURRENCY=4`; hàng đợi chờ bị chặn bởi response count/byte admission đã có |
| JSON wire và stored JSON text guard | `RESPONSE_MAX_JSON_BYTES=16777216`; hai phép kiểm tra độc lập |
| Source bytes | `RESPONSE_MAX_SOURCE_BYTES=52428800` |
| Large output reservations | `RESPONSE_LARGE_RESERVED_BYTES=134217728`; reserve worst case trước đọc, thu nhỏ theo Vec capacity khi chuyển sang egress |
| DB/projection total read operation | `RESPONSE_READ_TIMEOUT_SECONDS=10`; bao gồm pool acquire. SQL local lock2s/statement5s bảo vệ backend riêng |
| Response body lifetime | `RESPONSE_BODY_TIMEOUT_SECONDS=30`; bắt đầu khi tạo body |
| Active TCP connections | `HTTP_MAX_CONNECTIONS=128` |
| Hyper read/write buffer | `HTTP_WRITE_BUFFER_BYTES=65536`; write queue có thể vượt ngưỡng thêm một frame16KiB |
| Header deadline | `HTTP_HEADER_TIMEOUT_SECONDS=10` |
| Pending write / flush deadline | `HTTP_WRITE_STALL_TIMEOUT_SECONDS=10`; timer flush riêng, không reset write timer khi chưa ghi được byte |
| Absolute connection lifetime | `HTTP_CONNECTION_LIFETIME_SECONDS=120`; bao gồm keepalive và đóng cả slow trickle tránh stall timer |

Các headers `Cache-Control: private, no-store` và `X-Content-Type-Options: nosniff` áp dụng cả legacy routes và errors. 429 và503 có `Retry-After: 1`. Lỗi wire/stored allocation limit trả500 `response_limit_exceeded`; overload sau HTTP parsing trả429 `response_capacity_exceeded`. Khi TCP connection cap đầy, socket mới đóng trước parsing: caller có thể gặp connection error và cần backoff. HTTP/2/h2c không được cung cấp trong server hiện tại; downstream proxy cần dùng HTTP/1.1 đến API.

## Kiểm thử được thực thi

- `cargo test -p ocr-api -- --nocapture`: **18 ordinary tests** =10 library +5 HTTP không cần DB +3 real TCP.6 HTTP DB tests và1 RSS diagnostic được chạy tường minh bên dưới, không coi ignored là pass.
- `TEST_DATABASE_URL=... cargo test -p ocr-api --test http -- --ignored --test-threads=1 --nocapture`: **6 DB HTTP tests pass**, gồm3 test mới cho compatibility/limits, enqueue lock timeout và pool-acquire read deadline.
- `TEST_DATABASE_URL=... cargo test -p ocr-core --test response_limits -- --ignored --nocapture`: **1 core projection/review boundary test pass**.
- `cargo test -p ocr-api --test transport tcp_backpressure_rss_diagnostic -- --ignored --nocapture --test-threads=1`: **1 explicit RSS diagnostic pass**;12 cycles,2 unread16MiB bodies/cycle,48 rejected large reads và48 successful small polls. Lần chạy riêng cho A01: baseline5248KiB; held41408→42704KiB; steady tail spread640KiB sau4 cycles warmup. Full workspace run sau đó có held cuối42640KiB/spread672KiB, được ghi trong [M1 review](m1-execution-review.md). Toàn bộ samples của lần chạy riêng nằm trong log bên dưới.
- `cargo clippy -p ocr-api --all-targets -- -D warnings`: pass.
- OpenAPI YAML parse:8 paths hợp lệ về cú pháp YAML. Đây chưa phải full OpenAPI semantic validator.

Logs của lần verification hiện tại: `/tmp/ocr-a01-api-unit.log`, `/tmp/ocr-a01-http-integration.log`, `/tmp/ocr-a01-core-projection.log`, `/tmp/ocr-a01-transport.log`, `/tmp/ocr-a01-rss.log`, `/tmp/ocr-a01-clippy.log`. Không ghi PDF hay dữ liệu cá nhân vào các logs này. Các tests dùng synthetic documents và PostgreSQL disposable; không chạy trên DB chứa tài liệu người dùng.

## Giới hạn cần đọc đúng

128MiB là budget reservation cho output backing allocations, **không phải giới hạn RSS toàn process**. SQLx/serde decoded Values, extraction projection, upload buffers, Tokio tasks, allocator caches, Hyper read/write buffers và kernel TCP buffers có overhead riêng. Stored text cap ngăn blob tùy ý lớn đi qua SQLx nhưng không biến JSON text size thành RAM size chính xác; rất nhiều phần tử nhỏ có expansion factor khác một string dài. Business projection có bounded CPU pool riêng và giữ CPU permit đến khi closure kết thúc khi request bị hủy.

RSS diagnostic chỉ đo fixture transport trong một process trên máy hiện tại. Nó kiểm tra plateau khi lặp cùng workload, chưa thay thế benchmark Docker process/cgroup dưới corpus representative và burst thực. Không ngoại suy kết quả này sang throughput OCR, độ chính xác field hay toàn bộ deployment memory.

Small polling có response/CPU budget độc lập với large body downloads. Nó vẫn dùng PostgreSQL pool và TCP listener chung; full pool/connection exhaustion có thể gây503/connection errors đến khi deadlines trả tài nguyên. Đây là bound và recovery được kiểm thử, không phải bảo đảm availability tuyệt đối trước một client độc hại chiếm toàn listener.

JSON stored guard dùng uncompressed PostgreSQL `::text`; không dùng `pg_column_size` hoặc compressed TOAST size thay cho wire length. Cách trình bày/escape của PostgreSQL khác serde, nên stored guard và wire guard đều cần. Markdown tải ra là string bytes nhưng storage guard nhìn JSON string representation, có thể chặn sớm hơn final text size. Error/status/filename fields vẫn dựa trên invariant write-side hiện có; đây không phải general-purpose phòng vệ trước DBA tự sửa bất kỳ column thành dữ liệu vô hạn.

Không thay raw stored results khi quá limit; reviewer có thể tăng policy phù hợp sau đo memory hoặc tải artifact riêng. Upload/enqueue/review cancellation không chứng minh mutation chưa commit: retry cùng Idempotency-Key hoặc review request_id, không tạo key mới để thử lại.
