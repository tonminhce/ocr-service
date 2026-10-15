# A04 — Trace retention preview, holds và source guards

Ngày 15/10/2026. **IMPLEMENTED / VERIFIED TRÊN DEPLOYMENT CÔ LẬP.** A04 có tooling
giữ dữ liệu và preview; policy/xóa dữ liệu/restore/live cutover vẫn có gate riêng.
Live application và native Qwen vẫn là deployment M1 trước lượt này.
[Hướng dẫn lệnh](retention-operations.md),
[machine-readable verification](research/a04-verification-2026-10-15.json),
[execution plan](production-execution-plan.md).

## Kết quả đã chạy

| Kiểm chứng | Kết quả |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | **209 passed,0 failed;78 ignored chạy riêng** |
| Explicit ignored/integration suite, tuần tự | **78 passed,0 failed,0 ignored** |
| Retention integration trong78 tests trên | **11/11 PASS**, gồm observed PostgreSQL lock waits |
| Administration integration trong78 tests trên | **2/2 PASS**, actual restricted credentials + upgrade/drift negatives |
| Linux ARM64 image `ocr-service:a04` | Build PASS,150,313s |
| Production/smoke và verification Compose | Syntax PASS; không in expanded config |
| Packaged startup | Bootstrap/migrate/grant mỗi thao tác hai lần; schema check và API health PASS |
| Packaged restricted-role E2E | Một synthetic PDF2pages, attempt1, outbox0; hold→complete→release PASS |
| Source/raw integrity | Upload874bytes khớp sau completion và release; raw result4013bytes không đổi |
| File secrets |18files, private parent0700, files0444; API/worker/maintenance UID10001, đúng selective mounts |
| Env/argv/service log scan | Không chứa bộ giá trị secret disposable đã kiểm;15.427bytes service logs |
| Build source + published migrations |106 build-input files không drift;0001–0005 khớp A03 hashes |

209 và78 là hai nhóm không trùng nhau.11 retention và2 administration tests nằm
trong78; không cộng lại targeted reruns hoặc số của M1/A03. Final full-suite lượt
này qua ngay; không phải khẳng định mọi lượt phát triển trước đó đều chưa từng
fail. Hai targeted logs trước final suite được giữ riêng, không cộng vào tổng.
Không chạy lại private PDF, layout helper hoặc actual-model quality benchmark
cho A04. Stub không dùng để chứng nhận accuracy/capacity.

Docker inspect xác nhận Linux/arm64, image ID:
`sha256:26696d17f3674e4b38474e4a8f584155b8b6de30bc97ce3e5345f3fa1dde7620`.
Tag có thể thay đổi; record giữ identity và source fingerprints. Không khẳng
định bit-reproducible builds. Source runtime không đổi sau build; documentation
và verification harness được hoàn thiện sau đó.

## Code path và invariants

| Đường đi | Source để review |
|---|---|
| Nullable metadata, deletion/source/audit guards, narrow owner function | [migration0006](../crates/ocr-core/migrations/0006_retention_guards.sql) |
| Typed validation, redacted errors, bounded plan/hold transaction | [retention.rs](../crates/ocr-core/src/retention.rs) |
| Fenced completion giữ source và truthful metadata dưới cùng job lock | [store.rs](../crates/ocr-core/src/store.rs) |
| Stdin bounds, cursor parsing, apply-before-secret rejection | [retention_cli.rs](../crates/ocr-core/src/bin/admin/retention_cli.rs) |
| Maintenance EXECUTE; runtime không arbitrary retention writes | [grants.sql](../crates/ocr-core/administration/grants.sql) |
| Upgrade-aware guard/function metadata inventory | [dedicated_database.sql](../crates/ocr-core/administration/dedicated_database.sql) |
| Actual races/cancellation/bounds/source assertions | [retention tests](../crates/ocr-core/tests/retention.rs) |
| Restricted credential và catalog/ACL drift tests | [administration tests](../crates/ocr-core/tests/administration.rs) |
| One-shot maintenance, only maintenance DB URL mounted | [production Compose](../compose.production.yaml) |

Hold/release khóa parent job trước replay/CAS, active-state mutation, counter và
append-only event. Review và completion đã dùng parent job lock; races kiểm cả
hai thứ tự. Revision retention độc lập review revision. Audit lấy `session_user`,
không nhận actor do caller tự khai; đây là DB service principal, chưa phải human
identity provider. Owner function dùng qualified tables và fixed search path với
`pg_temp` cuối, PUBLIC EXECUTE bị revoke ngay migration transaction. Cách thiết kế
theo [PostgreSQL SECURITY DEFINER guidance](https://www.postgresql.org/docs/18/sql-createfunction.html)
và [row-lock lifetime](https://www.postgresql.org/docs/18/explicit-locking.html).

Hai holds độc lập cần hai releases. Identical request replay trả snapshot của
mutation gốc, gồm count/source presence lúc đó; không giả làm current state.
Changed fingerprint/principal hoặc stale revision bị conflict. Max128 active
holds và10.000 lifetime events/job; mỗi admitted hold giữ sẵn một event slot cho
release. Đạt event cap vẫn replay được event cũ. Không có audit trimming.

DB chặn mọi jobs DELETE và TRUNCATE; audit UPDATE/DELETE/TRUNCATE cũng bị chặn.
Held-input trigger dùng **OLD locked target row**, để legacy UPDATE đang chờ một
hold chưa commit không đọc nhầm trạng thái cũ từ statement snapshot. Modern
completion giữ input khi normal policy hoặc active hold yêu cầu; chỉ khi result
đã có `source.original_retained` thì cập nhật property đó theo bytes thật trong
cùng UPDATE. Generic JSON không có property giữ nguyên. Raw OCR, layout,
extraction và evidence không bị sửa bởi hold/release/review.

Hold sau khi source đã mất trả `source_present=false`; không có recovery giả.
Release không xóa bytes hoặc rewrite raw metadata. DB owner vẫn có thể disable
hay drop guards: đây không phải sandbox chống administrator.

## Trace concurrency và negative tests

Các dòng dưới là observations/assertions từ final integration log, không chỉ
đọc code rồi suy đoán scheduling:

| Scenario thực thi | Kết quả quan sát |
|---|---|
| Hold giữ parent lock → modern completion | `pg_stat_activity` thấy completion chờ; source giữ nguyên, metadata true |
| Uncommitted hold → legacy clear-input UPDATE | Thấy lock wait; sau hold commit, UPDATE bị held-source guard chặn |
| Legacy completion giữ lock → hold | Hold chờ; completion bỏ source trước, hold sau báo source absent |
| Hai requests cùng expected revision | Một mutation thắng; request cạnh tranh conflict; không lost update |
| Cùng request retry sau hold/release trong integration test | Một event cho request gốc; replay vẫn revision1/count1, current state đã revision3/count1 |
| Hold → review | Actual review chờ parent lock; review revision và raw result đúng |
| Review → hold và review → release | Actual `append_review` paused tại COMMIT bằng test-only deferred trigger/advisory barrier; thấy COMMIT barrier và retention parent-lock wait trước khi thả |
| Abort sau function nhưng trước COMMIT; abort khi chờ lock | State/audit rollback,0 leaked events, cả6pool connections phục hồi; retry revision1 |
|128 active holds và reserved release capacity | Over-cap rejected; revision9998 có2holds, release tới10000/0holds, không strand hold |
| Legacy privileged prune / direct DELETE / TRUNCATE | Guard từ chối; jobs/source/review/checkpoint rows còn nguyên |
| Audit UPDATE/DELETE/TRUNCATE; held source clear/replace | Bị DB guards từ chối |
| API/worker/maintenance direct retention writes; runtime function call | Restricted credentials bị ACL từ chối; maintenance narrow call hợp lệ |
| NULL/invalid SQL request, invalid/unknown CLI fields | Fixed/redacted failure; không partial write |
| Catalog drift và PUBLIC/table/column/function ACL drift | Inventory reject metadata sai; grants reconcile ACL drift đã biết |

Cancellation coverage là **trước COMMIT**. Sau khi COMMIT đã gửi, timeout/disconnect
có thể không nói rõ transaction đã commit hay chưa. Retry cùng request ID và
fingerprint để reconcile; chưa mô phỏng mọi commit outcome. Inventory kiểm
provenance/configuration/signature/return/security/path/trigger bits, chưa attest
toàn bộ function body hoặc physical schema.

## Bounded preview và hiệu năng

`plan` tối đa100 rows, default50, `(created_at,id)` keyset có index. Hai SQL
predicates cố định giữ continuation tuple indexable khi dùng generic prepared
plan.10s total operation gồm pool/transaction,2s lock và5s statement budgets;
CLI stdin tối đa64KiB và10s total read budget. Không thêm uncancellable input
thread. Không scan toàn checkpoint/audit payload để tính bytes.

Mỗi page là một read-only repeatable-read snapshot, database `snapshot_at`;
không gọi counts của page là tổng DB hoặc snapshot nhất quán xuyên nhiều pages.
Fixture4rows/page2 qua hai pages không trùng/mất rows khi created_at bằng nhau.
Review mới làm captured review revision cũ và bị exclusion. `metadata_due` chỉ
nói metadata predicates thỏa; **apply vẫn false**. Empty preview cũng lấy thời
gian từ DB. [PostgreSQL isolation semantics](https://www.postgresql.org/docs/18/transaction-iso.html).

Output không chứa filename/document text/source bytes/audit reasons. Opaque IDs
vẫn là operational data cần bảo vệ. `source_payload_bytes` là logical bytea
length; `result_stored_datum_bytes` là `pg_column_size`, có compression/TOAST
semantics. Không phải disk bytes reclaimable. Chưa benchmark large-table plan
latency hoặc hơn1.000docs/day; index/bounds PASS không thay capacity proof.

## Packaged Docker trace

Project `rust-ocr-a04-smoke`, API8380, protected metrics9391/9392; PG/Redis không
publish host port. Mới hoàn toàn, secrets/volumes riêng. Strategy explicit
`full_page_vlm`, không extraction, Rust model stub. Normal profile này sẽ clear
source khi complete; remaining hold làm source được giữ trong scenario đã chạy.

```text
bootstrap ×2 → API schema check trước migration: exit1/missing history
migrate ×2 → grant ×2 → đúng6 migration rows
A03 image check trên schema6: exit1/unsupported migration6
A04 image schema check: exit0 → API-only healthy; worker chưa start

POST /v1/ocr/jobs                         202 + Location đúng
GET  .../status                           200 queued
POST cùng Idempotency-Key                 202 cùng job
maintenance hold H1                      revision1/count1
maintenance hold H2                      revision2/count2
replay H1                                revision1/count1/replayed=true
maintenance release H1                   revision3/count1
plan                                     held + nonterminal + unassigned + apply_disabled
start Rust stub + worker                 Redis Streams → SQL lease →2 model calls → complete
GET  .../status                           200 succeeded, attempt1
GET  result/source/markdown               200, private/no-store + nosniff
source                                   exact874 upload bytes; original_retained=true
plan                                     held + unassigned + apply_disabled
review bằng API token                    401
review bằng review token, no extraction  404 expected; không tạo event
maintenance release H2                   revision4/count0
plan                                     unassigned + apply_disabled
GET result và source lần nữa              exact bytes không đổi
apply với missing và invalid DB URL      exit1/apply disabled; --network none

SQL: jobs1/succeeded1/attempts1/source874bytes
     revision4/activeholds0/holdrows0/events4/actor=ocr_maintenance
     reviewevents0/outbox0/migrations6
```

Review concurrency/replay thành công thuộc real DB integration tests; packaged
fixture cố ý không có extraction nên review đúng contract là unavailable. Không
gán review event cho fixture đó. Terminal observation sau worker start1,046s chỉ
là stub orchestration, không phải model throughput/OCR latency.

Selective inspection xác nhận API chỉ mount4 secrets của nó, worker4,
maintenance1 DB URL; cả ba UID10001, read-only rootfs, cap-drop ALL,
no-new-privileges. In-memory scan so bộ secret mới với env/argv và captured service
logs, không in full inspect/env hoặc secret values. Chưa attest external audit
extensions, proxies, backup encryption hay mọi secret của máy.

## Review, cleanup và gates còn lại

Review chéo đã sửa ba lỗi thiết kế trước final verification: event cap phải giữ
slot release cho mọi active hold; plan cần đúng scan index/keyset predicate;
snapshot clock phải từ DB, kể cả page rỗng. Review cũng yêu cầu metadata source
completion phản ánh hold dưới cùng khóa và race review-first thật. CLI help đã
bỏ ký tự `+` thừa; README/operations phân biệt current A04 và historical A03.
Không đổi model, concurrency, retry/lease semantics hoặc raw OCR để làm tests qua.

Final independent audit: PASS, không còn material mismatch. Đã đối chiếu
106 source hashes,63 evidence size/SHA-256 checks,209+78 test totals,12 trace
lines và87 local links. In-memory scan70 evidence/report files với18 disposable
secret values và7 decoded URI passwords không thấy leak. Image/start state của
bốn live containers khớp observation; test containers đã vắng, hai smoke volumes
còn nguyên. Reviewer không sửa source/runtime hoặc chạy lại tests.

Chỉ dừng containers/networks của hai verification projects do lượt này sở hữu.
Hai A04 smoke volumes và private secrets/evidence giữ để audit; không dùng `-v`,
không chạm application volumes/live credentials. Không commit/push/reset worktree.

Các gate còn mở:

- A05 local backup/restore drill, off-host encrypted destination/key policy,
  RPO/RTO và authoritative post-snapshot deletion reconciliation.
- Explicit retention class/deadline/review grace/historical backup/hold-audit
  policy; không tự dùng `RETENTION_DAYS` làm TTL. `apply` chưa có runnable delete
  path, environment toggle hoặc DELETE grant.
- Live adoption và O02 migration/rollback rehearsal. Old binary không chạy trên
  schema6; không down-migrate để xóa audit.
- Independent labels và live v2 exporter, human review identity, parser isolation,
  allocator A/B và measured capacity/headroom. Confidence giữ null khi chưa
  calibration; conflict không tự chọn; automatic acceptance vẫn tắt.

A04 verified không đồng nghĩa toàn OCR service đã production certified.
