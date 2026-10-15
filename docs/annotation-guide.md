# Gold annotation v2 và cách đánh giá trước khi đổi model

V2 là contract đánh giá offline độc lập. Nó không thay `layout.json`, `business.json`, review API hay v1 scorer. Lệnh v2 chỉ nhận prediction sidecar `ocr-eval-prediction/2.0.0`; không tự diễn giải dữ liệu v1 thành word boxes, card IDs hoặc gold labels.

## 1. Bảo vệ nguồn và tránh leakage

1. Giữ PDF gốc, page renders, labels và sidecar private trong thư mục ignored với quyền hạn chế. Không đưa số giấy tờ, ảnh hoặc transcript lên dịch vụ public để annotation.
2. Chốt SHA-256 của source và prediction artifact. Manifest dùng đường dẫn tương đối cùng thư mục; loader kiểm tra traversal, symlink escape, regular file và byte budgets. Source được kiểm tra lại khi score.
3. `id`, `group_id`, `template_family`, document/party/card IDs và actor IDs là opaque; không dùng tên người, CCCD, email hoặc tên file chứa PII. Gold IDs được tạo trước và độc lập với UUID output.
4. Một hồ sơ, phụ lục, bản sửa và near-duplicates cùng `group_id`, cùng split. Exact duplicate hash bị từ chối. Template có thể lặp giữa dev/test trong cohort representative; cohort `unseen_template` phải là test và family không xuất hiện trong dev/calibration.
5. Representative test lấy theo luồng thực tế đã chốt cách lấy mẫu. Challenge và unseen-template là cohort riêng; không gộp challenge enrichment thành accuracy đại diện tải thực.

## 2. Coordinates và identities

Mỗi page khai báo kích thước raster thực, `rotation_degrees` và `crop_box_pdf` `[x0,y0,x1,y1]` theo PDF points. Polygon dùng **rendered pixels**, gốc trên trái, x sang phải, y xuống dưới, sau khi áp CropBox và rotation. Không lấy bbox chuẩn hóa 0..1000 mà khai là pixels. Renders phải cùng source và transform đã chốt; schema metadata không tự chứng minh pixels đúng.

Mỗi polygon có 3–16 đỉnh theo thứ tự, strictly convex, không tự cắt, diện tích dương và nằm trong page. Polygon không convex cần tách thành vùng được annotation rõ ràng hoặc để unsupported; scorer này chưa hỗ trợ polygon tùy ý. `source_kind` phân biệt contract, identity_card, attachment, other. Identity card bắt buộc front/back; nguồn còn lại dùng not_applicable.

Tạo entity graph trước khi đánh field:

- Document: logical contract/attachment scope, không có parent hoặc role.
- Party: thuộc một document, có role landlord/tenant/unknown.
- Card: thuộc một document và party của chính document đó. Hai thẻ khác nhau vẫn là hai entity ngay cả cùng một người.

Mỗi entity có primary source anchor độc lập với field value. Party anchor cần đủ context nhận diện heading/role và người. Không đổi anchor cho vừa model; không lấy cùng một box cho hai entity không phân biệt được. Card front/back là observations cùng card ID khi được người kiểm tra xác nhận association. Cùng tên hoặc gần nhau trên trang không đủ để tự ghép.

Scorer đóng băng mapping **mutual unique best IoU ≥ 0.5**, cùng kind/source/side/page/transform, cho cả case. Ties abstain. Mapping không sử dụng tên, số giấy tờ, role hoặc giá trị cần chấm; role và liên kết parent được chấm sau. Vì vậy đổi chéo tên hai người không làm scorer tìm mapping mới để tăng điểm. Missing/extra entities có denominator riêng và chặn all-critical pass.

## 3. Source observations trước business truth

Mỗi observation gắn document, party khi có, card khi là card source, field, source anchor và `visible_text` chép nguyên nội dung thấy được. Không sửa dấu, thêm đơn vị hay suy ngày sinh từ thông tin khác. Giá trị typed nằm riêng trong `value`; raw visible text luôn được giữ.

Trạng thái source:

| State | Cách annotation |
|---|---|
| present | Có chữ đọc được; transcript nguyên văn và typed value bắt buộc. |
| source_blank | Người đã nhìn vùng pixels và xác nhận để trống; transcript rỗng, value null, vẫn có polygon. |
| unreadable | Có dấu/chữ nhưng không đọc chắc; value null. Không đoán. |
| placeholder | Dấu chấm, gạch hoặc placeholder thực sự có trong nguồn; value null, transcript giữ dấu. |
| unsupported | Loại nội dung/schema chưa xử lý; value null, vẫn giữ vùng và transcript thấy được. |

Field business dùng observation IDs của **từng candidate**. `found` có đúng một normalized value (có thể nhiều nguồn cùng xác nhận). `conflict` có ít nhất hai giá trị khác nhau đang thực sự hiện trong các nguồn. `ambiguous` giữ candidate nhưng không chọn truth; singleton bắt buộc `ambiguity_reason=recovery_only`, mọi prediction observation của recovery_only đều phải origin recovery, kể cả khi có nhiều candidate. Gold origin luôn human_gold vì người tự chép nguồn. `missing` không có observations. source_blank/unreadable/placeholder/unsupported phải có observations cùng trạng thái tương ứng.

Hợp đồng ghi ngày A và thẻ ghi ngày B: chép cả A và B, giữ conflict; adjudication không đồng nghĩa chọn thẻ là truth duy nhất. Schema này đánh giá source truth và preservation of discrepancies, chưa chứng thực pháp lý hay giải quyết business conflict.

Toàn bộ observations phải được field tham chiếu. Scope của fields/observations là exhaustive cho phần case được annotation; prediction thừa field/candidate bị tính extra. Transcript artifact có thể chứa thêm OCR toàn trang ngoài scope.

## 4. Independent labels và normalization

Annotation đầu tiên phải ẩn prediction. Mọi field ghi opaque annotator IDs, `prediction_hidden=true` và `pixels_checked=true`. Field critical bắt buộc hai người label độc lập và một adjudicator khác cả hai, có adjudication cuối. Các boolean/IDs là provenance declaration; cần quy trình và audit thực tế để chứng minh độc lập, scorer không kiểm tra danh tính người hoặc nhìn pixels thay họ.

Policy `nfc-typed/1.0.0`:

- Text và identifiers chuẩn hóa NFC; giữ nguyên dấu tiếng Việt, case, whitespace và leading zeros. Không fold accent hay tự thay lỗi chính tả.
- Date dùng ngày hợp lệ ISO `YYYY-MM-DD`.
- Money dùng decimal string, giữ currency VND và period null/month; area giữ unit m2. Chuẩn hóa số thập phân không làm mất phân biệt đơn vị/period. Schema khác phải version policy mới.
- Raw transcript được giữ riêng. So transcript và CER chỉ NFC, không loại whitespace/case/diacritics.

Bắt đầu 10–20 dev cases để hai người thử hướng dẫn, đo disagreement và công annotate. Sau khi sửa hướng dẫn, pilot 200–300 cases thật theo cohort đã chốt. File đã dùng để sửa rule thuộc dev; không chuyển nó thành blind test. Repository hiện chỉ có fixture synthetic để kiểm phép tính; chưa có corpus độc lập đủ để tuyên bố accuracy hay autoaccept.

## 5. Prediction sidecar và evidence

Sidecar có source SHA, normalization policy, pages, entities, transcripts, observations, fields và `machine_only=true`. Không đưa human corrections vào machine score. Origin prediction chỉ first_pass hoặc recovery.

Mỗi observation bắt buộc evidence trỏ transcript ID, UTF-8 byte start/end (0-based, end-exclusive) và SHA-256 của exact visible text. Empty span chỉ hợp lệ cho transcript rỗng như source blank. Loader kiểm quote/hash/span và anchor observation phải bằng anchor transcript, kể cả granularity. Không biến block box thành word box bằng cách đổi tên scope. Không có numeric confidence trong v2 contract này.

Hash/span đúng chỉ chứng minh cấu trúc nhất quán với artifact. Transcript và source polygon vẫn cần independent gold để kiểm semantic attribution; chúng không tự chứng minh pixels chứa text đó. Một quote giả nhưng được hash đúng vẫn trượt semantic transcript/CER khi so gold.

Runtime business v1 hiện thiếu stable logical-document/card identities và transcript-region geometry cho contract này. **Adapter live v2 vẫn là gate còn lại**: cần lấy geometry từ native/layout registry thật, giữ transform/provenance, chỉ export supported scopes; không copy gold IDs hoặc invent region boxes. Chưa dùng v2 score để báo accuracy của file PDF thật.

## 6. Lệnh chạy và đọc report

Tạo một fixture giả hoàn toàn mới, score v2 rõ ràng:

```sh
cargo run -p ocr-eval --example synthetic_v2_fixture -- /tmp/ocr-eval-v2-example
cargo run -p ocr-eval -- score-v2 /tmp/ocr-eval-v2-example/manifest.json --split test
cargo test -p ocr-eval
```

Thư mục example phải chưa tồn tại. JSON schema của manifest nằm tại `schemas/evaluation-manifest-v2.schema.json`; schema sidecar ở fragment `#/$defs/prediction`. JSON schema kiểm shape; Rust loader là authority cho hashes, references, cardinality theo state, types, polygon, provenance và split isolation.

Report chỉ có aggregate counts/rates; không xuất source paths, case/party IDs, transcript, field values hoặc actor IDs. Representative/challenge/unseen-template và quality strata tách riêng. Không fit confidence và không autoaccept.

- `field_value_state_matches` chỉ đúng giá trị/state (bao gồm ambiguity_reason); chưa đủ grounding.
- `field_grounded_exact` đòi candidate đầy đủ, transcript/typed value/state đúng, đúng entity/card/source/page/transform và geometry cùng granularity, gold coverage ≥90%, extraneous predicted area ≤10%.
- `all_critical_cases_correct` đòi mọi critical field grounded exact, graph identity/role đúng, không extra/missing entity, không extra field/observation hoặc transform mismatch.
- Conflict complete yêu cầu đủ từng candidate, không chỉ state conflict.
- CER dùng Unicode scalar values sau NFC. Matched pair >1024 ký tự và unmatched gold observation được đếm unavailable; CER reference denominator không giả định chúng đúng. Empty gold transcript không đóng góp ký tự. Đọc CER cùng matched/unavailable/candidate coverage, không dùng CER một mình.
- Geometry report chia word/line/block. Block grounding không phải word-level localization. Pair không match không có coverage trung bình; missing/unmatched counts vẫn giữ riêng.
- Rates không có denominator trả null. Đây không phải calibrated correctness probability hay statistical risk bound.

## 7. Release gates còn mở

B03 labels thật và adjudication; source/page rendering registry có transform; reviewed machine-only live sidecar adapter; sampling protocol và frozen held-out tests; confidence calibration/risk bound; và business review policy v2 cho multiparty/reassociation chưa hoàn tất. Không được dùng synthetic pass hoặc annotation metadata tự khai để vượt những gate này.
