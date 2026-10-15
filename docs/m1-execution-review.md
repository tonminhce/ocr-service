# M1 — Báo cáo thực thi, trace và review

Ngày 15/10/2026. Phạm vi: **P00 + A01 + A02 + B01/B02 tooling + E01**.
Đây là một milestone đã triển khai; toàn bộ roadmap production chưa hoàn tất.
Các kết quả dưới đây là của code mới, không tái dùng test counts từ baseline.

## Kết quả xác minh

| Kiểm chứng | Kết quả |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | **191 passed, 0 failed; 65 ignored được chạy riêng** |
| `cargo test --workspace --locked -- --ignored --test-threads=1 --nocapture` | **65 passed, 0 failed, 0 còn ignored** |
| Evaluator subset trong 191 tests | 25 v2 + 13 existing; không cộng lại vào tổng |
| Docker build | PASS, Linux ARM64; API/worker đã được recreate bằng image mới |
| Compose syntax | `docker compose config --quiet`: PASS; không in expanded config/secrets |
| Qwen thật, synthetic image | PASS exact expected tokens; 1 page; 6 s end-to-end |
| Qwen thật, synthetic PDF | PASS exact page order/tokens; 2 pages; 8 s end-to-end |
| Private development PDF | PASS,4 pages,171.095437 s; Markdown/layout/pages/extraction giữ nguyên |

Log đầy đủ: `/tmp/ocr-m1-{fmt,clippy,unit,integration,docker-build,real-e2e}.log`.
Private/synthetic actual-model artifacts ở ignored `artifacts/m1-verification/`;
thư mục cha quyền700, private result quyền600. Không upload tài liệu ra cloud.

Tests DB/Redis chạy trên container disposable riêng, loopback ports55432/56379/56380,
không dùng credentials hoặc dữ liệu application để fault-test. PostgreSQL test
17 Alpine; actual service E2E chạy PostgreSQL18. Redis maxmemory/OOM regression chỉ
thay server test dành riêng. Testcontainers được dọn sau verification; application
PostgreSQL/Redis volumes không reset hoặc xóa. Không commit/push.

## Trace cơ chế và ý nghĩa thực tế

### Intake, API responses và socket lifetime

`multipart → enqueue total deadline → SQL local budgets → idempotent transaction/
outbox → bounded stored-result read → bounded serializer → admitted body →
bounded HTTP/1 transport`.

Legacy POST vẫn202 + Location + full Job JSON, kể cả replay. Full GET vẫn schema
cũ. New `/status` có small response/CPU reserve riêng. Stored JSON guard được
thực hiện bằng CASE trong cùng SQL snapshot, trước SQLx decode; wire writer có
byte cap riêng và dừng ngay khi serialize vượt giới hạn. CPU/output permits nằm
trong blocking closure khi async waiter bị hủy. Body count/bytes sống tới EOF/drop/
deadline; frame copy16KiB và Hyper/connection/stall/lifetime caps bảo vệ phần dữ
liệu transport còn giữ sau body EOF.

Trace commit quan sát được trong full integration run:

```text
observed_server_commit=true
total_deadline=true
original_committed=true
jobs=1 outbox=1 pool_connections_recovered=10
```

Caller đã nhận deadline nhưng server đã commit. Retry **cùng key** tìm lại đúng
job; không coi timeout là bằng chứng chưa lưu. Tests còn giữ advisory lock,
idempotency row lock, statement sleep, pool exhaustion, caller cancellation,
heartbeat tiến triển và session timeouts không leak.

Real TCP diagnostic của full run:12 vòng, mỗi vòng2 client không đọc body16MiB;
48 large requests bị giới hạn,48 status polls thành công. RSS baseline5248KiB,
held cuối42640KiB; spread sau warmup672KiB. Đây là memory plateau của fixture API,
không phải RSS budget hoặc capacity của toàn OCR deployment. Small polling vẫn
phụ thuộc shared DB pool/TCP listener nên có thể gặp transient failure khi chúng
đầy; deadlines bảo đảm bound/recovery.

Chi tiết code paths, default knobs và từng test:
[A01 response/transport](a01-execution-trace.md), [A02 SQL/deadlines](a02-execution-trace.md).

### Đánh giá fields đúng người, nguồn và vùng ảnh

`score-v2` là opt-in offline contract. Gold IDs độc lập prediction IDs; mapping
one-to-one bằng source geometry được freeze cả case, không rematch mỗi field.
Chấm riêng value/state, grounded field, candidate completeness/extra, role/card/
source/page attribution, all-critical per case, CER và geometry coverage/
extraneous theo granularity thật. NFC giữ dấu, leading zeros, currency/unit/period.

Negative controls đã qua: đúng giá trị sai người/card/page; swap identities;
missing/duplicate conflict candidates; source_blank khác missing; forged hashes/
UTF-8 spans; wrong rotation/CropBox; clipped hoặc oversized geometry; recovery
ambiguity reason sai. Đúng text nhưng sai attribution không được full credit.
Structural integrity được báo riêng với pixel correctness.

**Live runtime v1 không tự biến thành v2.** Nó chưa có machine-only exporter với
logical-document/card IDs và transcript geometry đủ chính xác. Không tạo sidecar
giả từ gold hoặc giả word boxes. V1 scorer/benchmark vẫn như cũ; chưa có live v2
benchmark. Schema/guide/CLI đã chạy trên synthetic fixture, không chứng nhận real
OCR accuracy. Xem [B01/B02 trace](b01-b02-execution-trace.md) và
[annotation guide](annotation-guide.md).

### Telemetry dùng được cho benchmark tiếp theo

Mỗi leased attempt có recorder riêng, cùng shared R/process. Checkpoint hit không
được tính lại thành model call/token usage. Crop dimensions/bytes ghi từ encoded
JPEG thật; usage chỉ lấy thành phần hợp lệ server trả. Null/missing không thành
zero; malformed/overflow/impossible usage thành invalid. Finish reasons dùng enum
cố định; `length` vẫn lỗi. `ttft_seconds=null` vì non-streaming.

Actual Qwen synthetic image:1 attempt,189 prompt/7 completion tokens. Two-page
PDF:2 attempts,420 prompt/14 completion tokens. Usage hiện diện ở cả3 server replies,
W=1/shared R=1 giữ nguyên. Đây là token accounting thực, không confidence score.
Runtime declaration vẫn unverified, verified server configuration null.
[E01 trace](e01-execution-trace.md), [observability contract](observability.md).

## Review chéo đã phát hiện và sửa

| Finding | Sửa và bằng chứng |
|---|---|
| Capped writer chưa ngăn SQLx allocate oversized stored JSON trước đó | Same-statement SQL CASE guards cho Job/replay, source/artifacts, extraction/business/review; boundary + HTTP tests |
| Body EOF không bảo đảm socket đã gửi xong; Bytes slice có thể giữ cả16MiB | Copy16KiB frames, Hyper bounded buffers và TCP lifecycle; no-read/disconnect/stall tests + RSS diagnostic |
| Chỉ timeout request chưa kiểm soát SQL/cancel/COMMIT ambiguity | Local budgets + total deadline; observed commit/same-key recovery trace |
| Partial usage component có thể lớn hơn total | Reject lower-bound inconsistency; negative input cases và isolated metric denominator/saturation test |
| Nhánh mất lease trả về trước per-attempt trace | Drop processing scope → snapshot/log → LeaseLost return, không SQL mutation; lifecycle regressions qua |
| Manifest gọi full-page là course-greedy dù overrides chỉ có trên region task | Ghi exact common parameters + scoped region override values; HTTP request parity tests |
| Ambiguity reason khác nhau có thể được chấm exact; recovery-only nhiều candidates có thể chứa first-pass | So reason trong semantic/grounded exact, mọi recovery-only observation phải recovery;2 negative controls mới qua |

Root review và hai lượt peer review kiểm cancellation, SQL locks/idempotency,
evidence/entity matching, memory bounds và compatibility. Sau fixes không còn
finding correctness/security quan trọng ở phạm vi M1 đã review. Formatting/Clippy
ban đầu chỉ ra style lints ở code mới; đã sửa và full checks cuối qua. Không coi
review này là audit toàn bộ các milestone chưa triển khai.

## Private PDF regression

Đã thực thi trên API/worker image mới và native Qwen hiện có:

| Đo lường | Kết quả |
|---|---|
| Kết thúc | Succeeded,1 attempt,4 pages,171.095437 s từ submit tới terminal |
| Inference | 90/90 thành công,90 stop;0 failed/cancelled/truncated;90 checkpoints |
| Genuine token usage | 90/90 replies;20.029 prompt +2.712 completion =22.741 total tokens |
| Raw parity với baseline development | Markdown,layout,pages,extraction đều bằng nhau; source digest bằng nhau |
| Confidence | 411 properties trong full result đều null; không gán score giả |
| Worker memory peak | 1.876.815.872 bytes ≈1,75GiB,87,4% cap2GiB; chưa đạt75–80% headroom gate |
| HTTP thật | Status298B, business42.390B, layout43.027B, source637.111B; cả4 trả200/no-store/nosniff |
| Source artifact | SHA-256 khớp exact upload bytes |
| Business | Ba tenant fields vẫn conflict, mỗi field2 candidates, không selected; landlord DOB unreadable/không selected; needs_review |
| Protected worker metrics |93 actual attempts của3 jobs; mọi token count và denominator bằng tổng persisted snapshots |

Tổng tokens từ3 actual-model jobs trong exporter:20.638 prompt/2.733 completion/
23.371 total;93 replies có genuine usage. `ttft_seconds` và verified server config
tiếp tục null. Deployment đang đặt `RUST_LOG=warn`, nên INFO per-attempt log bị lọc;
không khẳng định đã nhìn thấy log đó. Persisted snapshots và protected metrics đã
được đối chiếu thực tế. Giữ log level hiện tại.

171,10 s so với178,43 s ở run cũ là hai observation warm-run, không phải proof
speedup. Chưa chạy representative capacity test hoặc concurrency sweep.
OCR text, extracted values và source PDF giữ ở ignored artifacts quyền hạn chế;
không dùng development PDF này làm gold độc lập hoặc calibration.

Máy-readable aggregate không có nội dung tài liệu:
[M1 verification record](research/m1-verification-2026-10-15.json).

## Những gate còn mở

- **WAITING_FOR_DATA:**10–20 independently labelled development docs để dry-run
  annotation, tiếp đến pilot200–300. Critical fields cần double annotation và
  adjudication; không tự lấy OCR/model output làm gold.
- **PLANNED:** live v2 observation/export adapter + true geometry; DB runtime roles/
  migrator, secret files, retention/holds/restore, parser isolation và reviewer
  identity/workflow.
- **CHƯA ĐO:** E02 memory trước concurrency; E03 open-loop runner; E04 serial load/
  fault/soak và Mac/GPU decision. Chưa cam kết >1.000 tài liệu/ngày hoặc deadline.
- Automatic acceptance vẫn tắt; confidence chưa calibration vẫn null. Không nâng
  score hoặc âm thầm chọn nguồn thắng khi contract/card có conflict.
- Không bật retention destructive policy, không cài/đổi model, không tăng W/R,
  không thêm Kubernetes hoặc API gateway trong M1.

M1 cung cấp resource bounds và thước đo đáng tin cho bước tiếp theo. Nó không tự
sửa mọi lỗi OCR/mapping trong tài liệu: lựa chọn engine/routing và field policies
cần independent labels và quality/capacity gates đã đặt trong kế hoạch.
