# Roadmap nâng chất lượng và năng lực production

Nghiên cứu và audit ngày 15/10/2026. **Đây là kế hoạch tiếp theo, chưa phải các tính năng vừa triển khai.** Lượt này không thay đổi code runtime, cấu hình đang chạy hay dữ liệu riêng tư.

[Kế hoạch thực thi chi tiết](production-execution-plan.md) tách roadmap này thành các gói code/test/deployment, phụ thuộc, tiêu chí nghiệm thu và rollback. Trạng thái thực thi nằm ở bảng đầu kế hoạch: M1 đã verified local, A03/A04 verified isolated, A05 logical backup/restore verified local, [A06a subprocess hygiene](a06a-execution-trace.md) đã kiểm trên Mac/Linux, [A06b parser isolation + O01 observer](a06b-execution-trace.md) đã kiểm cô lập; independent labels, production alert thresholds, quality/capacity and live adoption remain gated.

Yêu cầu đã xác nhận: **Mac hiện tại trước, trên 1.000 tài liệu/ngày; GPU là phương án nâng cấp khi có số đo.** API/consumer/layout/extraction tiếp tục dùng Rust; Docker, Redis Streams và PostgreSQL được giữ lại. Layout → crop → recognition → Markdown + layout JSON vẫn là hợp đồng đầu ra. Runtime inference bên ngoài được phép. Phạm vi tài liệu đầu tiên nên tiếp tục là hợp đồng thuê tiếng Việt và CCCD đính kèm.

## 1. Kết luận và tiêu chí chọn phương án

Ưu tiên tiếp theo là **đo đúng trên dữ liệu thật, giảm chi phí inference và hoàn thiện vận hành**. Thêm thread extraction không giải quyết nút thắt đã quan sát. Model nhỏ, engine chuyên OCR, native PDF fast path và batching đều là giả thuyết cần kiểm chứng trên cùng corpus.

Chọn cấu hình theo thứ tự: tính đúng của field/party/evidence và khả năng phát hiện mâu thuẫn → tỷ lệ tài liệu hoàn tất trong deadline → chi phí và thời gian review → tài nguyên. Một response JSON hợp schema chưa chứng minh đúng nội dung tài liệu. Một score layout lớn chưa chứng minh ngày sinh hay số tiền đúng.

Hai mốc phát hành có điều kiện riêng:

- **Production có review bắt buộc:** cần chất lượng được đo, reviewer có định danh, backup/restore, capacity/admission, alerts và retention rõ ràng. Có thể vận hành trước khi có confidence được hiệu chuẩn.
- **Tự động chấp nhận dữ liệu:** cần thêm policy được khóa phiên bản, calibration/test độc lập, giới hạn rủi ro đã chứng minh trên phạm vi hỗ trợ và giám sát sai lệch sau phát hành.

Các ngưỡng, cỡ pilot và cấu hình thí nghiệm trong tài liệu này là **đề xuất kỹ thuật**, không phải chuẩn ngành hay kết quả đã đạt.

## 2. Baseline và khoảng cách tới 1.000+ tài liệu/ngày

Baseline đã kiểm chứng nằm ở [báo cáo lần triển khai trước](production-pass-validation.md): 151 test thường, 51 test tích hợp, Docker end-to-end và một PDF thật bốn trang. Đây không phải benchmark trên corpus độc lập.

| Quan sát thực tế | Ý nghĩa cho kế hoạch |
| --- | --- |
| Client hoàn tất 4 trang trong 178,43 s, attempt 1 | Một mẫu thành công, chưa có p95/capacity đại diện |
| 86 lượt model ban đầu + 4 recovery; tổng HTTP model 165,01 s | Inference là phần ưu tiên tối ưu trong lần chạy này |
| Hậu xử lý 41,68 ms; extraction chính 4,20 ms | Chưa có lý do tăng thread KIE |
| Layout 8,81 s/4 trang, có một lần khởi tạo helper | Warm pool đã có; đo thêm trước khi tăng pool |
| Worker peak khoảng 1,75 GiB, limit 2 GiB | Cần headroom/bộ nhớ trước khi tăng document concurrency |
| Mac vLLM Metal 0.31.0, Qwen3.5-4B 4bit, W1/R1, một sequence | Khả năng batching/support phải xác nhận trên chính backend này |

Các stage có thể chồng lấn; không cộng chúng thành thời gian tài liệu.

**Sizing sơ bộ với đúng loại file mẫu:** 1.000 × 178,43 s ≈ 49,6 giờ nếu chạy tuần tự. Tương đương khoảng 484 tài liệu/24 giờ theo phép ngoại suy tuần tự, chưa tính lỗi, peak hay bảo trì. Đây không phải giới hạn throughput đã benchmark; pipeline overlap, độ dài tài liệu và batching có thể làm kết quả khác.

| Mô hình tải giả định ở mức sàn 1.000 file/ngày | Arrival trung bình | Năng lực hoàn tất cần thử nếu giữ 30% headroom |
| --- | ---: | ---: |
| Phân bố đều 24 giờ | 41,7 file/giờ | ≥59,6 file/giờ |
| Tập trung trong 8 giờ | 125 file/giờ | ≥178,6 file/giờ |

30% headroom là điểm bắt đầu thí nghiệm. Cả hai dòng giả định các tài liệu tương đương, không chứng minh SLA hay định luật scale tuyến tính. Cần ghi phân bố pages/regions/tokens, giờ cao điểm, burst và deadline; số file/ngày riêng lẻ chưa đủ để chốt phần cứng. Với mục tiêu **trên** 1.000/ngày, phải thay mức sàn bằng peak thực tế khi có dữ liệu.

## 3. Lộ trình và điều kiện nghiệm thu

| Gói | Ưu tiên và phụ thuộc | Công việc | Điều kiện hoàn tất |
| --- | --- | --- | --- |
| A | P0, bắt đầu ngay | Resource/deadline parity cho API; DB roles/secrets; backup/restore; retention | Khóa DB/large reads/cancellation bị giới hạn; runtime không có quyền quản trị; restore trên volume mới có kiểm tra hash/audit |
| B | P0, song song A | Định nghĩa supported scope, labels, pilot corpus và benchmark chất lượng | Manifest chia nhóm không leakage; nhãn critical được adjudicate; report field/role/evidence/conflict từ ảnh gốc |
| C | P1, sau baseline B | So engine và kiểm tra layout/crop/geometry | Challenger được chấm cùng dữ liệu; không tăng lỗi critical; chất lượng/chi phí/latency có số đo |
| D | P1, dựa trên lỗi B/C | Logical-document/role mapping và KIE candidate profile khi cần | Wrong-party, missing, conflict, unsupported-template có test; evidence thật và raw immutable |
| E | P1, sau capability/quality checks | Batching, pixel/token budget, memory, arrival-rate benchmark, backlog alerts | Đạt workload/deadline đã chọn với headroom, bounded backlog, không OOM hoặc quality regression |
| F | P1, reviewer tối thiểu hỗ trợ B | Review app có người dùng thật, feedback và sampling | Mọi sửa đổi truy về người/nguồn/revision; đo review time; audit mẫu cả kết quả tưởng là đúng |
| G | P2, chỉ khi đủ nhãn | Calibrated correctness và selective acceptance | Policy khóa trước test; risk/coverage có uncertainty bounds; unknown/conflict không tự accept |

A và B là hai luồng đầu tiên. F có thể làm bản tối thiểu để hỗ trợ gán nhãn B. G phụ thuộc vào dữ liệu, không thể hoàn thành bằng cách chỉ thêm code. Sau mỗi gói cập nhật báo cáo; không tự động nâng model/config khi chưa qua gate.

## 4. Gói A: sửa các khoảng trống vận hành cụ thể

Audit đọc source phát hiện các điểm sau; lượt nghiên cứu này chưa chạy tái hiện lỗi tải hay sửa chúng:

1. **API cũ chưa dùng cùng admission/serialization path.** `get_job` trong [jobs.rs](../crates/ocr-api/src/routes/jobs.rs) trả `Json<Job>` trực tiếp. Nó chưa đi qua `read_slots`, bounded off-thread serialization, response-size check và `private, no-store`/`nosniff` của [views.rs](../crates/ocr-api/src/routes/views.rs). POST lặp Idempotency-Key cũng có thể trả existing full Job. Giữ schema tương thích nhưng dùng shared bounded response path cho mọi response có OCR. Kiểm thử large concurrent reads, cancelled requests, slow clients, duplicate submit và headers; đo cả buffer đang gửi tới client, không chỉ permit serialize.
2. **Deadline enqueue chưa bao phủ chờ DB.** Timeout upload hiện bao quanh multipart; admission advisory lock trong `submit_inner` của [store.rs](../crates/ocr-core/src/store.rs) chưa có timeout transaction-local như các mutation lease. Áp dụng lock/statement/operation deadlines có chủ đích. Giữ khóa trong test, kiểm tra request kết thúc đúng hạn, transaction/pool hồi phục và retry cùng idempotency key không tạo job trùng. PostgreSQL phân biệt `lock_timeout` và `statement_timeout`; pool acquisition timeout không thay thế hai giới hạn này. [Tài liệu PostgreSQL](https://www.postgresql.org/docs/18/runtime-config-client.html).
3. **Tách quyền DB trước deploy ngoài local.** [compose.yaml](../compose.yaml) dùng cùng danh tính khởi tạo và runtime, cùng local defaults. Với DB mới, official image tạo `POSTGRES_USER` có quyền superuser; trạng thái role trên volume hiện có chưa được kiểm tra. Tạo owner/migrator/runtime/backup roles, migration one-shot, secrets theo file và production override không có default password. Runtime không được alter schema/role hoặc vô hiệu hóa audit. [Official image](https://raw.githubusercontent.com/docker-library/docs/master/postgres/README.md), [Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/).
4. **Backup cần chứng minh restore.** Chốt RPO/RTO, mã hóa backup ra ngoài host, retention và cảnh báo backup hỏng. Restore vào project/volume mới, đối chiếu source/result hash, review revisions và job recovery với Redis rỗng. Volume, Redis AOF và outbox không thay thế backup PostgreSQL. Dùng logical backup hoặc base backup/WAL theo RPO cần đạt. [PostgreSQL backup](https://www.postgresql.org/docs/18/backup.html).
5. **Retention của review cần policy riêng.** Prune hiện có xóa job và audit cùng nhau; review không gia hạn `jobs.updated_at`. Cần `retain_until`/retention class, hold khi cần, export/archive và thời hạn backup tương ứng. Test approval gần TTL, hold/release và xóa/khôi phục không làm mất hoặc hồi sinh record sai policy.

Parser/renderer hiện có timeout, page/pixel/output limits và kill/reap. Với file không đáng tin cậy, bổ sung helper/container bị giới hạn CPU/RAM/PID/scratch, không network và không kế thừa DB/model secrets. Phạm vi này là hardening thêm, không phải exploit đã xác nhận. [OWASP upload guidance](https://cheatsheetseries.owasp.org/cheatsheets/File_Upload_Cheat_Sheet.html).

## 5. Gói B: corpus và thước đo đúng nghiệp vụ

Pilot đề xuất **200–300 tài liệu thật** để tìm lỗi và chọn hướng; synthetic fixtures vẫn dùng regression nhưng không thay nhãn thật. Thu thập có quyền sử dụng, lưu local/private; không gửi PDF riêng tư lên dịch vụ cloud trong quá trình nghiên cứu.

Nhãn đọc từ **ảnh gốc**, không lấy output model làm ground truth. Ghi exact value, normalization cho phép, party/role, contract hay attachment, mặt trước/sau CCCD, page/region, missing/unreadable/conflict, trạng thái unsupported. Double-label/adjudicate các field critical như CCCD, DOB, ngày cấp, tiền thuê/cọc và liên kết chủ thể.

Bao phủ nhiều template, digital PDFs, scans/photos, thiếu dấu, m²/m³, vùng trống, crop cắt chữ, bảng, nhiều bên, nhiều card cùng trang và mâu thuẫn giữa nguồn. Nhãn mẫu đã dùng sửa rule chỉ thuộc development. Chia theo người/tài liệu gần trùng/template family; giữ test cho template chưa thấy và một lát cắt theo thời gian. Không đưa hai bản scan của cùng hợp đồng vào hai split.

Tách **representative test** lấy mẫu ngẫu nhiên từ luồng tài liệu dự kiến khỏi **challenge/template-holdout test** được chủ động làm khó. Báo cáo cả hai riêng. Tỷ lệ lỗi, review/coverage và binomial bound của production cần tập đại diện và policy cố định; không dùng tỷ lệ case khó được chọn bằng tay để suy ra tỷ lệ thật. Theo dõi cả kết quả theo nhóm/template, không chỉ số trung bình toàn tập.

Report tối thiểu:

- CER có giữ dấu tiếng Việt, exact critical-character errors, text coverage/crop cutoff.
- Precision/recall từng field; exact critical-field correctness theo **toàn tài liệu**.
- Party/role/source attribution, normalized state và conflict recall; unsupported selections riêng.
- Evidence đúng page/region/pixels; hash/span integrity tiếp tục là invariant riêng.
- Latency p50/p95, goodput, RAM, model calls/tokens và review time trên cùng workload.

Google Document AI đánh giá per-label precision/recall/F1 và threshold tradeoff, nhưng matching entity thông thường không dùng geometry. Vì vậy ta cần thêm source/role/location metrics. DocILE là tham chiếu cho field type + location và phân tầng layout chưa thấy/ít/nhiều mẫu. [Google evaluation](https://docs.cloud.google.com/document-ai/docs/evaluate), [DocILE](https://github.com/rossumai/docile).

Chưa chốt một con số “99% accuracy” chung. Critical identifier sai một chữ số, chọn nhầm người và không phát hiện conflict phải xuất hiện thành lỗi riêng, kể cả CER toàn trang vẫn rất tốt.

## 6. Gói C: chọn engine và giữ bằng chứng ảnh

Giữ engine Qwen hiện tại làm baseline. Dùng cùng ảnh/crops và provenance để so recognizer, rồi đánh giá lại end-to-end để bao gồm layout/routing. Không suy ra winner từ leaderboard khác ngôn ngữ/hardware.

| Nhánh thử | Mục đích | Điều kiện trước benchmark |
| --- | --- | --- |
| Qwen3.5-4B hiện tại | Baseline, vùng khó | Pin model/runtime/config và sampling đã đo |
| PP-OCRv5 Latin recognizer qua Rust/ONNX | Fast path text tiếng Việt, line geometry thật | Official checkpoint hỗ trợ Vietnamese; kiểm tra export, preprocessing, charset và decode parity; có line detection bên trong block |
| PaddleOCR-VL | Đối chứng model chuyên tài liệu trên Mac nếu chạy đúng | Capability test; Metal 0.31.0 vẫn ghi experimental |
| GLM-OCR 0.9B | Đối chứng đúng hướng layout + recognition của course | Kiểm tra backend; không giả định Metal hỗ trợ chỉ vì upstream vLLM có recipe |
| Native PDF text đã kiểm tra | Giảm model calls cho digital text phù hợp | Đo Unicode/geometry/coverage/visible-text agreement; route scan, text thiếu và disagreement sang OCR/review |

PP-OCRv5 liệt kê Vietnamese trong `latin_PP-OCRv5_mobile_rec`. Đây là lý do thử, không bảo đảm đáp ứng corpus. Rust có thể gọi ONNX; export/check parity là công cụ build riêng, không biến consumer thành Python. Model cần detector/preprocessing/decoder phù hợp chứ không chỉ đổi tên weights. [Official multilingual models](https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/main/docs/version3.x/algorithm/PP-OCRv5/PP-OCRv5_multi_languages.en.md).

GLM-OCR chính thức dùng PP-DocLayoutV3 + regional recognition và có model 0.9B. Rust worker có thể gọi inference qua HTTP; không cần Python SDK ở consumer. Kích thước nhỏ không chứng minh nhanh hơn hay đọc dấu Việt tốt hơn. [GLM model card](https://huggingface.co/zai-org/GLM-OCR), [upstream vLLM recipe](https://recipes.vllm.ai/zai-org/GLM-OCR).

Support matrix **đúng tag Metal 0.31.0** ghi Qwen3.5 dense image và PaddleOCR-VL experimental; GLM-OCR chưa được liệt kê. GLM trên Linux GPU là nhánh thử sau khi có phần cứng, không là khả năng đã xác nhận trên Mac. [Pinned Metal matrix](https://github.com/vllm-project/vllm-metal/blob/v0.31.0/docs/supported_models.md).

Native fast path nên là **profile mới có đánh giá**, không đổi âm thầm output course hiện có. Giữ original PDF, phân biệt native/OCR và route từng region. Text layer có thể thiếu, sai Unicode, ẩn hoặc lệch pixels; sự tồn tại của layer không đủ để bỏ OCR. Google cung cấp native-PDF routing và quality/rotation options; quality analysis cũng có chi phí và false positives, nên preprocessing phải theo lỗi quan sát. [Enterprise OCR](https://docs.cloud.google.com/document-ai/docs/enterprise-document-ocr).

Hướng đích đề xuất là router: text dễ qua engine rẻ đã kiểm chứng; table/formula/vùng khó qua model phù hợp; chỉ fallback vùng cần thiết với budget. Giữ region IDs và reading order. Đánh giá router trên test riêng: agreement giữa hai engine không tự chứng minh đúng.

Geometry phải do recognizer/native parser/detector thực sự cung cấp và map đúng về trang gốc. Không chia một block thành word boxes giả. Tìm coverage gaps/crop clipping và thử padding, deskew hoặc rerender vùng có vấn đề; literal `#` hay detector score thấp một mình không đủ kích hoạt recovery. Chỉ giữ biến thể giúp phát hiện/đọc đúng trên development corpus mà không tăng unsupported text.

## 7. Gói D: mapping field đa template, không sửa dữ liệu bằng phỏng đoán

Phân tích lỗi trước khi quyết định thêm model KIE. Nếu lỗi nằm ở recognition/crop thì parser thông minh hơn không khôi phục được evidence đã mất.

1. Scope logical documents và front/back regions, role headings, nhiều parties/cards, attachment association. Liên kết theo observed identifier phải giữ trạng thái ambiguous khi không xác định duy nhất.
2. Rule profile đã kiểm chứng là fast path cho mẫu hỗ trợ. Với biến thể rule không bao phủ, thử schema-constrained model **chỉ đề xuất candidates và block references** theo logical scope.
3. Rust kiểm tra type, raw value/normalization, quote/span/source, party, candidate distinctness và cross-source conflict. Model không được tự sinh hash/geometry đáng tin, sửa sự thật trong contract hay chọn card đè contract.
4. Candidate không có evidence hợp lệ bị loại/flag; document text được coi là dữ liệu, không là chỉ dẫn thực thi. Extraction model không có tool/network/DB privileges.
5. Recovery observation vẫn riêng; không tính lần retry cùng model thành corroboration độc lập. Blank/unreadable/unsupported không được biến thành một value “hợp lý”.

KIE model chỉ được promote nếu thắng baseline theo tiêu chí đã định trước trên field/role/evidence/state và nằm trong latency budget. Chạy shadow trước, cùng policy review. Mistral tách document/bbox annotations theo schema khỏi OCR là một tham chiếu kiến trúc, không phải lý do phải dùng cloud. [Document annotations](https://docs.mistral.ai/studio/document-processing/annotations).

## 8. Gói E: throughput thực tế trên Mac và điều kiện chuyển GPU

Trước sweep, thêm report bounded cho pixels, prompt/output tokens nếu backend trả về, finish reason, queue/inference latency, worker cgroup và host unified memory. Nếu metric không có trên Metal, ghi unavailable; không bịa TTFT hay token count. Truncation phải là failure/recovery/review có lý do, không tính là đủ chữ chỉ vì JSON parse được.

Thử tuần tự, dừng nhánh không có lợi:

| Thí nghiệm | W | R | Model max sequences | Layout pool |
| --- | ---: | ---: | ---: | ---: |
| E0 baseline | 1 | 1 | 1 | 1 |
| E1 sau memory/capability check | 1 | 2 | 2 | 1 |
| E2 chỉ nếu E1 tốt | 1 | 4 | 4 | 1 |
| E3 sau memory sizing | 2 | 1 | 2 | 1 |

Ghi cấu hình model **thực sự chạy**, không chỉ metadata do benchmark khai báo. Dùng native batching cho các request độc lập trước; ghép nhiều crops vào một prompt làm khó attribution và hiện không phù hợp giới hạn một ảnh/request.

Sau đó thử từng biến: max pixels theo vùng, output budget theo text/table/formula, padding, engine routing và native fast path. Không hạ resolution/tokens cùng lúc với đổi model. Không coi cache là giải pháp chính cho các crop khác nhau: checkpoint đã có cho retries; prefix cache chỉ giảm phần prefill tái sử dụng, không giảm decode mới. [vLLM prefix caching](https://docs.vllm.ai/en/v0.31.0/features/automatic_prefix_caching/).

Mở rộng Rust benchmark từ closed-loop 1–4 clients sang arrival-rate, burst, sustained load và backlog-drain. Dùng dữ liệu riêng, bounded total work và thời hạn; không spam hàng nghìn job lên deployment đang giữ tài liệu thật. Theo dõi các job chưa xong khi kết thúc phép đo, tránh chỉ thống kê request thành công. Phương pháp request-rate/goodput có ở [vLLM serving benchmark](https://docs.vllm.ai/en/v0.31.0/cli/bench/serve/); kiểm tra CLI/backend cụ thể trước khi áp dụng.

Gate đề xuất cho một thay đổi tối ưu: improvement đủ lớn để bù độ phức tạp (ví dụ ≥20% goodput lặp lại được), không làm xấu critical quality hay deadline, không OOM/lease failure/retry amplification. Profile peak RAM mục tiêu 75–80% giới hạn trên stress corpus, không tăng dần qua soak; đây là headroom thử nghiệm, không bảo đảm worst case. Cgroup worker không bao gồm toàn bộ memory inference trên host. [Docker resource limits](https://docs.docker.com/engine/containers/resource_constraints/).

Thêm backlog collector tần suất thấp cho eligible queued count/oldest age, due outbox age, expired leases, failure rate và worker progress; model saturation/health, Redis memory/PEL và DB storage/pool. Không chạy query nặng trong request hoặc scrape hot path. Test dừng worker khi vẫn nhận job, model timeout/503, Redis/PG gián đoạn, restart giữa region và drain backlog. Alerts phải chỉ rõ hành động; API readiness hiện có chỉ nói có thể nhận bền vững, không chứng minh model sẵn sàng. [SRE alerting](https://sre.google/workbook/alerting-on-slos/).

Khi có nhiều worker, budget inflight phải xét cả replicas × R và năng lực model. Chỉ thêm global dispatcher/admission khi deployment cần; một Redis lock cho từng crop không tự làm nhanh hơn.

**Gate chuyển GPU:** cấu hình Mac tốt nhất đã qua quality gate vẫn không đạt workload/deadline/headroom đã chốt, hoặc cost/review latency không phù hợp. Khi đó chạy cùng corpus và arrival profile trên Linux GPU vLLM; giữ nguyên Rust/Docker/Redis/PostgreSQL contract, so goodput, tail latency, memory và chi phí/tài liệu. Không mua/chọn SKU GPU từ một PDF; compute size phải theo model, tokens/ảnh, batching và VRAM đo thực tế. Native/cheap OCR có thể giúp Mac đạt tải, nhưng chưa có bằng chứng để hứa điều đó.

## 9. Gói F/G: review hữu dụng và confidence có ý nghĩa

Review API hiện có nhận diện service principal. Bổ sung app tin cậy có người dùng đăng nhập, roles và audit actor không cho client giả mạo. Hiển thị field/alternatives cạnh page/crop, nguồn contract/card, lý do review và revision. Đo thời gian xử lý và agreement; test stale edit, reopen và correction sau approval. Tenant isolation chỉ bắt buộc khi phục vụ các khách hàng/đơn vị không cùng miền tin cậy; không áp đặt hệ thống đa tenant cho scope nội bộ hiện tại.

Với trên 1.000 tài liệu/ngày, phải đo **năng lực review và thời gian đến business approval** riêng với OCR execution. Ước tính `reviewed files/day × mean review minutes`, staffing, utilization/headroom và review backlog/oldest age. Ví dụ giả định 1.000 file đều cần 2 phút review là 33,3 giờ công/ngày; cần khoảng 6 người làm 8 giờ ở mức sử dụng 70%. Đây là ví dụ sizing, chưa phải review time đã đo. Mốc production có review chỉ đạt khi cả machine queue và human queue nằm trong deadline đã chọn; tăng OCR throughput mà để review backlog tăng vô hạn chưa đạt mục tiêu.

Thu feedback có kiểm soát. Review các case missing/conflict/unreadable, đồng thời lấy mẫu ngẫu nhiên kết quả nhìn có vẻ đúng để phát hiện lỗi bị bỏ lọt. Hard-case review là dữ liệu development có thiên lệch lựa chọn; nó không tự đại diện cho production test distribution. AWS mô tả missing-key routing và random sampling như hai trigger riêng. [Human review conditions](https://docs.aws.amazon.com/sagemaker/latest/dg/a2i-json-humantaskactivationconditions-textract-example.html).

Giữ tách biệt layout score, recognition score, field correctness estimate và business decision. Microsoft cũng phân biệt word transcription và field confidence; không phải mọi field có confidence. [Confidence concepts](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/concept/accuracy-confidence?view=doc-intel-4.0.0).

Sau khi có nhãn, fit correctness model/calibrator offline; runtime Rust nạp versioned artifact nếu phù hợp. Đo reliability/Brier và risk-versus-coverage trên test untouched. Temperature scaling là tham chiếu calibration cho classifier, không phải bảo đảm cho VLM token probability hoặc self-rating. [Guo et al.](https://proceedings.mlr.press/v70/guo17a.html).

Policy phải quy định critical fields theo workflow, supported scope, source/role validation và hard stops cho conflict, unknown template, recovery-only hoặc missing critical values. Calibration chọn threshold; test chỉ đánh giá policy đã khóa. Mọi thay đổi model/quantization/prompt/preprocess/policy đều cần đánh giá lại. Không bật autoaccept chỉ vì score vượt 0,9.

Ví dụ toán học cho mục tiêu tự chấp nhận: với policy cố định và **0 tài liệu được chấp nhận sai** trong n case độc lập đại diện, giới hạn trên một phía 95% của unsafe-document error là `1 - 0.05^(1/n)`. Muốn ≤0,1% cần ít nhất **2.995 accepted documents** trong điều kiện đó. 300 case không lỗi chỉ cho bound khoảng 0,994%; fields trong cùng file không được tính như các trial độc lập. Đây là minh họa về cỡ bằng chứng, không chứng nhận rủi ro tương lai dưới distribution shift. [Exact binomial limits, NIST](https://itl.nist.gov/div898/software/dataplot/refman2/auxillar/exacbino.htm).

## 10. Release và phạm vi để sau

Mỗi thay đổi nên có invariant/regression tests, quality report trên dev, performance report khi ảnh hưởng tài nguyên, và compatibility report. Tới release: chạy test độc lập đúng policy, build immutable image, lưu model/config/source fingerprints, SBOM và dependency scan, chạy synthetic E2E trên image sẽ deploy, canary/shadow, rollback tương thích migration. Test phần mềm đã qua không thay cho chất lượng tài liệu thực tế.

Giữ PostgreSQL BYTEA/JSONB tới khi đo thấy DB/WAL/backup/retention là nút thắt. Object storage là nhánh theo dung lượng và consistency policy, không mặc định thêm MinIO. Signature hiển thị đọc được bằng OCR không là xác minh chữ ký số; module xác minh mật mã/trust là scope khác nếu nghiệp vụ yêu cầu.

Chưa ưu tiên Kubernetes, API gateway, thay queue/DB, thêm Rayon cho extraction vài ms, training/fine-tuning trước error taxonomy, hoặc nhiều worker trên Mac khi chưa có memory/load proof. Bước đầu cụ thể là **gói A + chuẩn labels/manifest/report của B**, rồi dùng số đo B chọn C/D/E cần làm tới mức nào.
