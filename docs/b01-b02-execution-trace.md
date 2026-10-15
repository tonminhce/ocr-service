# B01/B02 execution trace — independent evaluation v2

Ngày thực thi: 2026-10-15. Phạm vi: evaluator offline và annotation contract; không đổi raw OCR, inference model, runtime field mapping, review state hay dữ liệu private.

## Kết quả và trạng thái

**IMPLEMENTED / VERIFIED:** additive contract `ocr-eval-manifest/2.0.0`, prediction sidecar `ocr-eval-prediction/2.0.0`, CLI `score-v2`, source/graph/span/transform validation, frozen entity matching, candidate-specific grounding, CER/convex geometry metrics, cohort/quality aggregates và negative controls. V1 loader/scorer và `score`/`benchmark` giữ semantics cũ; v2 phải chọn bằng lệnh rõ ràng.

**WAITING_FOR_DATA:** independent double-labelled real corpus, adjudication và sampling protocol. Không tạo gold labels từ output hiện có; không dùng PDF đã sửa rule làm blind test.

**NOT IMPLEMENTED IN THIS PACKAGE:** live v1→v2 machine sidecar adapter với document/card identities và genuine region geometry; annotation UI; calibration/risk bound; multiparty human approval policy. Không báo accuracy của PDF thật từ fixture.

## Vấn đề → đường code → thay đổi → bằng chứng

| Vấn đề/gate | Trace thực thi | Kiểm chứng và kết quả |
|---|---|---|
| Gold dùng prediction party IDs hoặc chỉ fixed aliases có thể thưởng nhầm người | `ocr-eval/src/v2.rs`: `Case`/`Entity`, `unique_mapping`, `score_case` | Gold/prediction IDs khác hoàn toàn vẫn 5/5 entity đúng. Đổi chéo tên hai người giữ entity mapping: value/state còn 2/4, grounded còn 2/4, all-critical = 0. Reorder prediction entity/observation/field vectors không đổi score; geometry tie abstains. |
| Đúng giá trị nhưng sai thẻ/trang/nguồn | Observation có logical document/party/card refs và Anchor source/page/side; frozen observation mapping không dùng value | Valid graph đúng DOB nhưng gắn sang một thẻ khác của cùng người: value/state vẫn 4/4, grounded 3/4; missing candidate +1, extra +1. Sai page cũng giữ value score 4/4 nhưng grounded 3/4. Dangling/wrong-party card ref bị reject trước scoring. |
| State conflict nhưng thiếu candidate vẫn có thể được tính đúng | Field references từng observation và normalized candidate set, shape validator; exact candidate coverage sau mapping | Thiếu một nguồn conflict và trả singleton recovery ambiguity: conflict complete 0, missing candidate 1. Duplicate reference bị reject; duplicate observation hợp lệ bị tie/unmatched, extra candidates 2 và missing 1. |
| Gold v1 ép ambiguous phải ≥2 values | V2 `AmbiguityReason`, `State::Ambiguous` | Singleton recovery-only gold/prediction hợp lệ và không bị đổi thành found. Singleton prediction phải origin recovery. Đổi cùng candidate sang found làm all-critical = 0. |
| Missing/blank/unreadable/placeholder bị gộp | Source observation states và field state/cardinality riêng; annotation xác nhận đã nhìn pixels | Đổi observed blank thành missing giảm field state/grounding 4→3. Source blank không thể dùng empty substring của transcript có chữ để attestation. |
| Hash/span có thể bị nhầm thành semantic/pixel correctness | Source/artifact SHA, actual transcript UTF-8 span + quote SHA; semantic transcript và provenance so với independent gold riêng | Forged SHA/transcript/UTF-8 span bị reject. Transcript sai dấu nhưng hash/spans tự nhất quán được load; value/state vẫn 4/4, grounded 3/4, CER edit distance = 1. Report không nói cấu trúc hợp lệ là pixels đúng. |
| Bbox lớn/crop lẹm hoặc transform sai có thể cho điểm localization sai | Page metadata exact equality; simple convex polygons; coverage ≥90%, extraneous ≤10%; geometry cùng declared granularity | Whole-page region và clipped region không đạt grounded field; rotation/CropBox mismatch đếm transform mismatch và không dùng geometry match. Geometry scope xuất theo word/line/block, không nâng block thành word. |
| Sai dấu hoặc mất số 0 khi normalization | `nfc-typed/1.0.0`, không casefold/strip accent; typed dates/units riêng | Canonically-equivalent Vietnamese NFC bằng nhau; bỏ dấu khác; identifier 00123 khác 123; VND/month khác VND/no-period; m3 chưa support bị reject. |
| Gold mang prediction bias hoặc actor metadata giả thành chứng minh độc lập | Critical field đòi ≥2 opaque labelers, hidden initial prediction, pixels_checked và separate adjudicator | Bad/missing provenance bị reject. Guide nói rõ metadata chỉ là declaration; cần independent workflow/audit thật và chưa có corpus đó. |
| Aggregate score che khuất source mismatch hoặc subgroup | Counts/denominators, value/state accuracy riêng grounded accuracy, candidate precision/recall, conflict complete recall, all-critical per case, cohorts và quality | Representative/challenge/unseen-template aggregates tách; near-duplicate group không cross split; unseen family không vào dev/calibration; representative family có thể recur. Undefined denominator trả null. |
| Report hoặc lỗi parse làm lộ dữ liệu private | Report không gồm IDs/paths/raw values/actors. V2 parse errors dùng message bounded, không in serde invalid value chain | Aggregate privacy test và malformed-private-enum CLI test pass; stdout rỗng khi lỗi. |

## Review về resource và semantics

- `read_bounded` và `safe_resolve` tái sử dụng đường kiểm tra an toàn v1: manifest 8 MiB, prediction 16 MiB, source 32 MiB; source hash được recheck lúc score, prediction artifact hash là bắt buộc.
- Case graph bị chặn ở 32 entities, 1.024 observations, 256 fields, 32 references/field và 2.048 transcript regions; transcript text tổng ≤4 MiB. Geometry 3–16 vertices, finite, bounded, strictly convex, không tự cắt. Không đưa evaluator vào request path của API.
- Mutual best matching dùng O(P+G) state, không dựng ma trận P×G. Similarity chỉ tính geometry sau khi scope/source/field tương thích. Mapping chỉ chạy một lần mỗi case; cùng metrics được cộng vào tổng/cohort/quality, không chạy CER ba lần.
- CER dùng hai hàng dynamic programming O(n) memory. Pair dài >1.024 Unicode scalar values không chạy O(n×m); đếm unavailable. Unmatched gold observations cũng unavailable. Report CER phải đọc cùng reference characters, pair availability và candidate recall; không suy unmatched = đúng.
- Transcript anchor phải đúng bằng observation anchor, bao gồm granularity, để không lấy quote trong block rồi giả word box. Gold granularity vẫn phải được người annotation trung thực. Hash validation không xác minh source pixels.
- Gold IDs, roles, card associations và source truth được chốt độc lập. Mapping không nhìn field values; role và parent association được chấm sau mapping. Không làm per-field rematching.
- All-critical case đòi mọi critical field grounded exact và graph không extra/missing entity, không extra field/observation, không transform mismatch. Không sửa hoặc approve runtime business data.
- Không có confidence/calibration/autoaccept trong package; candidate/state metrics không được gọi là probability.

## Finding từ review chéo và bản sửa

Reviewer của luồng A02 phát hiện state comparison ban đầu chưa so `ambiguity_reason`, và origin recovery chỉ được ép ở singleton. Hai đường này có thể thưởng nhầm gold uncertain_source khi prediction trả recovery_only, hoặc chấp nhận multi-candidate recovery_only có candidate first_pass. Bản sửa:

- Semantic state, value/state match và grounded exact so cả enum state và ambiguity reason.
- Mọi prediction recovery_only bắt buộc toàn bộ observations origin recovery, bất kể số candidate.
- Hai negative controls mới: reason mismatch với graph hợp lệ không còn exact; multi-candidate first-pass hoặc mixed origin bị reject, all-recovery mới hợp lệ.

Đây là finding đã sửa và reverify; không để lại dưới dạng chỉ ghi chú review.

## Verification thực tế

Commands đã chạy:

```sh
cargo test -p ocr-eval
cargo clippy -p ocr-eval --all-targets -- -D warnings
cargo run -p ocr-eval --example synthetic_v2_fixture -- /tmp/ocr-eval-v2-reviewed-20261015
cargo run -p ocr-eval -- score-v2 /tmp/ocr-eval-v2-reviewed-20261015/manifest.json --split test
cargo run -p ocr-eval -- score crates/ocr-eval/fixtures/synthetic/manifest.json --split test
```

Kết quả: **38 tests pass** = 1 existing config unit + 1 existing benchmark HTTP test + 11 existing v1 scoring tests + **25 v2 tests**, 0 ignored/failed. Strict all-target Clippy pass; `cargo fmt -p ocr-eval` đã chạy. V1 fixture CLI thành công và tiếp tục xuất `ocr-eval-report/1.0.0`.

JSON schema được kiểm meta-schema bằng `jsonschema 4.25.1` trong isolated `/tmp` venv; synthetic manifest và prediction sidecar đều **0 validation errors**. Đây là công cụ verification, không thêm Python runtime vào service/evaluator.

Synthetic CLI output quan sát được: 1 case, 5/5 entities exact, 4/4 grounded fields, 5/5 candidates exact, 1/1 complete conflict, 5 line geometry pairs, CER 0 và all-critical accuracy 1.0. **Đây chỉ là baseline của fixture evaluator, không phải accuracy OCR thực.** Negative controls ở bảng trên là bằng chứng scorer không tặng điểm cho đúng chữ nhưng sai attribution.

Logs/artifacts local có thể đối chiếu:

- `/tmp/ocr-eval-v2-tests.log`
- `/tmp/ocr-eval-v2-clippy.log`
- `/tmp/ocr-eval-v2-report.json`
- `/tmp/ocr-eval-v1-compatibility-report.json`
- `/tmp/ocr-eval-v2-reviewed-20261015/{manifest.json,prediction.json,source.txt}` — synthetic, không có source PDF private.

## Handoff và gate tiếp theo

Các interfaces: `ocr_eval::v2::{Dataset, score, Manifest, Prediction, Report}`; CLI `score-v2`. Canonical example tạo thư mục mới và artifacts có hash để chạy lại. `score` và `benchmark` chỉ nhận v1 như trước; chưa có v2 load runner/benchmark.

B03 phải bắt đầu bằng annotation dry run 10–20 dev cases với independent annotators, rồi pilot 200–300 representative/challenge cases có adjudication. Song song, implement/review adapter export machine-only sidecar từ actual source/layout/native geometry registry; unsupported mappings cần abstain, không tạo IDs hay boxes từ gold. Sau dữ liệu frozen mới đo actual field/party/source accuracy và quyết định engine, recovery, calibration hoặc autoaccept.
