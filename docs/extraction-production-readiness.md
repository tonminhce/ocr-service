> Implementation update, 15 October 2026: the compact business/review API, stage
> metrics, warm detector, fenced checkpoints/recovery and Rust evaluation tools
> proposed in this audit are now implemented. See [delivery and actual validation](production-pass-validation.md).
> This document retains the earlier diagnostic and research; its observations
> about missing features describe that earlier baseline.

# Structured extraction: production readiness and performance

Nghiên cứu và audit ngày **15/10/2026**. Phạm vi: implementation Rust hiện có, Docker, Redis Streams, PostgreSQL và Qwen3.5-4B qua vLLM Metal. Đây là báo cáo và đề xuất; các thay đổi bên dưới chưa được triển khai.

## Kết luận

**Hiện tại là baseline KIE có kiểm chứng nguồn và yêu cầu review; chưa đủ bằng chứng để tự động chấp nhận dữ liệu nghiệp vụ trong production.** Hạ tầng job đã có lease, fencing, outbox, retry và giới hạn tài nguyên. Những cơ chế đó giúp thực thi đáng tin cậy, nhưng không chứng minh một CCCD, ngày sinh hoặc số tiền đã được đọc và gán đúng.

Không cần đổi sang Python consumer, bỏ Redis/PostgreSQL hay thêm Kubernetes. Các phần cần bổ sung phù hợp với API và worker Rust hiện có.

| Khía cạnh | Bằng chứng hiện có | Khoảng trống |
| --- | --- | --- |
| Thực thi job | PostgreSQL/outbox, Redis Streams, heartbeat, fencing, idempotency | Chưa có benchmark tải dài và số đo SLO |
| Pipeline tài liệu | Rust layout → crop → regional recognition → Markdown/layout JSON | Chưa có chính sách recovery cho vùng bị bỏ sót hoặc đọc sai |
| KIE | Rule theo anchor, role scope, normalization, conflicts | Chỉ profile hợp đồng thuê hỗ trợ; chưa đo trên tập độc lập |
| Evidence | Hash, UTF-8 spans, source/page/geometry validation | Quote khớp OCR chưa chứng minh OCR khớp ảnh |
| Confidence | Phân biệt detector score; KIE để null | Chưa có xác suất field được hiệu chuẩn |
| Review | Quyết định `needs_review` | Chưa có correction/approval lưu bền và review UI |
| API output | Audit JSON đầy đủ và versioned schema | Chưa có business projection gọn hoặc artifact API |
| Performance | Concurrency có giới hạn, CPU tách khỏi async executor | Chưa đo từng stage; detector reload mỗi trang; chưa checkpoint region |

## 1. Confidence hiện tại thực sự là gì?

Audit output thực tế trong `artifacts/user-pdf/` cho thấy:

- 266 thuộc tính `confidence` trong extraction đều là **null**, không phải 0 hoặc score thấp.
- 92 vùng layout có score trung vị **0,8417**; 10 vùng dưới 0,65. Tám trong mười vùng đó là vùng image trên trang 4. Chỉ có 86 vùng được gửi nhận dạng.
- 20 business fields tổng hợp gồm 12 `found`, 3 `unreadable`, 2 `missing`, 3 `conflict`. Các field riêng theo contract/identity và các bản evidence không được đếm thêm vào 20 field này.
- `found` nghĩa là rule đã chọn một giá trị từ candidates, **không phải được nghiệp vụ phê duyệt**. Tất cả kết quả vẫn là `needs_review`.

`score` của detector trả lời câu hỏi về việc phát hiện/phân loại vùng. Nó không trả lời xác suất đọc đúng từng chữ hay xác suất field thuộc đúng người. Code ghi rõ `layout_detection_not_ocr_confidence` trong [rules.rs](../crates/ocr-extraction/src/rules.rs). Schema hiện còn buộc confidence bằng null; thêm numeric confidence phải thay đổi hợp đồng và policy có chủ đích.

Microsoft phân biệt confidence của document type, field và word transcription; cần xét cả OCR và extraction. Không phải mọi field đều có score. [Microsoft accuracy and confidence](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/concept/accuracy-confidence?view=doc-intel-4.0.0).

Đề xuất cho service này:

| Tín hiệu | Ý nghĩa | Cách dùng |
| --- | --- | --- |
| `layout_detection_score` | Score vùng từ detector | Cảnh báo coverage/crop; ngưỡng đánh giá theo label |
| `recognition_score` | Tín hiệu của recognizer, nếu có | Giữ loại score, model, thang đo; không đổi tên thành field probability |
| `source_agreement` | Native/OCR hoặc các quan sát có đồng thuận không | Một feature; các nguồn có thể cùng sai |
| `validation_results` | Ngày parse được, currency rõ, role có anchor, có conflict | Hard gates và reason codes |
| `field_correctness_probability` | Xác suất đúng field đã được hiệu chuẩn | Chỉ công bố khi có calibration record và test độc lập |
| `decision` | Có được phép dùng tự động không | Policy riêng theo mức quan trọng của field |

Không dùng trung bình detector score và token probabilities; không yêu cầu Qwen tự ghi `0.98` rồi coi đó là độ chính xác. Token logprob, nếu backend hỗ trợ, chỉ là một tín hiệu chưa hiệu chuẩn. Hai lần gọi cùng một model có thể lặp lại cùng lỗi.

Calibration làm score phù hợp hơn với tần suất đúng quan sát được; nó không tự sửa OCR. Nghiên cứu gốc về temperature scaling cũng phân biệt dự đoán và hiệu chuẩn xác suất. [Guo et al., 2017](https://proceedings.mlr.press/v70/guo17a.html).

Với rule KIE hiện tại, nên bắt đầu bằng policy minh bạch và reason codes. Khi đủ dữ liệu, fit mô hình correctness/calibrator offline, đóng gói versioned artifact để Rust load. Không cần Python consumer ở runtime. Nếu chưa đủ mẫu cho một nhóm tài liệu, tiếp tục để confidence null và yêu cầu review.

## 2. JSON có điểm chưa tốt nào?

Extraction hiện dài **178.450 bytes**, full result **762.303 bytes**. Có 126 lần nhúng evidence nhưng chỉ 63 spans khác nhau. Contract, identity và combined fields lặp candidates/evidence; raw OCR, comparison và projection còn lặp block text. Đây là chi phí của contract audit hiện tại, không phải bằng chứng model hallucinate.

Hai điểm cần sửa trực tiếp:

1. **Response cho consumer quá nặng.** `GET /v1/ocr/jobs/{id}` trả toàn bộ Job và result, theo [jobs.rs](../crates/ocr-api/src/routes/jobs.rs). Poll trạng thái không cần tải lại audit JSON.
2. **`unreadable` gộp nhiều nguyên nhân.** Trong [resolve](../crates/ocr-extraction/src/rules.rs), có observation nhưng không có candidate đều thành `unreadable`. Field có anchor nhưng nguồn để trống, OCR không rõ và parser chưa hỗ trợ format cần reason khác nhau. Chỉ xác nhận `source_blank` khi đã có bằng chứng phù hợp; không suy ra từ việc regex không match.

Đề xuất contract mới, triển khai additive để không phá API hiện tại:

- Endpoint trạng thái gọn, không nhúng result; extraction endpoint trả business fields.
- Mỗi field có `state`, `value`, `reason_codes`, `evidence_refs`; mỗi candidate chuẩn hóa duy nhất trỏ đến nhiều evidence refs. Chỉ gộp khi cùng giá trị và cùng scope/role, không gộp contract và card thành một nguồn.
- Evidence/source text lưu một lần với ID ổn định, hash và tọa độ. Raw Markdown, layout và diagnostics lấy riêng khi cần.
- Giữ `conflict` với value null và nhiều candidates khác nhau. Không chọn giá trị chỉ vì có score lớn hơn.
- Phân biệt execution state, field state và business review state. Một job `succeeded` vẫn có thể chưa đủ điều kiện nghiệp vụ.
- Correction tạo revision mới kèm actor/time/reason/source, không ghi đè raw OCR.

Ví dụ **đề xuất**, dùng dữ liệu giả; chưa phải response hiện có:

```json
{
  "schema_version": "business-extraction/proposed-v2",
  "execution_status": "succeeded",
  "review_status": "needs_review",
  "fields": {
    "tenant.birth_date": {
      "state": "conflict",
      "value": null,
      "confidence": null,
      "reason_codes": ["contract_identity_disagreement"],
      "candidates": [
        {"value": "2000-01-02", "origin": "contract", "evidence_refs": ["ev-1"]},
        {"value": "2000-02-01", "origin": "identity_card", "evidence_refs": ["ev-2"]}
      ]
    }
  }
}
```

Tọa độ whole-block phải tiếp tục ghi `geometry_scope=source_block`; không hiển thị như box chính xác của từng chữ. Offset và reference phải khớp snapshot text bất biến. Microsoft cũng dùng text spans và spatial grounding để truy ngược field về tài liệu, và lưu ý cải thiện extraction examples không chữa được lỗi transcription. [Microsoft analyzer improvement](https://learn.microsoft.com/en-us/azure/ai-services/content-understanding/document/analyzer-improvement).

## 3. Chạy đa luồng chưa?

**Đã có.** Cần phân biệt CPU threads, async requests và model batching:

| Stage | Implementation hiện tại | Giới hạn |
| --- | --- | --- |
| Async runtime | Tokio multithread | Crop/KIE dùng blocking tasks; một phần parsing, formatting và serialization còn inline |
| Documents | `JoinSet` trong worker | `WORKER_CONCURRENCY` |
| Region requests | `buffer_unordered`, semaphore dùng chung giữa jobs | `REGION_CONCURRENCY` trong một worker process |
| Crop/encoding | `spawn_blocking` | Decode permits |
| Post-processing/KIE | `spawn_blocking`, semaphore | Hai tác vụ đồng thời mỗi process; không tự chia một document ra hai threads |
| Layout ONNX | Native Rust subprocess | Intra-op 2, inter-op 1 |

Các file đối chiếu: [worker](../crates/ocr-worker/src/worker.rs), [recognition](../crates/ocr-worker/src/inference.rs), [post-processing](../crates/ocr-worker/src/post_processing.rs), [ONNX model](../crates/ocr-layout/src/model.rs).

Cấu hình local đang dùng `WORKER_CONCURRENCY=1`, `REGION_CONCURRENCY=1`; server model cũng giới hạn một sequence. Nâng phía Rust riêng lẻ có thể chỉ chuyển hàng chờ sang model server. Semaphore chỉ bảo vệ một process: N worker replicas có thể tạo khoảng N × R requests đồng thời vào cùng model.

Tokio khuyến nghị giới hạn CPU-bound `spawn_blocking` bằng semaphore hoặc executor chuyên dụng. Tác vụ đã chạy không thể bị abort như async task. Vì vậy vẫn cần input budgets và cancellation points. [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).

ONNX có thread pool riêng bên trong operator. Thêm Rayon, nhiều helper processes và tăng intra-op cùng lúc có thể tranh CPU; cấu hình phải theo CPU quota của Docker và workload đo được. [ONNX Runtime thread management](https://onnxruntime.ai/docs/performance/tune-performance/threading.html).

## 4. Số đo và bottleneck

Bằng chứng trước đó: một job thật bốn trang hoàn thành trong **169,03 giây**, gồm 86 regional recognition calls. Đây là một lần integration, không phải throughput benchmark. Xem [kết quả đã ghi](production-pass-validation.md).

Diagnostic mới trong pass này chạy example `postprocess_layout` trên PDF gốc và output OCR đã lưu:

- Không gọi model; giữ cả 108 native blocks và 86 OCR blocks, không có lỗi native extraction.
- Binary debug hiện có mới hơn source Rust liên quan.
- Process đầu: 174,93 ms. Mười process tiếp theo: median 132,13 ms, min 131,20 ms, max 139,99 ms.
- Đo gồm process startup, đọc file, parse JSON, Poppler, KIE, validation và pretty serialization ra `/dev/null`.

Metadata không chứa nội dung riêng tư nằm ở [diagnostic JSON](research/postprocess-diagnostic-2026-10-15.json). Đây là diagnostic cùng một sample, process mới mỗi lần nhưng OS caches đã nóng; không phải số đo release/server, p95 production hoặc accuracy benchmark. Không dùng tỷ lệ giữa hai lần đo khác điều kiện để tuyên bố speedup. **Inference là giả thuyết bottleneck ưu tiên; cần stage timings để xác nhận.**

## 5. Thứ tự tối ưu hiệu năng

### A. Đo từng stage trước

Thêm histograms/spans cho queue wait, render, detector initialization/inference, crop/encoding, model request, postprocess, validation và DB commit. Đếm regions/document, input/output tokens nếu backend trả về, retries, timeouts, budget errors, queue age và review reasons. Tránh ghi PII hoặc dùng job IDs làm metric labels.

Model server có các metric queue time, request latency, TTFT, generation tokens và KV cache; tên và tính sẵn có cần kiểm tra trên bản Metal đang chạy. [vLLM production metrics](https://docs.vllm.ai/en/latest/usage/metrics/). Hiện Rust service chủ yếu có tracing logs; chưa có metrics endpoint hoặc stage histograms.

### B. Giữ detector ấm

Hiện mỗi trang ghi PNG, spawn `ocr-layout`, đọc/hash-check model và tạo ONNX session mới. Đề xuất pool nhỏ gồm helper Rust sống lâu, mỗi helper sở hữu một session đã load, kiểm tra model hash lúc khởi động và nhận request có giới hạn. Khi timeout/crash, kill và thay helper. Giữ process isolation và hard cancellation; không chuyển native inference dài sang thread không thể dừng chỉ để bớt subprocess.

Tiếp tục dùng page/region IDs để ráp kết quả về reading order. Giảm encode/decode/copy chỉ sau khi profiling cho thấy đáng làm. Chưa đo lợi ích của warm helper trên sample này.

### C. Điều chỉnh inference concurrency theo backend

Chạy sweep có giới hạn: W=1 trước, R=1/2/4, rồi mới thử W=2 khi model và RAM còn headroom. Điều chỉnh server capacity tương ứng; không chỉ tăng biến phía Rust. Chọn cấu hình tốt theo documents/minute và p95 ở cùng workload, trong khi lỗi và field quality không xấu đi. Dừng khi swap/memory pressure, queue time, timeout hoặc chất lượng giảm.

Với vLLM GPU, batching và token budget ảnh hưởng throughput, latency và KV memory. Không sao chép cấu hình GPU lớn vào Mac. [vLLM optimization](https://docs.vllm.ai/en/latest/configuration/optimization/). Bản Metal 0.31.0 đánh dấu image inference của checkpoint Qwen3.5 dense đang dùng là experimental; giữ pin version và benchmark trên backend thực tế. [Pinned Metal support matrix](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/docs/supported_models.md).

Async region requests cho model server cơ hội batch requests độc lập; không cần một OS thread cho mỗi HTTP call. Không ghép hàng loạt crops thành một ảnh/prompt nếu chưa chứng minh không mất scope và evidence. Prefix cache chỉ giúp prefill phần thực sự tái dùng; không giảm decode mỗi chữ mới và không bảo đảm lợi ích khi các crop khác nhau. [vLLM prefix caching](https://docs.vllm.ai/en/latest/features/automatic_prefix_caching/).

### D. Checkpoint để retry không OCR lại cả tài liệu

Hiện lỗi một region có thể làm document retry từ đầu. Lưu kết quả region/page đã hoàn tất với key gồm source digest, renderer/preprocessing versions, page/geometry, detector/model revision và prompt/settings digest. Không dùng filename làm cache key.

Checkpoint phải giữ tenant/scope, hash, lease fencing và commit semantics; chỉ publish kết quả cuối khi đủ regions yêu cầu. Thay pipeline version làm cache miss. Đây thường là hướng có ích cho tải có retries, nhưng cần đo tần suất retry để xếp ưu tiên thực tế.

### E. Tối ưu KIE sau khi có profile

`make_output` hiện gọi extractor hai lần: combined evidence và raw OCR quality. Có thể tách lexical quality pass nếu chi phí đáng kể. Index native/OCR blocks theo page/spatial buckets thay vì so sánh mọi cặp, và dùng evidence refs để giảm clone/serialize. Giữ comparison budgets.

Native XML parsing, một phần formatting và full-result serialization còn chạy inline trong async path. Đo executor lag và thời gian các bước này; nếu đáng kể ở workload lớn, đưa phần CPU đó qua cùng bounded CPU executor, thay vì tạo thêm blocking tasks không giới hạn.

Không chia tùy tiện các dòng trong một document cho Rayon: role scope, reading order và linkage có phụ thuộc. Ưu tiên concurrency giữa documents/logical documents độc lập; chỉ song song hóa CPU kernels thực sự độc lập khi đo thấy cần.

### F. Tách metadata khỏi artifacts khi tăng tải

Giữ PostgreSQL làm nguồn sự thật cho jobs, versions, review và outbox; Redis làm delivery/backpressure. Response gọn giảm read/serialization JSONB. Khi artifacts lớn hoặc retention dài, dùng object storage/MinIO trong Docker và lưu refs/hash trong PostgreSQL. Không cần thêm object storage chỉ vì có một file 762 KB; quyết định theo retention, I/O và tổng dữ liệu đo được.

## 6. Chất lượng extraction: cách nâng cấp phù hợp

Giữ pipeline layout → crop → recognition → Markdown/layout JSON theo course. Thêm các bước có hợp đồng riêng:

1. Nhận diện loại tài liệu và scope: hợp đồng, phụ lục, mặt trước/sau card, party/role. `supported_template` hiện là rule match, chưa phải confidence classifier đã đo.
2. Sàng lọc native text/scan quality; giữ native và OCR như các quan sát khác nhau. Token agreement hiện tại không chứng minh khớp pixels.
3. Với mẫu ổn định, rule KIE vẫn là fast path. Với phrasing/template khác, thử schema-constrained model extraction theo logical document/role, trong token budget, cùng block IDs. LLM chỉ đề xuất candidates; Rust dựng/kiểm tra evidence.
4. Chạy type/normalization, role/linkage và cross-source validation. CCCD đủ số chữ số chưa chứng minh đọc đúng; checksum hạn chế cũng không chứng minh tài liệu thật.
5. Nếu có dấu hiệu lỗi crop/recognition, chạy recovery có giới hạn: nới crop theo lớp vùng, tăng resolution hoặc gọi recognizer khác cho vùng đó. Không chạy lại mọi trang mặc định. Giữ output gốc và provenance của lần đọc lại.
6. Field quan trọng thiếu, conflict, ngoài phạm vi hỗ trợ hoặc uncertainty chưa hiệu chuẩn → review với crop/source highlight. Không sửa dữ liệu bằng dictionary rồi âm thầm coi là gốc.

Google OCR cung cấp native extraction, rotation và các defect như blur, glare, text cutoff. Quality analysis cũng có chi phí và có thể false positive trên PDF số. Hướng áp dụng ở đây là kiểm tra/recover có chọn lọc, không thêm preprocessing nặng cho mọi tài liệu. [Google Enterprise OCR](https://docs.cloud.google.com/document-ai/docs/enterprise-document-ocr).

Mistral cũng mô tả OCR và document/bbox annotations theo schema là các chức năng tách biệt. [Mistral annotations](https://docs.mistral.ai/studio/document-processing/annotations). vLLM có JSON-schema-constrained generation. [vLLM structured outputs](https://docs.vllm.ai/en/latest/features/structured_outputs/). Từ phạm vi đó, kết luận thiết kế là JSON đúng schema vẫn cần kiểm chứng nội dung và nguồn. Structured outputs chưa được chứng minh tương thích với cấu hình Metal của service này; cần capability test trước khi triển khai.

## 7. Điều kiện để bật tự động chấp nhận

Tạo tập test khóa sớm, đại diện cho tài liệu thực tế, không trùng template/file gần giống giữa development và test. Google đề xuất ít nhất 100 tài liệu mỗi loại cho test set; đây là điểm bắt đầu đánh giá, không đủ để chứng minh mọi mức lỗi rất nhỏ. [Google dataset guidance](https://docs.cloud.google.com/document-ai/docs/create-dataset).

Tách development, calibration/threshold tuning và final held-out test. Gán nhãn thủ công có adjudication cho cases mơ hồ. Bao gồm native PDFs, scans, ảnh CCCD, dấu tiếng Việt, field trống, nhiều parties, layout mới và nguồn mâu thuẫn. Không dùng native text/model output làm ground truth duy nhất.

Đo tối thiểu:

- Field exact match sau normalization và đúng party/role; riêng ID cần đúng từng chữ số.
- Precision/recall cho việc tìm field; missing/blank và conflict detection phải có phép đo riêng.
- Evidence đúng nguồn và đúng vùng, cùng end-to-end semantic correctness; quote match chỉ là integrity check.
- Tỷ lệ document có toàn bộ critical fields đúng.
- Tỷ lệ xử lý tự động, lỗi trong tập được tự động chấp nhận và thời gian review.
- Hiệu năng p50/p95/p99, throughput, memory, retries ở workload đại diện, đồng thời giữ quality gates.

Google đánh giá extraction theo precision/recall/F1 từng label, và threshold làm thay đổi tradeoff precision/recall. F1 tối đa không nhất thiết là mục tiêu cho số định danh hoặc tiền. [Google evaluation](https://docs.cloud.google.com/document-ai/docs/evaluate).

Policy đề xuất: chỉ auto-accept khi đúng scope hỗ trợ, mọi required field đủ bằng chứng, không conflict/hard validation failure, và policy đã chứng minh đạt mục tiêu lỗi theo nhóm tài liệu. Lưu calibrator/model/prompt/pipeline/policy versions. Thay quantization, crop hoặc model phải chạy lại evaluation phù hợp.

Review cần có queue, quyền truy cập nguồn, correction revisions và explicit approve/reject; không chỉ trả `needs_review` mãi. Lấy mẫu một phần kết quả auto-accepted để phát hiện drift/lỗi tự tin sai. AWS cung cấp ví dụ routing review cho keys thiếu và confidence của key/value/words. [AWS review conditions](https://docs.aws.amazon.com/sagemaker/latest/dg/a2i-json-humantaskactivationconditions-textract-example.html).

## 8. Gói triển khai đề xuất

| Ưu tiên | Deliverable | Điều kiện kiểm chứng |
| --- | --- | --- |
| P0 | Business projection, reason codes, status-only endpoint | Consumer không phải parse diagnostics; contract cũ vẫn chạy |
| P0 | Labelled evaluation + stage metrics | Có baseline chất lượng/latency theo loại tài liệu |
| P0 | Review/correction/audit API và quyền truy cập source | Mọi thay đổi truy nguyên được; chưa phê duyệt không thành approved |
| P1 | Warm detector helper + region checkpoints | Timeout/kill/retry/fencing giữ đúng; đo before/after |
| P1 | Bounded crop recovery và classifier/scope coverage | Tăng recall mà không tăng false selections vượt mục tiêu |
| P1 | Backend-specific concurrency sweep | Chọn bằng p95/throughput/memory/error ở cùng quality |
| P2 | Optional model KIE cho template variants | Tốt hơn rule baseline trên held-out; evidence validation vẫn bắt buộc |
| P2 | Calibration và selective auto-acceptance | Đạt mục tiêu accepted-field/document error, có uncertainty bounds |

Không sửa code runtime hoặc thay concurrency trong pass nghiên cứu này. Những số đếm và diagnostic ở trên mô tả implementation hiện có; các deliverables trong bảng là công việc đề xuất.
