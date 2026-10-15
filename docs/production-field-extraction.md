# Trích xuất fields cho OCR service: nghiên cứu và thiết kế đề xuất

Ngày nghiên cứu nguồn: **14/10/2026**; trạng thái implementation cập nhật **15/10/2026**. API Axum, Redis Streams, PostgreSQL và consumer worker chạy bằng Rust trong Docker; Qwen chạy qua vLLM Metal trên Mac. Default OCR là pipeline Rust PP-DocLayoutV3 → crop polygon → nhận dạng từng vùng → formatter SDK 0.1.4. **Profile scoped `vi_rental_contract_v1` đã được triển khai**, gồm native-text/OCR evidence song song, field mapping bằng rule, chuẩn hóa, đối chiếu CCCD và quyết định `needs_review`. Xem [hợp đồng runtime và giới hạn](structured-extraction.md). Schema generic, extraction bằng LLM và quy trình review/acceptance mô tả bên dưới vẫn là thiết kế đề xuất; các bảng “hiện tại/cần bổ sung” giữ bối cảnh của nghiên cứu ban đầu.

## Quyết định chính

Giữ Rust API/worker và vLLM đã có, nhưng tách **đọc tài liệu**, **đề xuất field**, **kiểm chứng field** và **quyết định cần review**. Model có thể đề xuất giá trị; Rust kiểm tra kiểu dữ liệu, nguồn chứng minh, vai trò và xung đột trước khi hệ thống khác dùng kết quả.

Đầu ra hữu ích cần trả lời: giá trị nào, của ai, xuất hiện ở đâu, chuẩn hóa theo quy tắc nào, có mâu thuẫn hay không. JSON parse được chưa đủ để trả lời những câu này. vLLM hỗ trợ ràng buộc JSON Schema; từ phạm vi tính năng đó, kết luận thiết kế ở đây là vẫn cần kiểm chứng ngữ nghĩa sau generation. Chưa kiểm thử structured outputs trên backend Metal đang cài. [vLLM structured outputs](https://docs.vllm.ai/en/latest/features/structured_outputs/).

## Điều đã quan sát trên máy này

Các số đo dưới đây thuộc chiến lược full-page ban đầu. Kết quả pipeline layout mới được ghi riêng trong [báo cáo layout end-to-end](layout-e2e-report.md); không dùng số đo cũ để chứng minh chất lượng nhận dạng từng vùng.

Hai lần xử lý PDF riêng tư đều đi đủ đường API → PostgreSQL → Rust worker → Qwen thật → kết quả được lưu và poll qua API. Bản 1 MP hoàn thành 4 trang ở lần thử đầu trong khoảng 113 giây. Bản 1,5 MP hoàn thành trong khoảng 122 giây. Đây là thời gian của từng job trong điều kiện local, không phải benchmark throughput.

Ở bản 1 MP, một mã hợp đồng dài bị mất hai chữ số dù văn bản tổng thể rất giống nguồn. Bản 1,5 MP đã đọc đúng chuỗi đó; độ tương đồng ký tự chuẩn hóa của ba trang có text layer lần lượt là 1,0000, 0,9947 và 0,9734. Những chỉ số này **không phải độ chính xác fields**, và text layer trang CCCD không đủ để làm ground truth cho ảnh thẻ.

Hợp đồng và ảnh CCCD chứa các ngày không khớp nhau. Đó là mâu thuẫn giữa nguồn, cần giữ các quan sát riêng và báo `conflict`; không thể sửa bằng việc chọn lần xuất hiện cuối cùng. Chi tiết có thông tin cá nhân chỉ nằm trong `artifacts/user-pdf/` bị Git ignore, không sao chép vào nghiên cứu công khai.

Bài kiểm tra ảnh tổng hợp chỉ chứa `OCR TEST 123` đã **thất bại về nội dung**: Qwen đọc sai một phần dòng chữ và sinh thêm một bài hướng dẫn kiểm thử dài. Response vẫn kết thúc bằng `stop`, nên job hiện tại được đánh dấu `succeeded`. Script giữ kết quả thất bại và dừng trước fixture PDF. Điều này cho thấy trạng thái thực thi thành công và prompt yêu cầu không bịa không bảo đảm nội dung đúng. Xem [bằng chứng chạy model thật](real-e2e-report.md).

Một lần thử PDF tổng hợp 2 trang riêng biệt hoàn thành qua cùng Rust service trong khoảng 15 giây; cả hai trang khớp chính xác chuỗi kỳ vọng sau chuẩn hóa case/punctuation/whitespace và đúng thứ tự. Không sửa hoặc bỏ qua assertion ảnh để đổi trạng thái script ban đầu thành pass. Kết quả PDF này và failure ảnh cùng được giữ trong báo cáo.

## Các hệ thống production lưu gì

Google Document AI tách `mentionText`, `normalizedValue`, entity type, text/page anchors và confidence. Azure tách content gốc, typed value, spans và bounding regions. Thiết kế này giúp đối chiếu một field với vùng tài liệu và giữ bản gốc khi chuẩn hóa. [Google Document v1](https://docs.cloud.google.com/document-ai/docs/reference/rest/v1/Document), [Azure Analyze Result v4](https://learn.microsoft.com/en-us/rest/api/aiservices/document-models/get-analyze-result?view=rest-aiservices-v4.0%20(2024-11-30)).

Vai trò phải được nêu rõ. AWS hướng dẫn phân biệt các field cùng tên trong các phần khác nhau và chỉ định phạm vi trang khi đặt query. Trong hệ thống này, cách áp dụng là hỏi CCCD của Bên A, CCCD của Bên B và người đứng tên trên phụ lục như ba quan sát có scope riêng. [AWS query best practices](https://docs.aws.amazon.com/textract/latest/dg/bestqueries.html).

Confidence cũng có nhiều nghĩa: đọc đúng chữ, chọn đúng field, phân loại đúng tài liệu. Azure nhấn mạnh việc xét cả OCR và field extraction; không phải field nào cũng có score. Vì vậy Qwen tự ghi `0.98` trong JSON không trở thành xác suất đã được đo. Thiết kế ban đầu dùng `confidence: null`, giữ các tín hiệu riêng và chỉ thêm score sau khi đánh giá/calibrate trên dữ liệu được gán nhãn. [Azure accuracy and confidence](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/concept/accuracy-confidence?view=doc-intel-4.0.0).

## Pipeline phù hợp cho tài liệu này

1. **Giữ nguồn bất biến và tách bundle.** Lưu file gốc, SHA-256, số trang và cấu hình xử lý. Nhóm hợp đồng, phụ lục, mặt trước/mặt sau thẻ; hai vùng tài liệu có thể nằm trên cùng một trang. Giữ mapping về trang và vùng gốc.
2. **Lấy text và vị trí từ nguồn phù hợp.** Với PDF số, kiểm tra text layer và ưu tiên text native khi phù hợp với bản render. Với ảnh scan, chạy OCR có vị trí và crop vùng cần đọc ở độ phân giải đủ. Text layer có thể sai encoding, ẩn hoặc lệch; không chấp nhận chỉ vì nó không rỗng. Docling cũng có pipeline native tách text cells và bitmap khỏi bước OCR/model. [Docling native PDF](https://docling-project.github.io/docling/usage/advanced_options/#extract-the-native-content-of-a-pdf).
3. **Qwen đề xuất candidates có scope.** Trích từng logical document/role, dùng schema và định nghĩa field rõ ràng. Xác định vai trò từ nội dung/heading; không giả định Bên A luôn là bên cho thuê ở mọi template. Model trả raw value cùng ID các source blocks; Rust tự dựng quote/span từ block đã lưu. Trường hợp chỉ có text, offset phải được kiểm tra bằng UTF-8 bytes. Không tin offset hoặc bounding box model tự viết ra.
4. **Rust kiểm chứng và chuẩn hóa.** Kiểm tra schema, reference tồn tại, đúng trang/vùng, quote khớp source, ngày hợp lệ, số định danh là string, tiền là decimal string kèm currency và kỳ thanh toán. Quy tắc format chỉ phát hiện một số lỗi; chuỗi đủ 12 chữ số vẫn có thể là CCCD đọc sai.
5. **Đối chiếu và review.** Gán phụ lục cho một party chỉ khi có đủ bằng chứng liên kết. Giữ các candidates nếu mơ hồ; tạo issue khi nguồn khác nhau. Review các field quan trọng chưa đủ chứng minh. Lưu correction, người sửa, thời điểm và bằng chứng riêng, giữ OCR gốc.

Quote khớp Markdown chứng minh field xuất phát từ Markdown đó; nó chưa chứng minh Markdown khớp ảnh. Bài thử hallucination cho thấy cần kiểm tra thêm nguồn native hoặc OCR có vị trí/ảnh crop. Chạy lại cùng một model có thể lặp lại cùng lỗi; đồng thuận giữa hai lần gọi không phải kiểm chứng độc lập.

Diagram tương tác của pipeline đề xuất: [field-extraction-pipeline.html](field-extraction-pipeline.html). Nội dung sơ đồ bằng tiếng Việt; giao diện viewer và thuộc tính HTML language dùng fallback tiếng Anh. Đây là sơ đồ thiết kế, không phải các stage đã chạy trong worker.

## Schema và những field cần phân biệt

[JSON Schema v1](../schemas/field-extraction-v1.schema.json), [ví dụ dữ liệu giả](../schemas/field-extraction-v1.example.json), [quy tắc kiểm chứng](../schemas/README.md) là hợp đồng đề xuất. Các trạng thái giá trị gồm `found`, `missing`, `unreadable`, `ambiguous`, `conflict`. Missing không tự động có nghĩa là model đã chứng minh field không có; chất lượng nguồn và phạm vi đọc phải đủ để phân biệt với unreadable.

Schema và ví dụ đã qua Draft 2020-12 validation; 4 source hashes và 13 evidence spans khớp chính xác dữ liệu giả. Các phép thử âm từ chối field found không có evidence, số định danh kiểu number, accepted còn conflict, confidence thiếu calibration record và region không có nguồn định vị. Đây là kiểm tra cấu trúc thiết kế, chưa kiểm tra extraction thật hoặc business policy trong worker.

| Nhóm | Fields và điều cần giữ riêng |
| --- | --- |
| Hợp đồng | Mã hợp đồng, ngày ký, ngày bắt đầu/kết thúc, logical document ID |
| Các bên | Danh sách party có ID ổn định; vai trò cho thuê/thuê/đại diện; tên, định danh, địa chỉ theo từng nguồn |
| Tài sản | Địa chỉ cho thuê, khác với địa chỉ cư trú hoặc địa chỉ in trên CCCD |
| Thanh toán | Tiền thuê, tiền cọc, utilities, kỳ thanh toán; số tiền, currency và đơn vị kỳ riêng |
| Phụ lục CCCD | Holder, số thẻ, ngày sinh, ngày cấp/hết hạn, mặt thẻ và source scope; quan hệ với party có thể chưa xác định |
| Đánh giá | Evidence, candidates, validation issues, review decision và versions của toàn bộ pipeline |

Số định danh, tài khoản và số điện thoại là string để giữ số 0 đầu. Ngày gốc giữ nguyên; chỉ thêm ISO date khi cách đọc không mơ hồ. Tiền không dùng floating point; không suy ra currency chỉ từ ngôn ngữ tài liệu. Giá trị được suy luận phải ghi `method=derived` và các đầu vào, không gắn evidence trực tiếp giả.

Pipeline mới có geometry ở cấp vùng do detector thực đo: `bbox_2d`, polygon, kích thước trang đã render/upright và raw region text. Nó chưa có box của từng từ hoặc liên kết field-to-region. Bước extraction phải tham chiếu vùng này và kiểm tra quote; không tự gán box chính xác của một field từ box cả đoạn. Với kết quả full-page cũ, giữ `polygon=null`. JSON Schema không kiểm chứng hash, liên kết source, nội dung ảnh hoặc chính sách bắt buộc field; các kiểm tra đó cần Rust validators.

## Chọn OCR/model trong giới hạn Rust + Docker

Giữ Qwen đang chạy làm baseline extraction. Để có OCR text kèm vị trí với ít thay đổi nhất, có thể thử Tesseract local bằng subprocess được giới hạn deadline và `vie` language data, trả TSV/hOCR cho Rust parse. Đây là một baseline cần đo, chưa phải kết luận nó tốt hơn model hiện tại. [Tesseract languages](https://tesseract-ocr.github.io/tessdoc/Data-Files-in-different-versions.html), [TSV/hOCR output](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html).

Paddle OCR/layout là ứng viên đối chiếu vì pipeline chính thức có text detection, recognition và vị trí. Nếu muốn orchestration hoàn toàn Rust, chạy detector/recognizer đã export bằng ONNX Runtime và viết preprocessing, crop, decode, postprocessing trong Rust. Rust bindings của ONNX Runtime là dự án community; gọi runtime chưa tương đương port trọn PP-Structure. Không tự thêm Python consumer hoặc layout service vào scope này. [ONNX Runtime community APIs](https://onnxruntime.ai/docs/get-started/community-projects.html), [nghiên cứu local và lựa chọn engine](field-extraction-local-research.md).

Cloud là nguồn tham khảo thiết kế, không mặc định thay model cho tiếng Việt. Google/Azure có hỗ trợ tiếng Việt ở một số OCR/custom modes; mode generative có giới hạn khác. Textract không liệt kê tiếng Việt trong các ngôn ngữ được hỗ trợ. Kiểm tra theo đúng processor/model/version, không suy từ “OCR multilingual” sang “extract CCCD Việt Nam chính xác”. [So sánh capability và giới hạn nguồn chính thức](field-extraction-cloud-research.md).

## Những thay đổi cụ thể cần có trong Rust service

| Hiện tại | Cần bổ sung cho extraction |
| --- | --- |
| Course jobs giữ upload gốc và SHA-256 đến terminal retention; baseline full-page xóa bytes khi thành công | Artifact storage cho rendered pages/crops nếu cần audit byte-exact và review lâu hơn retention |
| Job chưa snapshot model/profile/schema/settings; worker đọc config khi chạy | Snapshot cấu hình ở submission; model revision và quantization cụ thể, OCR/extraction prompt, normalizer và policy versions |
| Idempotency so SHA-256 + media type, dùng key toàn service | Fingerprint cả extraction profile/options; đổi profile không được replay field cũ dưới cùng ngữ nghĩa |
| `succeeded` nghĩa là inference hoàn thành và result được lưu | Giữ execution status; thêm extraction decision độc lập như pending review/accepted, không dùng thành công job để chứng minh field đúng |
| Result có Markdown, raw/formatted text và geometry cấp vùng; crops không được giữ | Field-to-region links, exact spans, source artifacts và mapping cần thiết cho review |
| Chưa có profile fields hoặc validator nghiệp vụ | Module extraction typed, normalization, evidence checks, conflicts và versioned acceptance policy |

Đề xuất thêm modules `artifacts`, `extraction`, `validation` trong workspace hiện tại. Axum tiếp tục chỉ nhận job/poll kết quả; Rust worker thực hiện các stage và queue/lease đã có. Result envelope có thể chứa cả `ocr` và `extraction`, nhưng cần version API/schema trước khi thay đổi contract. Chưa triển khai các modules/endpoint mới trong pass nghiên cứu này. Docker vẫn là deployment mục tiêu.

## Đo độ tin cậy trước khi tự động chấp nhận

Tạo bộ tài liệu đã gán nhãn với các mẫu hợp đồng khác nhau, scan mờ/xoay, nhiều parties, nhiều phụ lục cùng trang, missing fields, xung đột và tài liệu lạ. Tách evaluation khỏi prompt examples/training; tránh để cùng template/người/file gần trùng rò rỉ giữa các tập. Không gán nhãn ground truth chỉ bằng output của model.

Đo riêng: exact match field, đúng role, evidence đúng, chuẩn hóa đúng, hallucination khi field thiếu, tỷ lệ tất cả critical fields đúng, tỷ lệ review và lỗi trong nhóm tự động accepted. Số định danh phải đúng từng chữ số; character similarity tổng thể không thay thế được phép đo này. Google đánh giá precision/recall/F1 theo label và cho phép chỉnh confidence threshold; ngưỡng tối đa F1 có thể khác mục tiêu giảm false accept của ứng dụng. [Google evaluation](https://docs.cloud.google.com/document-ai/docs/evaluate).

Bản đầu nên trả candidates có evidence và yêu cầu review critical fields. Chỉ bật tự động accepted sau khi tập held-out chứng minh đạt mục tiêu lỗi của ứng dụng. Lưu phiên bản và theo dõi lỗi theo loại tài liệu/template/chất lượng ảnh; thay model, quantization, prompt hoặc preprocessing phải đánh giá lại.

Thứ tự triển khai: artifact retention + job profile snapshot → native text/spatial OCR baseline → schema candidates + Rust validators → conflicts/review → labeled evaluation → selective acceptance. Ưu tiên field đúng và kiểm tra được trên corpus thật trước khi tăng số model hoặc port toàn bộ layout pipeline.

## Tài liệu kèm theo

- [Cloud research: Google, Azure, AWS](field-extraction-cloud-research.md).
- [Local research: Paddle, GLM-OCR, Docling, Tesseract, ONNX](field-extraction-local-research.md).
- [Schema và ví dụ giả](../schemas/README.md).
- [Sơ đồ pipeline đề xuất](field-extraction-pipeline.html).
- [Kết quả Qwen chạy thật và các failure](real-e2e-report.md).
- [Cài đặt Qwen local](local-qwen-macos.md).
