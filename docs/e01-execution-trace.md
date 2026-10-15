# E01 — Telemetry thật và identity của từng lần chạy

Trạng thái: triển khai trong M1. Kết quả full verification và Docker/actual-model
được tổng hợp ở [M1 review](m1-execution-review.md). Không thay engine, prompt,
sampling requests, crop pixels, formatter, raw OCR hoặc field confidence.

## Vấn đề → code path → thay đổi

Trước đây chỉ có stage timings, model_failed và cache hit/miss. Không biết mỗi
attempt đã gửi bao nhiêu crop pixels/bytes, server có trả token usage không,
request bị truncate hay bị hủy; deployment fingerprint là chuỗi operator tự khai.

```text
for_job → recorder mới theo leased attempt (R vẫn dùng chung)
  → checkpoint lookup
    → hit: ghi cache hit, không thêm model attempt/token/finish
    → miss: ghi cache miss
  → đợi semaphore R                    [model_queue]
  → CallTrace::start                   [attempt + crop header/bytes]
  → send exact unchanged request       [model]
  → bounded non-streaming reply <=2MiB
  → parse envelope
  → finish reason enum + optional validated server usage
  → validate stop/content → success hoặc error
  → dropped HTTP future → cancelled, trả local R permit
  → fenced completion: execution.identity + inference_telemetry
```

`crates/ocr-worker/src/telemetry.rs` dùng summaries/arrays cố định; không tích lũy
danh sách crops hoặc OCR text. Count/sum u64 dùng saturating arithmetic.
JPEG dimensions đọc từ header của encoded crop, không decode thêm ảnh. Base64
payload bytes lấy exact encoded-length formula của bytes có sẵn; không gọi nó
là total HTTP wire bytes hay server tensor pixels.

`ParsedUsage` giữ prompt/completion/total riêng; có mẫu zero khác với không có
mẫu. Missing/null/empty usage là unavailable. Count âm, fraction, string, overflow,
component lớn hơn total hoặc tổng ba thành phần mâu thuẫn là invalid. Không tính
ngược phần thiếu và không suy đoán tokens từ ký tự. Reply truncate vẫn ghi usage
thật đã tiêu tốn rồi trả lỗi; không lưu partial OCR như thành công.

`ModelFinish` cố định stop/length/content_filter/tool_calls/function_call/other/missing.
Unknown reason không đi vào metric labels/log error. Finish và usage được ghi khi
parse được envelope; transport/JSON malformed errors không được giả làm missing
usage replies. Non-streaming không có TTFT; giá trị đó luôn null.

## Concurrency và cancellation review

- Một `region_slots: Arc<Semaphore>` dùng chung qua clones và `for_job`. W document
  workers không nhân R; N processes có tối đa N×R local inflight futures.
- Recorder được reset cho mỗi lease; bố mẹ không cộng nhầm vào job con. Local R
  mới bắt đầu attempt sau acquire. Hủy task đang chờ R không tăng model call.
- Guard Drop phân biệt cancelled với failed. Hủy future trả local permit; không
  có bằng chứng remote server đã dừng GPU work và không tuyên bố như vậy.
- Heartbeat vẫn chạy đồng thời với processing. Nhánh maintenance failure giờ đi
  qua cùng exit scope: drop HTTP, chốt trace rồi finish LeaseLost; không cố ghi SQL
  bằng lease không chắc còn sống. Không thay lease clock/fencing.
- Cache hit không tính lại usage/finish. Declaration khác phải miss; khi unset,
  fingerprint giữ nguyên so với baseline, không vô tình mất checkpoint đang có.
- Khi INFO logging bật, worker phát bounded per-attempt trace ở đường thành công,
  failure, deadline và mất lease. `RUST_LOG=warn` lọc event này; persisted snapshot
  và protected metrics vẫn hoạt động. Abrupt process death có thể mất telemetry/
  log chưa flush.

## Identity và compatibility

`Recognizer::execution_manifest` giữ các execution keys cũ và thêm identity của
actual client prompt/preprocessing/profile/model alias. Sampling ghi common
parameters riêng và top_p/top_k/repetition_penalty chỉ áp dụng region tasks; không
gọi full-page là course greedy. `INFERENCE_DEPLOYMENT_JSON` là optional, strict,
bounded declaration cho runtime/revision/weights/quantization/server limits.
Validate lại cả khi Settings được tạo trực tiếp trong Rust.

Declaration được ghi `operator_declared_unverified`; verified server configuration
null. `/v1/models` chỉ quảng bá alias. Xem [observability](observability.md) để cấu
hình; không đưa API key/URL/PII vào declaration hoặc labels.

Raw `recognize` output không thêm telemetry; worker thêm metadata cạnh raw vào
execution trước persist. Existing raw Markdown/layout và field confidence không
bị thay đổi. V1 scorer và evidence hashes không lấy telemetry làm ground truth.

## Trace kiểm thử mục tiêu

| Kiểm tra | Kết quả quan sát |
|---|---|
| Genuine usage + 8×4 image | 1 started/1 succeeded; 17 prompt +3 completion/20 total; crop 8×4/32 pixels; exact raw Vietnamese/leading zeros giữ nguyên |
| Missing/null/partial/zero usage | Missing không có token observation; partial không dựng total; zero có observation; invalid usage không làm sai raw transcription |
| Invalid/overflow/impossible usage | Âm/fraction/string/u64 overflow/tổng sai/partial component>total đều invalid, không có token samples |
| Unknown/length/content_filter finish | reason cố định; tất cả từ chối success; usage thật của truncated reply vẫn được ghi |
| Queued + started cancellation | Hủy queued không có started/cancelled call; hủy running có 1 cancelled; request kế tiếp dùng lại local R thành công |
| Six job clones, R=2 | Model fixture peak=2; mỗi job có exactly1 attempt; parent recorder0 |
| Checkpoint replay | 2 identical transcribe calls chỉ có1 model call/usage reply,1 cache hit/1 miss |
| Metrics overflow | Sum saturates u64::MAX; mỗi token kind có denominator riêng; invalid usage không cộng count được đưa vào giả |
| Bad declaration | Unknown fields/unsafe string/oversize/invalid limit/hash bị từ chối; error không echo input |

Targeted lần chạy sau fixes: inference-contract **18 passed,1 ignored** (Poppler);
retry-efficiency **5 passed,2 ignored** (DB). Hai DB checkpoint tests chạy riêng
trên PostgreSQL disposable: **2 passed**. Logs:
`/tmp/ocr-m1-e01-targeted.log`, `/tmp/ocr-m1-e01-checkpoints.log`.
Full workspace run bên M1 bao gồm các ignored tests trong môi trường thích hợp.

## Giới hạn thật

Telemetry cho biết workload/cost/time và giúp chọn experiment; chưa chứng minh
accuracy hay capacity. Server metadata chỉ đáng tin trong trust boundary của
deployment; JSON declaration không attestation. Chưa thu TTFT/streaming/server
queue/GPU memory, chưa chạy W/R sweep. Persisted snapshot chỉ chứa attempt thành
công cuối cùng cùng cache accounting; các attempts lỗi trước cần protected logs
và process metrics. Không dùng phần này để tự tăng confidence hoặc bật autoaccept.
