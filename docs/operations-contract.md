# M0/P00 — Contract và baseline thực thi

Ngày 15/10/2026. Đây là contract của đợt M1; acceptance về accuracy/capacity và
deployment bên ngoài chưa được xác nhận. Xem [kế hoạch](production-execution-plan.md)
và [trace thực thi](m1-execution-review.md).

## Những hành vi được giữ

| Đường sử dụng | Contract |
|---|---|
| Submit mới | Multipart → durable PostgreSQL transaction + outbox; 202 + Location + full Job JSON |
| Submit cùng Idempotency-Key | Cùng source/media type/profile trả job cũ; fingerprint khác trả conflict; giữ tên file đầu tiên |
| Key legacy bỏ trống | Tiếp tục được chấp nhận; gửi lại sau uncertain outcome không có dedup guarantee |
| Full GET job | Giữ full Job JSON và status; giới hạn resource có thể từ chối response quá lớn |
| `/status` | Projection nhỏ; không chứa full result, có admission/CPU reserve riêng |
| `/business`, review, artifacts | Xác thực; raw OCR/source không bị correction ghi đè; cache no-store và nosniff |
| Queue transport | Redis Streams notification/replay; PostgreSQL outbox và lease là nguồn trạng thái bền vững |
| Claim/renew/checkpoint/complete/fail | Lease token + fresh clock sau khi lấy row lock; stale attempt không được ghi đè |
| Course profile | Detect layout → crop → task recognition → Markdown + layout JSON; Rust consumer |
| Extraction | Evidence, candidates, source/role/conflict; confidence chưa calibration giữ null; không autoaccept |

HTTP upload timeout riêng enqueue timeout. Enqueue total bao gồm pool acquire,
SQL và COMMIT; transaction-local lock/statement limits nhỏ hơn total. HTTP 503
`enqueue_timeout` có Retry-After: retry cùng key, vì timeout có thể xảy ra sau
server commit. Response quá lớn không làm thay đổi job đã lưu.

API transport có connection/header/write-stall/lifetime limits; một response lớn
không giữ vô hạn slot hoặc userland buffer khi TCP client không đọc. Những giới
hạn này không thay PostgreSQL/query, host memory hay reverse-proxy sizing.

## Fingerprints và dữ liệu bất biến

- Source digest là SHA-256 của exact upload bytes, không hash OCR text.
- Detector artifact có SHA-256 đã pin; result có strategy và layout profile.
- `execution.identity` thêm client prompt/preprocessing hashes và requested profile;
  `execution.inference_telemetry` ghi server usage thật nếu có. Không sửa raw text/layout.
- Model alias và optional runtime/weights declaration chưa phải attestation. Các
  giá trị không đo được giữ null; không dựng token count, TTFT hoặc confidence.
- Version schemas extraction/review hiện tại được giữ. Evaluation v2 là contract
  riêng, opt-in; v1 scorer không được đổi nghĩa dữ liệu cũ.
- Gold độc lập phải đọc pixels và giữ observation contract/card riêng. Structural
  hash/span integrity khác với đúng nội dung ảnh; conflict thật không được xóa.

## Baseline trước M1 (đã đo ở lượt trước)

PDF development bốn trang: 178.431643958 s, 86 first-pass + 4 recovery model calls,
worker peak 1,880,678,400 bytes trên cap 2 GiB. W=1, R=1 dùng chung, layout pool=1;
native vLLM Metal 0.31.0, Qwen3.5-4B 4bit. Ba tenant contract/card conflicts và
landlord DOB trống vẫn không selected; confidence null. Báo cáo gốc:
[production-pass-validation](production-pass-validation.md).

Các số này không phải kết quả mới của M1, không đại diện corpus và không chứng
minh >1.000 tài liệu/ngày. PDF đã dùng sửa rules là development regression, không
được dùng làm frozen quality test. Private source/results ở ignored artifacts,
không đưa vào tracked fixture hoặc gửi lên cloud.

## Gate chưa được thông qua

Representative independent labels, calibration, runtime DB roles, retention/holds,
restore reconciliation, parser isolation, reviewer identity, capacity/open-loop
load và release hardening còn theo milestone riêng. Hiện không được tuyên bố
toàn dịch vụ production-ready hay bật automatic acceptance.

Giới hạn runtime chính được mô tả trong `.env.example` và
[observability](observability.md); HTTP resource budgets và exact tests được ghi
trong [A01](a01-execution-trace.md), SQL deadlines trong [A02](a02-execution-trace.md).
