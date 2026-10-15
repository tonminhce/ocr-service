# A04 — Preflight retention/holds

Ngày 15/10/2026. **READ-ONLY REVIEW / PROPOSED — chưa triển khai A04.** Lượt này chỉ đọc code và ghi thiết kế; không thêm migration, hold, policy, lệnh xóa hoặc thay dữ liệu/quyền DB.

Đây là preflight lịch sử trước implementation. Source hiện tại đã triển khai và
kiểm A04 trên deployment cô lập; xem [execution trace](a04-execution-trace.md)
và [hướng dẫn vận hành](retention-operations.md). Các số dòng và câu “chưa tồn tại”
bên dưới mô tả thời điểm preflight. Policy, destructive apply và live cutover
vẫn chưa thực hiện.

A03 đã kiểm thử restricted roles trên deployment tách biệt. **Live M1 chưa cut over sang A03**; không suy ra database live đã được bảo vệ bởi grants mới. Thời hạn giữ dữ liệu, destructive apply, backup/restore và deletion reconciliation vẫn có gate riêng.

| Đường đi hiện tại | Bằng chứng và hệ quả |
|---|---|
| Legacy pruner | [store.rs:662](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/src/store.rs:662): `prune()` chọn terminal jobs theo `updated_at`, khóa `SKIP LOCKED`, xóa tối đa 1.000 rows. Không kiểm hold/policy/review revision. Privileged caller hoặc old worker đang chạy vẫn có thể gọi logic này. |
| A03 runtime | [grants.sql:52](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/administration/grants.sql:52) giữ API/worker/maintenance không có jobs DELETE/TRUNCATE; [worker.rs:125](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-worker/src/worker.rs:125) đã bỏ pruning loop. Đây là bảo vệ của cấu hình A03, chưa phải xác nhận live cutover. |
| Held source có thể bị xóa khi complete | [store.rs:612](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/src/store.rs:612): completion dùng `input = CASE WHEN retain THEN input ELSE empty`. Thêm hold column mà giữ câu SQL này sẽ không bảo vệ original source ở profile không retain. |
| Điểm khóa chung đã có | [store.rs:540](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/src/store.rs:540) khóa job trước fenced write; [review.rs:430](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/src/review.rs:430) cũng khóa chính job trước quyết định review. Hold/release/future delete cần dùng cùng thứ tự khóa. |
| Dữ liệu con bị cascade | [outbox:4](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/migrations/0002_job_outbox.sql:4), [review:3](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/migrations/0004_review.sql:3), [checkpoints:4](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/migrations/0005_region_checkpoints.sql:4) đều `ON DELETE CASCADE`. Xóa job đồng thời xóa source/result, idempotency key và các records này. |

## Gói implementation đề xuất

1. **Migration mới `0006_retention_guards.sql` — chưa tồn tại.** Thêm nullable `retain_until`, policy ID/class, captured review revision và retention revision. Mặc định chưa gán policy/TTL; không lấy `RETENTION_DAYS=7` làm quyết định nghiệp vụ. Thêm append-only hold/release events với opaque hold ID, request ID, revision, service principal, bounded reason và timestamp. Các holds độc lập: release một hold không release hold khác.
2. **Chặn legacy delete ở DB trước khi bật holds.** Giữ runtime/maintenance không có DELETE/TRUNCATE. Thêm DELETE guard từ chối xóa khi apply chưa được mở bằng policy đã chốt; nếu bảo vệ privileged tooling có TRUNCATE thì cần statement trigger riêng vì row DELETE trigger không bảo vệ TRUNCATE. Guard không phải sandbox chống administrator chủ động disable/drop nó. Old worker rollback chỉ hợp lệ khi không thể bỏ qua bảo vệ xóa.
3. **Bảo vệ original source.** Completion mới giữ input khi policy hiện tại hoặc active hold yêu cầu; DB input-update guard ngăn old completion xóa/thay source đang hold. Hold đặt sau khi source đã mất không khôi phục dữ liệu; kết quả phải nói rõ source có còn hay không.
4. **`retention.rs` + admin `plan/hold/release`.** Hold/release khóa job trước, kiểm request replay/CAS, ghi audit và tăng revision trong cùng transaction. Maintenance chỉ được gọi operation hẹp; một fixed-search-path owner function với EXECUTE giới hạn tránh cấp arbitrary retention UPDATE rồi dựa vào caller tự ghi audit. Principal vẫn là service account, chưa phải verified human identity F01.
5. **`apply` giữ disabled.** Không có env toggle, DELETE grant hay destructive execution path trong gói tooling này. `plan` chỉ là snapshot/dry-run có bounds/deadline/cursor, exclusion reasons và counts của phạm vi đã đọc; không gọi một page là tổng toàn DB. Opaque IDs thuộc output vận hành được bảo vệ, không vào metric labels. Báo payload sizes có tên rõ; không gọi đó là số disk bytes sẽ được thu hồi.
6. **Review gần TTL.** Policy tương lai lưu review revision đã xét. Quyết định review mới làm revision đó cũ; future apply phải khóa job rồi kiểm lại latest review/hold/time/policy. Không xóa ngay một approval mới theo plan cũ. Chưa có policy thì không tự chọn một khoảng grace period.

Files cần đổi khi thực thi: migration mới; `ocr-core/src/retention.rs`, `lib.rs`, completion path trong `store.rs`, admin CLI; administration grants/inventory cho objects mới; tests và operations docs. Không rewrite raw OCR khi hold/release; trạng thái retention hiện tại phải nằm ngoài immutable result.

## Verification và gate

| Kiểm chứng cần có | Điều phải chứng minh |
|---|---|
| Old pruner | Restricted credentials bị ACL từ chối; privileged old `prune()` cũng không xóa được do DB guard. Test active, terminal, held và unassigned-policy jobs. |
| Held input | Hold → complete và complete đang chờ khóa → hold đều giữ original bytes. SQL completion cũ bị DB guard chặn; không mất source âm thầm. |
| Hold protocol | Hai holds; release một còn một; duplicate replay; changed payload conflict; stale revision; cancellation/transaction rollback không để state/audit lệch. |
| Review concurrency | Review và hold/release ở cả hai lock orders; fresh review invalidates captured retention revision. Future apply phải kiểm lại sau lock, không chỉ trước khi chờ. |
| Dry-run | Nullable/unassigned policy, queued/processing, due/not due, hold và restored expired fixtures phân loại đúng; deadline/row/output bounds; không có document text trong output/logs. Không xóa dữ liệu. |
| Privilege/audit | API/worker không sửa retention state; maintenance không arbitrary UPDATE/DELETE/TRUNCATE; audit UPDATE/DELETE bị chặn; narrow function không nhận dynamic SQL hoặc caller tự nhận danh tính người thật. |
| Cascade compatibility | Giữ source/review/checkpoint invariants. Chỉ gọi destructive apply/cascade verified khi có execution path được phép và test thực sự chạy qua nó trên disposable DB. |

Các tests hiện chủ động dùng privileged `prune()` ở [store tests:1058](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/tests/store.rs:1058), [review tests:98](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/tests/review.rs:98) và [checkpoint tests:368](/Users/tonminh-mac/Documents/GitHub/ocr-service/crates/ocr-core/tests/checkpoints.rs:368) cần thay expectations khi guard được triển khai. Không giữ automatic delete chỉ để tests cũ xanh; cũng không dùng fixture bypass để tuyên bố production apply đã qua.

**Gate còn mở:** chốt policy/class/retain-until/review semantics; chạy dry-run và race/privilege tests; A05 backup/restore và authoritative deletion reconciliation; review concrete migration/grants/rollback diff trước live cutover. Hold events trong snapshot không phải external deletion ledger: restored DB không tự biết job nào đã bị xóa sau snapshot. A04 tooling hoàn thành cũng chưa đồng nghĩa retention production đã hoàn thành.

Cơ sở PostgreSQL: [row locks giữ tới transaction end và chặn concurrent update/delete](https://www.postgresql.org/docs/18/explicit-locking.html); [SECURITY DEFINER cần kiểm soát search_path và EXECUTE](https://www.postgresql.org/docs/18/sql-createfunction.html); [trigger behavior](https://www.postgresql.org/docs/18/trigger-definition.html). Đây là cơ sở thiết kế, không thay thế các race tests của implementation tương lai.
