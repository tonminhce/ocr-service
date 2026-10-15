# Kế hoạch thực thi chi tiết: Rust OCR production và capacity

Ngày 15/10/2026 — **M1 verified local; A03/A04 verified isolated; A05 logical restore verified local; A06a subprocess verified Mac/Linux; E02 audit/observer verified; A06b isolation và các gate còn lại giữ riêng**.

Tài liệu chuyển [roadmap nghiên cứu](production-next-roadmap.md) thành các gói thực thi có phụ thuộc, code liên quan, kiểm thử và điều kiện phát hành. Đợt đầu P00/A01/A02/B01/B02/E01 có kết quả trong [M1 execution review](m1-execution-review.md). A03 và A04 đã kiểm trên DB/volumes mới trong [A03 execution trace](a03-execution-trace.md) và [A04 execution trace](a04-execution-trace.md); [E02 memory audit](e02-memory-audit.md) đã xác định retained process và kiểm observer. Live M1 chưa cut over; retention apply, đổi model và concurrency sweep chưa thực hiện. Những gói không được đánh dấu thực thi bên dưới vẫn là kế hoạch.

| Gói | Trạng thái thực thi |
|---|---|
| P00 | Contract/baseline đã ghi trong [operations-contract](operations-contract.md) |
| A01/A02 | Code + targeted tests + review chéo đã qua; full verification trong M1 report |
| B01/B02 | V2 contract/scorer opt-in đã có; negative controls đã qua. Live v2 exporter/adapter còn chưa có |
| E01 | Bounded truthful telemetry đã có; full verification trong M1 report |
| B03/F00 independent corpus | WAITING_FOR_DATA; không tự sinh gold từ machine OCR |
| A03 | IMPLEMENTED / VERIFIED ISOLATED: 205 ordinary +67 explicit integration tests; packaged restricted-role Docker flow qua. Live adoption/cutover chưa làm |
| E02 | AUDIT + OBSERVER VERIFIED: helper giữ khoảng1,58GiB;7 focused observer tests + live idle smoke qua. Allocator A/B, workload envelope/headroom chưa qua |
| A04 | IMPLEMENTED / VERIFIED ISOLATED: 209 ordinary +78 explicit integration tests toàn workspace; 11 retention race/guard tests và packaged Docker hold→completion→release qua. [Trace](a04-execution-trace.md). Policy/apply/live cutover vẫn chưa thực hiện |
| A05 | IMPLEMENTED / VERIFIED LOCAL ISOLATED: Rust PG18/age tooling, 227 ordinary +86 explicit integration; Linux17 binary tests; two full restores and empty-Redis worker recovery. [Trace](a05-execution-trace.md). Off-host/PITR/RPO/RTO/deletion reconciliation/live activation remain gated |
| A06a | IMPLEMENTED / VERIFIED:240 ordinary +86 explicit integration trên Mac;86 Linux ARM64 release tests. [Trace](a06a-execution-trace.md). Env/commands/process-group/scratch/shutdown hygiene; chưa phải parser sandbox |
| A06b/O/C/D/E03–E04/F/G | PLANNED; A06b real isolated renderer/IPC chưa triển khai. Goal tạm dừng sau A06a theo yêu cầu người dùng; không tự chuyển trạng thái nhờ tests của gói khác |

## 1. Mục tiêu và ràng buộc đã chốt

- Mac hiện tại trước; GPU là phương án sau khi benchmark chứng minh cần.
- Hơn 1.000 tài liệu/ngày; phân bố giờ cao điểm, độ dài tài liệu và deadline chưa chốt.
- API/Axum, consumer, layout, extraction và công cụ đánh giá dùng Rust. Inference server bên ngoài được phép; công cụ export/reference model chỉ dùng trong build/evaluation, không thêm Python consumer.
- Docker, Redis Streams và PostgreSQL; giữ kiến trúc hiện có.
- Giữ output layout → crop → recognition → Markdown + layout JSON. Hybrid/native là profile mới, không âm thầm đổi ý nghĩa profile course hiện tại.
- Raw source, OCR, candidates và evidence bất biến. Correction/approval, native text và recovery là các quan sát/quyết định riêng.
- Confidence hiện tại tiếp tục null; automatic acceptance tắt trong các milestone đầu.
- Triển khai bằng các gói nhỏ, có report và rollback. Không commit/push, reset worktree, xóa volumes hoặc đổi live secrets trong bước lập kế hoạch.

## 2. Quyết định cần chốt trước khi tuyên bố đạt production

Các điểm này không chặn viết tooling hoặc test cô lập; chúng chặn việc tự tuyên bố SLA, bật xóa dữ liệu hay tự chấp nhận field.

| Quyết định | Cách xử lý trong kế hoạch |
| --- | --- |
| Mức tải chính xác trên 1.000/ngày | Dùng 1.000 làm sàn; thử thêm scenario **1.200/ngày** như giả định growth, không coi là yêu cầu đã được xác nhận |
| Tải phân bố 24 giờ hay tập trung 8 giờ; burst | Benchmark cả hai scenario; thay bằng arrival profile thực tế khi có |
| Mix pages/regions/pixels/tokens | Ghi từ pilot; chia buckets 1, 2–4, 5–10 và 11–20 trang; không ngoại suy một PDF thành toàn corpus |
| Deadline intake, OCR completion và business approval | Báo cáo riêng; chốt trước capacity acceptance; không chọn một p95 tùy ý rồi gọi là đạt |
| Critical fields và mức rủi ro chấp nhận | Field policy theo workflow; pilot dùng CCCD, DOB/ngày cấp, tiền thuê/cọc và liên kết đúng người làm các nhóm cần adjudicate |
| RPO/RTO, nơi backup, retention/audit hold | Chỉ xây tooling/dry-run trước; không bật destructive policy hoặc gửi backup ra đích bên ngoài khi chưa có cấu hình/quyết định cụ thể |
| Identity provider của reviewer | Adapter cho nguồn định danh tổ chức nếu có; fixture local chỉ dùng test, không thay danh tính thật |

Sizing tham chiếu: PDF bốn trang hiện mất 178,43 s; 1.000 file tương tự chạy tuần tự cần khoảng 49,6 giờ. Với 1.200 file/ngày, arrival trung bình là 50/giờ trong 24 giờ hoặc 150/giờ trong 8 giờ. Capacity target thử nghiệm ở 70% utilization tương ứng khoảng 71,4/giờ và 214,3/giờ. Đây là phép tính planning, chưa phải benchmark hoặc cam kết phần cứng.

## 3. Những bất biến phải giữ qua mọi gói

1. Idempotency, transactional outbox, Redis replay, heartbeat và SQL lease fencing giữ đúng hành vi hiện có. Timeout sau commit có thể là kết quả chưa rõ với caller; retry cùng key phải lấy lại đúng job.
2. Không sửa migrations đã phát hành `0001`–`0005`; thêm migration mới có forward/rollback compatibility plan.
3. Client cũ giữ schema/status code/Location của submit và full-job GET; endpoint/profile/version mới phải opt-in khi thay semantics.
4. Recovery-only candidate không tự thành found; conflict không tự chọn nguồn; hash/span hợp lệ không được mô tả thành bằng chứng OCR đúng pixels.
5. Giữ party/document/source scope; cùng giá trị nhưng sai người hoặc sai card vẫn là lỗi.
6. Geometry chỉ được công bố đúng granularity và coordinate transform thực sự có. Human transcription không được giả làm machine OCR span.
7. Không dùng dữ liệu test để sửa model/prompt/rule/threshold; synthetic và private development regression không trở thành chứng nhận accuracy.
8. Mọi concurrency, bytes, process, retries, output và thời gian đều có budget. Không mở thêm thread/helper/replica chỉ để tăng số lượng.
9. Không đưa document text, tên, CCCD, filename, job ID vào metric labels; error/log/report tổng hợp phải kiểm soát PII.

## 4. Milestone và thứ tự triển khai

| Milestone | Gói chính | Có thể song song | Điều kiện chuyển bước |
| --- | --- | --- | --- |
| M0 — Khóa contract | P00 | — | Compatibility/invariant/test matrix và cấu hình baseline được ghi rõ |
| M1 — Intake/resource và thước đo | A01, A02, B01, B02, E01 | API/core, evaluator và inference telemetry là ba luồng riêng | Tests cơ chế qua; baseline raw output không đổi; scorer có negative controls |
| M2 — Data và vận hành bền vững | A03–A05, O01, B03, F00 | Labels/annotation, DB/ops và observer | Runtime hạn quyền; restore drill qua; pilot labels có adjudication và baseline report |
| M3 — Chất lượng nhận dạng/mapping | C01–C05; D01 nếu lỗi đòi hỏi | Geometry/adapter có thể làm trước khi đủ corpus; promotion chờ nhãn | Challenger/router thắng hoặc có kết luận dừng rõ ràng; nguồn/role không sai thêm |
| M4 — Capacity có đo | E02–E04; A06 và O02 trước external rollout | Runner bằng fake HTTP có thể xây sớm; real runs tuần tự | Đạt arrival/deadline/headroom đã chọn, bounded backlog và fault recovery |
| M5 — Production có review | F01–F02, retention và alerts/release | UI/identity có thể bắt đầu sớm sau contract annotation | Người thật thao tác có audit/evidence; cả machine queue và review queue đạt deadline |
| M6 — Selective acceptance | G01–G02, D02 chỉ nếu cần | Report scaffolding có thể xây sớm | Đủ independent labels, fixed policy và risk bound; không đạt thì giữ review bắt buộc |

Không xây toàn bộ challenger cùng lúc. Sau baseline B03, chọn lỗi chiếm ưu thế để quyết định C/D nào đáng triển khai. Gói có gate dữ liệu chưa đạt được ghi **WAITING_FOR_DATA**, không đánh dấu hoàn thành bằng synthetic tests.

## 5. P00 — Khóa baseline và contract

**Files:** [OpenAPI](openapi.yaml), [business/review contract](business-review.md), [validation report](production-pass-validation.md), API/client fixtures và `docs/operations-contract.md` mới.

**Việc làm:** ghi response compatibility; raw/source/profile/schema/model/config fingerprints; các resource/deadline knobs; phân biệt durable intake readiness, processing health và business approval. Ghi max pages/upload/output hiện tại, trạng thái token/geometry thực tế, và cờ thử nghiệm mặc định off. Không in expanded Compose hoặc credentials.

**Gate:** checklist có thể đối chiếu bằng fixture; source mẫu đã dùng sửa rule được đánh dấu development. Code implementation sau này bắt đầu từ baseline này. Không cần chạy lại toàn bộ test suite chỉ vì sửa tài liệu.

## 6. Luồng A/O — Reliability và vận hành

### A01 — Shared bounded HTTP response path

- **Files:** `crates/ocr-api/src/routes/{jobs,views}.rs`, `config.rs`, `lib.rs`; module `routes/response.rs` mới; `tests/http.rs` và test socket/backpressure mới.
- **Thiết kế:** dùng chung admission, bounded CPU serialization và cache/security headers cho legacy GET, idempotent POST replay, business, review history và artifacts. Giữ JSON/status/Location tương thích. Writer dừng ở giới hạn bytes trong lúc serialize; không chỉ kiểm tra sau khi đã cấp phát hết. Preflight size từ DB khi phù hợp, với wire limit vẫn là invariant riêng.
- **Bộ nhớ:** tách budget handler, CPU và bytes/bodies/connections; permit CPU sống tới khi closure dừng. Body-owned accounting, giới hạn connections/streams/buffers và slow-client deadlines cần phối hợp. Chỉ giữ permit tới body EOF không chứng minh socket đã gửi hết; phải kiểm tra bằng transport thực. Status nhỏ có budget riêng để large artifact reads không làm nghẽn polling.
- **Tests:** legacy/full/replay response parity; overlimit stored output; cancelled serialize không mở thêm CPU; network reader rất chậm; clients ngắt kết nối; status vẫn tiến triển dưới large reads; memory/buffers plateau và slot được trả.
- **Gate/rollback:** vượt capacity trả lỗi ổn định; RSS phù hợp budget trên stress fixture. Không schema migration; có thể revert binary, nhưng không bỏ giới hạn đã cần cho deployment ngoài local.

### A02 — Enqueue/SQL/request deadlines

- **Files:** `ocr-core/src/store.rs::submit_inner`, `ocr-api/src/{config,error}.rs`, `routes/jobs.rs`; core store và HTTP tests.
- **Thiết kế:** upload deadline riêng; enqueue total deadline bao gồm pool acquire và transaction. Dùng transaction-local lock/statement timeouts với lock budget nhỏ hơn statement/operation budget; map transient failure thành API code có retry guidance. First-party caller giữ cùng Idempotency-Key.
- **Tests:** giữ admission advisory lock, idempotency row lock, pool exhaustion; timeout trước insert, quanh commit và caller cancellation; xác minh một job/outbox row, không slot/transaction leak; heartbeat không bị đói tài nguyên.
- **Gate/rollback:** request kết thúc trong allowance đã cấu hình và pool phục hồi. Không hứa timeout nghĩa là chưa lưu. Giữ error contract/documentation trước khi rollout; không đổi lease semantics.

### A03 — Migrator, runtime roles và secret files

**Trạng thái:** implemented/verified trên deployment cô lập; xem [trace và findings đã sửa](a03-execution-trace.md), [operations](a03-operations.md). Không adopt live DB. A03 kiểm maintenance chỉ đọc; A04 sau đó bổ sung narrow hold/release function. Worker mới bỏ auto-prune, DELETE/TRUNCATE jobs không cấp cho runtime/maintenance.

- **Files:** `ocr-core/src/store.rs`, API/worker `main.rs`, config loaders; Rust admin binary mới, bootstrap/grants SQL ngoài schema migrations; `compose.production.yaml`/ops profile mới; `Dockerfile` và docs.
- **Thiết kế:** migrator one-shot; API/worker production startup kiểm tra schema version thay vì tự chạy DDL. Owner NOLOGIN, migrator, runtime API/worker, maintenance và backup có quyền theo nhu cầu. Secret-file loading có quy tắc env/file rõ ràng, fail khi xung đột hoặc thiếu; không đưa credentials quản trị vào app containers.
- **Lưu ý:** đổi `POSTGRES_USER` trong Compose không sửa role của existing volume. Bootstrap phải additive. Outbox trigger hiện chạy theo invoker: kiểm thử grants thực tế; nếu cần security-definer thì schema qualification, fixed search_path và quyền execute phải được giới hạn.
- **Tests:** bằng đúng restricted credentials: submit → outbox → worker → review; từ chối CREATE ROLE/DDL/disable audit/update history; startup từ chối schema không hỗ trợ; migrator chạy một lần; secret file không lộ logs/inspect env. Tests admin hiện có không thay thế permission tests.
- **Gate/rollback:** thử trên DB/volumes mới trước. Không rotate live credentials hoặc reset DB từ scripts. Rollback image phải tương thích schema và migration mode; không khôi phục quyền superuser cho app làm đường tắt.

### A04 — Retention/holds và quyền xóa

**Trạng thái:** tooling/guards implemented và verified trên deployment cô lập;
[execution trace](a04-execution-trace.md), [operations](retention-operations.md).
Migration0006, bounded preview, independent holds, replay/CAS/audit và held-source
completion đã qua tests. Policy không tự suy từ `RETENTION_DAYS`; apply luôn disabled.

- **Files:** `0006_retention_guards.sql`, `ocr-core/src/retention.rs`, completion trong `store.rs`, admin CLI, administration ACL/inventory, Compose maintenance và tests. Review runtime không đổi; race tests gọi review transaction thật.
- **Thiết kế đã kiểm:** nullable unassigned policy/class/deadline/review metadata; plan tối đa100 rows, keyset index, database snapshot time và10s total budget; maintenance chỉ EXECUTE narrow function, không direct writes/deletion. Giữ raw OCR/evidence; metadata source completion phản ánh bytes thật dưới cùng khóa.
- **Compatibility:** DB chặn mọi jobs DELETE/TRUNCATE và thay held input, kể cả privileged legacy SQL. Không down-migrate audit/holds. A03 binary từ chối schema6 trong packaged test; rollback phải qua release/restore rehearsal, không chạy old binary lên schema mới.
- **Tests đã chạy:**11 retention integration tests gồm cả hai thứ tự review/hold/release/completion, cancellation trước COMMIT, independent holds và release budget, metadata pagination;78 explicit integration tests toàn workspace PASS. Packaged restricted-role Docker test giữ source qua completion và không prune khi release. Destructive cascade, restore và mọi COMMIT outcome chưa được chứng nhận.
- **Gate còn lại:** policy/class/review grace semantics, A05 backup/restore và authoritative deletion reconciliation, live adoption và O02 release/rollback. Không có env enable flag, DELETE grant hoặc destructive implementation trong A04.

### A05 — Backup và restore drill

**Trạng thái:** local tooling và isolated drill đã kiểm chứng; xem [execution trace](a05-execution-trace.md), [runbook](backup-operations.md). Full production backup/restore activation chưa nghiệm thu. Runtime không được grant hoặc khởi động tự động; synthetic-only rehearsal có explicit test activation riêng.

- **Files:** Rust admin/ops commands hoặc scripts gọi công cụ PostgreSQL chuẩn; ops Compose profile; `docs/backup-operations.md`, Rust `ocr-backup`/content-manifest và gated `backup_rehearsal` helper.
- **Thiết kế:** chọn logical backup hoặc base backup/WAL theo RPO; mã hóa bằng công cụ đã được kiểm chứng, không tự viết crypto; manifest gồm version/schema và hashes/counts không PII. Backup destination/key/retention là cấu hình riêng. Không copy live PG volume thành “backup”.
- **Tests:** restore vào project/volume mới, Redis rỗng; kiểm source/result hashes, review revisions/audit, checkpoints, queue/outbox và expired lease recovery. Backup fail, disk full, file hỏng/thiếu key phải hiện lỗi. Không chạy restore trên volume hiện tại.
- **Gate/rollback:** báo cáo elapsed time/size/data-loss window và so RPO/RTO. Restore-time reconciliation tính lại TTL và xử lý post-snapshot deletions từ authoritative tombstone/deletion manifest bền vững ngoài snapshot đang restore, có access/retention policy riêng. Restored DB một mình không biết những gì đã bị xóa sau backup. MVP chỉ chứng minh deterministic TTL; restored service giữ isolated/unavailable tới khi có manifest hoặc reconciliation đã được kiểm chứng. Thiếu nguồn đó thì không hứa chống hồi sinh record đã xóa hay gọi restore production complete. Historical backup expiry là một policy riêng. Tooling có thể xong trước khi off-host destination được chọn; backup production không được đánh dấu xong chỉ bằng local dump.

### A06 — Parser/renderer isolation

**A06a đã kiểm trên Mac + Linux ARM64:** [trace](a06a-execution-trace.md). Typed Poppler builders, fixed executable resolution/env allowlist, bounded group supervision/capture/admission, scratch lifetime và cancelled-shutdown barrier đã có. **A06b chưa triển khai**; native PDF/image decode vẫn ở worker boundary. Không bật external rollout nhờ A06a. Phiên tạm dừng sau bàn giao theo yêu cầu người dùng; tiếp tục bằng A06b trước O01 khi được yêu cầu resume.

- **Files:** worker `preparation.rs`, native parser path, Docker/ops profile; helper Rust và bounded IPC tests nếu cần tách container.
- **Hai bước:** trước hết scrub subprocess environment, allowlist executable/args và giữ kill/reap/budgets. Sau đó persistent renderer/helper không network, không DB/model secrets, có riêng CPU/RAM/PID/scratch bounds; không spawn Docker mỗi trang. Đưa Poppler và untrusted decode qua cùng boundary khi có thể.
- **Tests/gate:** sentinel secret không tới child; malformed/huge PDF, oversized frames/path traversal, cancellation/timeout/OOM không để orphan/scratch leak; next valid job vẫn chạy; overlay/crop/order parity qua. Đo IPC overhead. Env scrub đơn lẻ không được mô tả là sandbox hoàn chỉnh.
- **Rollback:** engine/render protocol versioned; không bật lại unsandboxed upload path cho dữ liệu không đáng tin cậy chỉ để rollback thuận tiện.

### O01/O02 — Observer, alerts và release

- **Files:** `ocr-core/src/metrics.rs`, SQL observer/admin, worker queue/progress signals; protected exporter; `.github/workflows/ci.yml`, Docker build/release docs và ops alerts mới.
- **Observer:** snapshot cache theo interval; queued/eligible/delayed counts và oldest age, due outbox age, expired leases, last progress, failures, Redis PEL/memory, DB pool/storage và model health; freshness/error của collector riêng. Query không nằm trong request/scrape hot path; interval/query budget được benchmark.
- **Tests:** dừng worker nhưng vẫn accept một job; model timeout, Redis/PG failure; alert phân biệt intake/processing, backlog drain, backup failure, collector stale. Không PII labels.
- **Release gate:** immutable image/model/config digests; packaged synthetic E2E; SBOM/dependency scan/provenance; migration compatibility và rollback rehearsal. Baseline CI checks giữ lại; lỗi đã có tests mới không đòi lặp vô hạn mọi checks.

## 7. Luồng B/F00 — Annotation và evaluator trước lựa chọn model

### B01 — Gold annotation contract v2

- **Files mới/đổi:** `schemas/evaluation-manifest-v2.schema.json`, `docs/annotation-guide.md`; tách evaluator modules `manifest/labels/scoring/report` khi việc thêm v2 cần, giữ v1 reader/scorer.
- **Contract:** source SHA + page dimensions/rotation/CropBox; opaque case/logical-document/party/card IDs độc lập prediction IDs; per-source observations và candidates, source type/front/back, exact visible text, typed normalization, page/real polygon, legibility/blank/placeholder/unsupported, annotator/adjudication metadata private.
- **State:** hỗ trợ ambiguous singleton recovery-only; không bắt nó thành conflict hoặc found. Tách source content truth khỏi business truth: contract/card thật sự ghi khác nhau là conflict cần giữ, không chọn card làm gold duy nhất.
- **Labels:** NFC policy có version, giữ dấu/leading zeros/currency/period/unit; raw transcript riêng. Nhãn source_blank cần kiểm pixels. V1 labels không được diễn giải lại theo v2.
- **Tests/gate:** schema boundaries, duplicate/dangling IDs, multibyte spans, geometry/source mismatch và recovery singleton; legacy v1 fixtures vẫn cho cùng score.

### B02 — Scoring đúng field, role và nguồn

- **Files:** `ocr-eval/src/lib.rs` và các module mới, `tests/scoring.rs`, synthetic fixtures.
- **Metrics:** exact value/state và all-critical-fields-correct theo case; matching party/logical doc/card không phụ thuộc prediction UUID; candidate-specific source/evidence và đủ conflict candidates; unsupported candidates, CER giữ dấu, line coverage/crop clipping; geometry coverage/extraneous area theo block/line/word granularity. Một box lớn phủ cả trang không được điểm precise-field grounding.
- **Entity matching:** khóa thuật toán one-to-one predicted↔gold và điều kiện attribution trước khi chấm challengers; cùng mapping được dùng cho mọi field trong case. Unmatched/extra/missing entity có penalty riêng. Không rematch từng field để vô tình cho hai người bị tráo giá trị cùng đạt điểm đúng; wrong-person/card negative controls phải chứng minh điều này.
- **Report:** denominators và null khi không có mẫu; representative/challenge/strata riêng; machine prediction tách human corrections; aggregate report không in raw values. Giữ hash/span integrity riêng với pixel correctness.
- **Negative controls:** đúng chữ sai người/card/page; thiếu một conflict candidate; duplicate candidate; missing vs observed blank; forged quote/hash; rotation/CropBox sai; field value đúng nhưng evidence không hỗ trợ.
- **Gate:** mọi negative control bị tính lỗi đúng loại, không inflated accuracy. Chưa cần thêm real model run để xác nhận phép tính scorer.

### B03/F00 — Pilot dataset và annotation workspace tối thiểu

- **Việc làm:** trước tiên 10–20 development documents để kiểm hướng dẫn/adjudication và đo effort; sau đó pilot 200–300 tài liệu thật. File mẫu đã biết và biến thể dùng sửa rule thuộc dev.
- **Splits:** grouping chống case/revision/attachment/near-duplicate/person leakage khi liên quan; hash chỉ bắt exact duplicate. Representative test lấy mẫu luồng dự kiến; challenge/unseen-template cohort riêng. Representative có thể chứa template family phổ biến đã thấy ở dev, nhưng không cùng case; strict template-family holdout là cohort đo khả năng tổng quát riêng. Không ép bỏ template phổ biến rồi gọi test là phân bố production.
- **Annotation:** đọc ảnh gốc, ẩn machine suggestion ở vòng gán nhãn độc lập; critical fields được hai người gán và adjudicate. Gold người/card/scope riêng; lưu record riêng tư có quyền truy cập. Không tự sinh gold từ OCR/model khác.
- **Workspace:** source/page viewer và gold export/import tối thiểu qua Rust/Axum nếu hữu ích; annotation không đồng nghĩa sửa/approve business job. Offline label files vẫn dùng được trước khi có UI.
- **Gate:** data consent/access phù hợp, manifests frozen, no leakage, critical labels có provenance/adjudication, baseline report có error taxonomy. Tooling có thể làm ngay; real documents và independent labels là phụ thuộc dữ liệu, không được giả lập thành kết quả đã đạt.

## 8. Luồng C/D — Engines, geometry và mapping có kiểm soát

### C01 — Recognition seam và baseline parity

- **Files:** worker `inference.rs`, `settings.rs`, `formatting.rs`, checkpoint key builder; module `recognition/{mod,openai_compatible,router}.rs` đề xuất; inference/layout/retry tests.
- **Contract:** task + bounded crop + transform + engine/config identity → immutable text observation, optional genuine geometry/usage/finish reason. Giữ orchestration/formatter hiện tại; không dựng generic plugin registry.
- **Cache:** actual crop bytes + model/runtime/tokenizer/charset/prompt/preprocess/router/profile revisions; switching engine phải miss. Stable detector-region ID riêng với formatter index vốn có thể merge/reindex.
- **Gate/rollback:** default Qwen course adapter raw output byte-equivalent trên fixtures; bounds/cancellation/fencing còn đúng; adapters mới off. Không gọi native/ONNX output là `course_layout_vlm`.

### C02 — Geometry và native structure thật

- **Files:** worker `post_processing.rs`, `preparation.rs`, `native_pdf.rs`/`geometry.rs` mới; shared versioned observation types; extraction validator/schema và overlay fixtures.
- **Thiết kế:** giữ native word boxes và exact text offsets, không nối hai cột chỉ vì cùng baseline; record inverse transform của render scale, CropBox, rotation, crop, resize và deskew. Registry geometry mới opt-in; không thay box whole-block trong v1 Evidence mà validator hiện giả định bằng source block.
- **Tests/gate:** 90/180/270°, crop offsets, columns, small text, two cards trong một block; overlay khớp fixture pixels. Field highlight lấy từ token/line polygons thật; thiếu alignment thì ghi block/line, không tạo word boxes theo độ rộng chia đều.

### C03 — Rust/ONNX text OCR feasibility spike

- **Bước đầu:** pin official model/dictionary/config/license/converter/hash; kiểm export PP-OCRv5 hiện tại, input/output signatures, resize/RGB-normalization/padding/charset/blank/CTC decode bằng reference fixtures. Test **Linux ARM64 Docker CPU**, không giả định Docker có Metal/CoreML.
- **Workload:** 100–200 dev line crops và khoảng 20 block/page cases đủ dấu Việt/CCCD/ngày/tiền/m²/m³/vùng trống/mờ/rotation. Đây là spike budget, không accuracy certification.
- **Sau feasibility:** thêm text-line detection/ordering trong layout blocks và real transforms. Chỉ khi parity/quality khả thi mới thêm helper crate Rust `ocr-text` cùng pinned ONNX Runtime, persistent pool, bounded IPC và kill/reap. Không thêm ORT sessions/threads không giới hạn; tính cả pools hiện có.
- **Gate/rollback:** recognition parity, line coverage và critical character errors được so Qwen trên cùng crops; end-to-end geometry/party đúng. Fail capability/export/quality thì dừng nhánh, lưu report; baseline còn nguyên. Không dành nhiều tuần tích hợp một artifact chưa chạy đúng.

### C04 — Native early routing và VLM capability challengers

- **Native:** hiện parse xảy ra sau full OCR và token-agreement; chưa tiết kiệm inference. Tách early parser **shadow trước**, vẫn chạy OCR để đánh giá. Xây routing policy có thể kiểm tra độc lập, không dựa vào OCR agreement rồi gọi là bypass. Có thể giới hạn digital-producer/profile hỗ trợ; partial/hidden/wrong Unicode text layer, scans/cards/tables đi fallback khi chưa đủ điều kiện.
- **VLM:** một capability spike mỗi lần; tiny synthetic text/table/formula/blank/rotation, request/token/bytes/time cap. Qwen baseline; PaddleOCR-VL experimental; GLM-OCR không mặc định được coi là Metal-supported. Unsupported backend dừng ở report; Linux GPU trial là nhánh sau.
- **Gate:** same-crop comparison trước, full documents sau. Mọi speed/quality report ghi đúng backend/revision; benchmark tác giả/model size không quyết định winner. Capability pass không là corpus quality pass.

### C05 — Hybrid router opt-in

- **Files:** recognition router, strategy/profile dispatch, formatter metadata, checkpoint keys và eval fixtures.
- **Thiết kế:** validated native hoặc cheap OCR cho vùng phù hợp; table/formula/unknown qua engine đã qua task gate; fallback có reason/budget. Source/engine/geometry observation tách biệt; same-engine retry hay cross-engine agreement không tự chứng minh đúng.
- **Gate:** chọn candidate/router/config và tolerance bằng development/calibration trước, rồi mới mở frozen representative/challenge reports; saved calls/time thật, fallback/review rate và critical errors có denominators. Predeclare noninferiority/error tolerance trước xem test. Thay đổi sau khi xem test cần chu kỳ validation mới; chưa chọn tolerance thì chưa promote.
- **Rollback:** profile flag + immutable image/config; không rewrite stored raw hoặc tái dùng checkpoint khác engine.

### D01/D02 — Scope graph và optional KIE

- **D01 files:** `ocr-extraction/src/{rules,lib,normalize}.rs`, profile/scope/party/identity/resolve modules chỉ tách theo thay đổi thực sự cần; business projection/schema, validator và extraction tests.
- **D01 scope:** logical contract/card/front/back/party references; tránh gộp hai contract đều dùng A/B; không gộp card bởi cùng tên hoặc gần nhau. V1 aliases landlord/tenant chỉ khi unique; output đa party opt-in v2, không tự chọn phần tử đầu. Unsupported scope tiếp tục review/refusal nếu corpus chưa đòi mở rộng.
- **Review compatibility:** multiparty/reassociation v2 không dùng ngầm approval logic v1 vốn đòi đúng một landlord/tenant và fixed aliases. V2 approval tắt tới khi có scope-aware policy versioned cùng tests; correction targets gắn stable logical-document/party/field IDs, không array index hay alias có thể đổi người.
- **D02 điều kiện:** chỉ xây model KIE challenger khi B chứng minh lỗi mapping mà rules không đáp ứng. Candidate schema + bounded model call theo scope; Rust dựng/kiểm evidence/hash/type/role/conflict. Document instructions là data; model không có tools/network/DB privileges. Truncation hoặc invalid evidence không cho selected value.
- **Tests:** hai contracts A/B; tên trùng/ID khác; nhiều bên/cards; card chia nhiều blocks; role/native disagreement; table-wrapped anchors; fabricated refs/value/currency; recovery-only selection; instruction text và truncated JSON.
- **Gate/rollback:** giảm đúng error class đã đặt mục tiêu, không tăng critical unsupported/wrong-role selections và nằm trong latency budget. D02 shadow budget nhỏ, không mặc định nhân đôi tải Mac. Profile v1 còn dùng được; không migrate machine result cũ.

## 9. Luồng E — Đo, memory và chứng minh capacity

### E01 — Telemetry và experiment manifest

- **Files:** worker `inference.rs`, `settings.rs`, core metrics, inference-contract tests; startup script chỉ thay sau khi có experiment; sanitized run manifests trong docs, private results ở ignored artifacts.
- **Record:** image/model/weights/runtime/quantization/layout/prompt/preprocess/profile fingerprints; actual W/R/layout pool, server max sequences/pixels/model length; crop dimensions/encoded bytes; genuine optional usage và bounded finish reason. `/v1/models` không attest scheduler config; phân biệt declared và verified startup settings.
- **Tests/gate:** absent/malformed/overflow usage, unknown finish reason, length truncation, cache-hit accounting, shared R across job clones, credential-free errors. Không bịa token count hoặc TTFT từ non-streaming response. Raw layout/text không đổi.

### E02 — Memory trước concurrency

**Trạng thái:** [allocation-owner audit + observer](e02-memory-audit.md) đã kiểm; cgroup peak1,75/2GiB và helper retained khoảng1,58GiB. Chưa có allocator A/B, optimization promotion, representative workload envelope hoặc capacity gate. Không tăng W/R/pool.

- **Files:** profiling tool/report mới; source changes chỉ sau allocation evidence ở preparation/inference/formatting/helper lifecycle.
- **Workload:** 3 synthetic smoke + 6–12 dev documents thấp/trung/cao page-region count và max-size stress fixtures; số mẫu này chỉ tìm vấn đề memory.
- **Measure:** worker cgroup/current/peak/events, helper RSS/CPU/threads, Docker VM, native model memory và host pressure/swap riêng. Không cộng RSS/unified-memory metrics chồng lấn. Trace nguồn, crop/PNG/base64/JSON copies và đồng thời giữ ảnh.
- **Gate:** baseline 1,75/2 GiB chưa đủ headroom thử nghiệm 75–80%; cần giảm live buffers hoặc sizing có số đo trước W/R sweep. Dừng khi OOM, swap tăng kéo dài, lease failure hoặc memory không plateau. Không âm thầm tăng mem_limit/model memory/pool.

### E03 — Bounded arrival-rate runner

- **Files:** `ocr-eval/src/{benchmark,main,lib}.rs`, `arrival.rs` mới; HTTP fixture/scheduler tests và evaluation docs.
- **Modes:** giữ closed-loop; thêm fixed-rate và seeded Poisson, finite duration/max jobs/max upload bytes/max pending submit-poll tasks. Schedule có thể tham chiếu lặp corpus entries đã verify mà không nhân duplicate hashes vào manifest. Mỗi scheduled occurrence có ID/accounting riêng và idempotency key gồm **run ID + occurrence sequence + source fingerprint**; chỉ retry chính occurrence đó dùng lại key. Run+source key một mình sẽ replay job và làm sai throughput. Repetitions không trở thành independent quality samples.
- **Accounting:** scheduled/offered/accepted/rejected/client-skipped-or-late/completed/failed/timed-out/outstanding; actual offered rate và scheduler lag. Client cap không được âm thầm hạ offered load rồi báo đạt target. Accepted jobs sau client timeout vẫn có thể chạy; lưu private IDs và drain deadline, không bỏ chúng khỏi denominator.
- **Metrics:** intake latency, accepted-to-completion, queue/drain time, unfinished jobs; p95 kèm n và warm/cold conditions. Execution goodput khác quality-qualified goodput trên labeled corpus; production sampling mới ước lượng quality, không có online oracle.
- **Tests/gate:** fake HTTP trước: rate/seed/lag/cap, 429/503, slow status, cancellation, jobs hoàn tất sau deadline; no unbounded futures/bytes. W/R metadata không tự thay server; preflight nhận actual deployment manifest.

### E04 — Serial experiments, faults và Mac/GPU decision

Per consumer process model inflight bị chặn bởi **R dùng chung**, không phải W×R. N replicas có thể gửi N×R. Decode slots hiện theo W; layout pool per process, không tự tăng cùng R.

| Experiment | W | Shared R | Model max sequences | Layout pool | Câu hỏi |
| --- | ---: | ---: | ---: | ---: | --- |
| Baseline | 1 | 1 | 1 | 1 | Mốc tham chiếu |
| R2 | 1 | 2 | 2 | 1 | Crop requests độc lập có được batch hữu ích? |
| R4, nếu R2 qua | 1 | 4 | 4 | 1 | Có thêm goodput trong memory/latency budget? |
| W2, sau memory proof | 2 | 1 | 1 | 1 | Chỉ đổi W để đo CPU/layout overlap và fairness; model vẫn chỉ 1 inflight/process |
| W2/R2, chỉ nếu cần | 2 | 2 | 2 | 1 | Document overlap ở cùng inflight cap với R2 |

Chỉ đổi một yếu tố/lần; không gộp crops thành mosaic/prompt nhiều ảnh. Sau concurrency mới thử output tokens/pixels/padding/router từng biến với quality replay. Viable configurations lặp ba lần trên 6–12 dev performance docs sau warmup giống nhau; exploratory percentiles chưa là p95 production.

**Load ladder trong evaluation project/volumes/streams riêng:** smoke → 20–30 jobs dưới saturation → 60–120 phút target arrival → 2× burst tối đa 15 phút + drain → 4 giờ soak → 8–24 giờ final workload cho winning config. Mỗi bước có max total work/bytes/time/stop triggers; không bắt đầu config khác khi còn accepted jobs chưa drain. Long runs chỉ được lên lịch sau khi scope thực thi và giờ chạy đã rõ, không tự tạo automation trong lượt planning.

**Fault gate:** worker restart giữa crop, model timeout/503, Redis/PG interruptions, checkpoint reuse, outbox replay và backlog drain; retry không khuếch đại overload. Tests phá Redis/disk chỉ dùng instance disposable. Reliability fixtures không thay real-model capacity evidence.

**Promotion:** quality gate trước; repeatable benefit đủ bù complexity (≥20% goodput là ví dụ threshold thử nghiệm); deadline/backlog, host+worker memory và lease/retry đạt envelope. Giảm memory hoặc code đơn giản hơn có thể được chọn dù speed gain nhỏ, nhưng ghi rationale.

**GPU gate:** Mac profile tốt nhất đã qua quality vẫn không đạt arrival/deadline/headroom hoặc cost vận hành. Chạy cùng corpus/config accounting trên Linux GPU; chốt SKU từ VRAM/model/batching đo thật. Không hứa GPU scale tuyến tính hoặc chọn phần cứng từ một PDF.

## 10. Luồng F/G — Review có nguồn và acceptance có bằng chứng

### F01 — Real reviewer identity và workflow

- **Files:** API `auth.rs`, routes/session/authorization mới, review types/events migration additive; Rust-served reviewer UI; core/API review tests.
- **Thiết kế:** verified ActorContext (issuer/subject/roles) bên cạnh service principal; browser không nhận privileged service token. Session/CSRF/authorization rõ ràng; không tin actor name/header từ client. Legacy events giữ service-only identity, không gán người giả.
- **UI:** source/page/crop cạnh fields, contract/card alternatives, reason codes và geometry granularity; keyboard workflow/CAS stale edit reload; raw machine state tách correction/approval. Unknown/blank/unreadable không biến thành nội dung đoán.
- **Gate:** forged actor/session expired/CSRF/wrong scope bị chặn; correction→separate approval→reopen giữ audit. Tenant boundary chỉ thêm nếu có unrelated tenants.

### F02 — Human source annotation cho OCR omission

- **Vấn đề:** correction hiện yêu cầu existing field evidence; phần chữ không có trong OCR cần evidence loại người đọc ảnh, không thể bịa machine quote/span.
- **Thiết kế:** versioned human annotation gắn original source SHA, page/real polygon, logical scope, manual transcript, verified actor/time; party/document association correction riêng, giữ assignment gốc. Target gắn stable document/party/field IDs; reassociation invalidates affected corrections/approval để review lại, không chuyển correction sang người khác theo alias/index. Policy v2 phải qua gate D01 trước khi cho approve. Rendered page endpoint bounded/authenticated với transform metadata; annotation không giả machine confidence.
- **Tests/gate:** source hash đổi, polygon ngoài trang, sai field/person, forged machine origin, text omission thật; history/CAS/idempotency; reviewer xử lý được omission mà raw không đổi. Schema v1 vẫn đọc được; new request shape opt-in.
- **Capacity:** đo mean/median/p95 review minutes, agreement, completed reviews/hour, oldest backlog và deadline business approval. Ví dụ 1.000 file × 2 phút là 33,3 giờ công/ngày; staffing là quyết định từ effort đo được, không mặc định đủ người. Machine throughput một mình chưa hoàn thành production workflow.

### G01/G02 — Correctness calibration và policy assessment

- **G01:** offline score/report scaffolding có thể làm sớm. Sau đủ labels mới fit correctness/calibrator từ machine-time features; target gồm value+party+source, thêm document-all-critical. Không dùng self-rating/detector score thay correctness probability. Report reliability/Brier/risk–coverage/subgroups và uncertainty với denominators.
- **G02:** policy/model/normalizer/runtime/dataset khóa phiên bản; chọn threshold trên calibration, test chỉ đánh giá. Conflict/unknown/recovery-only/missing critical/truncation/failed provenance luôn abstain. Lưu assessment artifact riêng; không đổi historical v1 `needs_review`/confidence-null semantics.
- **Gate:** representative independent accepted cases, predeclared risk target và one-sided bound đạt; sample người đã sửa hoặc deliberately enriched challenge không được dùng làm population error estimate. Minh họa 0 unsafe/2.995 accepted độc lập cho bound một phía 95% ≤0,1% đã giải thích trong roadmap; không biến thành yêu cầu đủ với mọi distribution shift.
- **Rollout/rollback:** shadow recommendation → limited opt-in only after gate; cờ autoaccept tắt ngay khi fail; random audit cả accepted-looking cases và drift monitoring. Không đủ data thì dừng ở review bắt buộc, confidence null.

## 11. Verification và quy tắc phát hành chung

Mỗi gói code có tests đúng hành vi thay đổi, chạy targeted tests trước; tới milestone chạy format/strict Clippy/workspace tests và các integration liên quan. Chỉ mở rộng/repeat tests khi changes/failures/concerns mới đòi hỏi. Không ghi lại 151+51 của baseline thành số tests của implementation mới.

Release candidate dùng đúng Docker image sẽ deploy: packaged Rust client + synthetic image/PDF, PG/Redis restricted roles, checkpoint/review/retention compatibility, renderer protocol và nguồn raw. Real PDF quen thuộc là development regression; representative test chỉ mở sau freeze. Phần lớn logic dùng mock model/HTTP; actual-model calls có budget và report riêng.

Mỗi completion report ghi package IDs, files changed, schema/config delta, checks đã chạy, quality/capacity evidence, chưa đạt gì, và rollback. Chỉ gọi complete khi gate thật qua; implementation xong nhưng thiếu nhãn/hardware/off-host backup được ghi trạng thái riêng.

Deployment live chỉ diễn ra sau concrete config/migration/permissions diff, restore result và rollback path. Role/secret changes không áp dụng bằng việc sửa env rồi restart. Destructive retention không chạy cùng migration. Không dùng `docker compose down -v` hoặc restore đè dữ liệu hiện tại.

## 12. Đợt triển khai đầu tiên đề xuất

**Scope đợt 1:** P00 → A01 + A02, song song B01/B02 và E01. Đây là contract/resource/evaluation/telemetry, chưa đổi engine, profile mặc định, concurrency hay retention live.

**Đầu ra review được:** shared bounded response/deadline patch; v2 annotation guide/schema và scorer negative controls; inference telemetry không đổi raw output; compatibility/test report; danh sách dữ liệu cần cho B03. Các gói có cùng file được ghép hoặc thực hiện tuần tự để tránh refactor chồng nhau.

**Sau M1/A03:** A04 safe tooling/DB guards và A05 local restore drill đã qua; tiếp theo A06/O01 trên disposable deployment; B03/F00 chờ independent annotation. E02 allocator experiment cần fresh cgroup và chạy tuần tự. Chọn C/D/E từ error taxonomy và memory report; ước lượng thời gian/hardware chỉ chốt sau feasibility và pilot, không cam kết mọi nhánh nghiên cứu đều phải được triển khai.

Bảng đầu tài liệu là trạng thái hiện tại. Milestone report là bằng chứng tại thời điểm chạy; không dùng kết quả synthetic để đánh dấu real quality/capacity hoặc các gói chưa triển khai là hoàn thành.
