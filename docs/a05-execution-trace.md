# A05 — Trace backup mã hóa, full restore và Redis-empty recovery

Ngày 15/10/2026. **IMPLEMENTED / VERIFIED LOCAL ISOLATED.** Phạm vi hoàn tất là
Rust tooling và diễn tập PostgreSQL18 logical backup/restore trên Docker ARM64.
Backup production, RPO/RTO, off-host, PITR, deletion reconciliation và live
cutover còn gate riêng. Hệ thống live M1 và native Qwen không bị thay đổi.
[Runbook](backup-operations.md), [verification record](research/a05-verification-2026-10-15.json),
[kế hoạch](production-execution-plan.md).

## Kết quả kiểm chứng

| Kiểm chứng | Kết quả |
|---|---|
| Workspace format và strict Clippy all-targets | PASS |
| Ordinary workspace suite | **227 passed, 0 failed; 86 ignored chạy riêng** |
| Explicit ignored suite, tuần tự, PG18 + Redis riêng | **86 passed, 0 failed, 0 ignored** |
| Trong86: snapshot manifest / restore target / administration | 5 / 3 / 2 tests PASS; các subset này không cộng lại |
| Backup binary unit tests trên Mac | 16 PASS, gồm10 process tests,4 config,2 orchestration |
| Backup binary trên Linux release image | **17 PASS**, thêm ENOSPC/sync-failure test |
| Three Docker ARM64 image builds | PASS; app3,632s, ops0,612s, rehearsal166,960s nhờ shared build cache |
| Actual restricted-role `pg_dump` → age → offline verify | PASS; không plaintext dump file ở bước create |
| Full restore trên hai fresh PG18 volumes | Cả hai content manifest và backup ID khớp receipt nguồn |
| Archive TOC | 7 TABLE DATA,6 FUNCTION,7 application TRIGGER |
| SQL recovery với natural expiry | Delayed/active/dispatch giữ đúng hạn; stale tokens bị chặn; attempts giữ nguyên |
| Rust worker + fresh authenticated Redis + Rust model stub | 6 succeeded,1 failed ở attempt3; outbox0; retained checkpoints2 |
| Redis sau drain | `entries-added=7`, length0, groups1, pending0 |
| Source/result/review/hold preservation | Snapshot exact; completed raw hash trước/sau worker không đổi; audit1/review1/hold1 |
| Target runtime isolation trước explicit synthetic-only grant | API SELECT=false, worker SELECT=false, PUBLIC function EXECUTE=false |
| Runtime env/argv/service-log secret scan | PASS,4containers/19generated values/26.255bytes logs |

227 ordinary và86 explicit integration là hai tập không trùng.17 Linux tests là
kiểm cùng binary ở nền tảng khác; không cộng vào workspace total. Targeted reruns
và các mốc M1/A03/A04 là bằng chứng lịch sử, không cộng dồn. Đây không phải
benchmark OCR, accuracy, large-DB restore hoặc workload1.000+docs/day.

Final packaged run `rc0c5b7721b` có60 recorded steps gồm một auxiliary TOC failure
đã sửa và kiểm lại riêng. Không gọi60steps là60independent tests. Evidence riêng:
`artifacts/a05-verification/rc0c5b7721b/verification-completed.json`; bản lỗi ban
đầu, stdout/stderr và3lượt harness trước vẫn được giữ. Artifact directory bị Git
ignore và parent0700; không publish keys, receipts, dump hay service logs.

## Đường đi và review source

| Đường đi | Source |
|---|---|
| Strict CLI, narrow TCP URI, inherited PG environment gate, private passfile/key/tmpfs | [config.rs](../crates/ocr-core/src/bin/backup/config.rs) |
| Exported snapshot → standard tools → seal; verify-before-connect → full restore → compare | [ocr-backup.rs](../crates/ocr-core/src/bin/ocr-backup.rs) |
| Bounded streaming/hashing, process groups, failure/cancel cleanup, flush/sync | [process.rs](../crates/ocr-core/src/bin/backup/process.rs) |
| 61 persisted columns/7 table roots +6 function definitions/owners | [backup.rs](../crates/ocr-core/src/backup.rs) |
| Actual migrator login, empty target/default ACLs, retained session lock; migrate coordination | [administration.rs](../crates/ocr-core/src/administration.rs) |
| Same-snapshot concurrency, all61column mutations, schema/catalog negatives | [manifest tests](../crates/ocr-core/tests/backup_manifest.rs) |
| Empty/identity/role/GUC/ACL drift và cooperative locks | [target tests](../crates/ocr-core/tests/restore_target.rs) |
| Synthetic-only storage/recovery assertions, no runtime inclusion | [rehearsal helper](../crates/ocr-core/examples/backup_rehearsal.rs) |
| One-shot restricted backup/offline verifier, separate quarantine restore | [backup Compose](../compose.backup.yaml), [restore Compose](../compose.restore.yaml) |
| PG18 + pinned age1 operations target, separate rehearsal target | [Dockerfile](../Dockerfile) |

Published migrations0001–0006 không sửa. Default production example image chuyển
thành `ocr-service:a05`; đây là thay đổi file cho deployment mới, không recreate
live containers. Application runtime image không chứa `ocr-backup`, age hoặc
rehearsal helper. Ops image không chứa helper; chỉ target rehearsal chứa helper.
Mọi actor DDL cần dùng binary A05 để tham gia khóa hợp tác mới.

## Snapshot, integrity và bounded work

Create dùng actual `ocr_backup` login, một SQLx-tracked REPEATABLE READ READ ONLY
transaction và exported snapshot. Manifest và `pg_dump --snapshot` đọc cùng
snapshot đang mở. Shared operations key3 phối hợp với exclusive migrate/
bootstrap/grant; đọc bảng giữ ACCESS SHARE tới hết exporter transaction. Điều
này không ngăn administrator bypass lock hoặc sửa DDL ngoài tooling.
[PostgreSQL snapshot option](https://www.postgresql.org/docs/18/app-pgdump.html).

Manifest hash đủ61cột, gồm source bytes, result JSONB, review/retention events,
holds, checkpoints, outbox tokens/timestamps và migration history. Hash row dùng
PG18 JSONB text với UTC/ISO/bytea/float settings cố định, framing và PK order;
client stream fixed-size32byte row digests thay vì gom toàn bảng. Jobs source
và result hash riêng để tránh giant hex copy của input. Client memory O(1) theo
số rows, nhưng DB vẫn đọc/hash toàn bộ logical content; JSONB/TOAST có chi phí.
Max10millionrows/table,120s manifest,30s statement,2s lock. Đây là giới hạn cho
local tooling, chưa đo được DB lớn nào đáp ứng chúng.

Manifest không trả document text, filenames, per-job IDs hoặc row hashes. Sáu
function roots gồm body và owner để chứng minh preservation; không là attestation
against known-release source hay toàn bộ physical schema. Unknown large objects,
publication/subscription, language/transform và các catalog objects ngoài phạm vi
được từ chối thay vì bị bỏ ngoài roots một cách âm thầm.

Create stream standard custom/zstd dump qua age với64KiB buffers, separate
plaintext/ciphertext bounds và process deadlines. File `.partial` không hợp lệ;
manifest cũng mã hóa, completion seal xuất bản cuối và fsync directory/parent.
Receipt hash phải giữ độc lập. age không xác thực sender; adjacent seal không
đủ provenance. [age documentation](https://github.com/FiloSottile/age).

Verify không cần DB/network. Nó kiểm externally pinned manifest cipher hash,
decrypt/validate manifest, hash archive qua O_NOFOLLOW regular FD, rewind và đưa
chính FD đó vào age. Toàn bộ plaintext được giải mã vào private Linux tmpfs,
kiểm byte/hash và age exit0 trước restore. Không stream trực tiếp vào SQL.
Tmpfs có thể swap; không hứa plaintext tuyệt đối không chạm disk/VM storage.

Restore yêu cầu PG18 tools/server, actual migrator, explicit database name,
fresh-bootstrap roles/default ACLs, empty supported inventory và không backend
khác trong target lúc preflight. Session lock key5 giữ qua restore+verification.
Đây là cooperative isolation, không chống privileged concurrent admin.
`pg_restore --dbname ... --single-transaction --exit-on-error --no-owner
--no-acl --role=ocr_owner` tạo schema, COPY data, rồi post-data constraints và
triggers. Không pre-migrate target, không disable audit triggers trên một schema
đang tồn tại. [PostgreSQL restore options](https://www.postgresql.org/docs/18/app-pgrestore.html).

Hai lần full restore tái tạo chính xác outbox6rows, checkpoint2rows,
review/hold/retention event mỗi loại1 và6migration rows. Chỉ so manifest trước
bất kỳ recovery nào vì recovery thay durable timestamps/attempts hợp lệ.
Runtime vẫn không có object grants sau thành công. SQL có thể đã COMMIT trước
timeout/lỗi postcheck; target phải giữ quarantine và inspect, không tự drop,
clean, migrate, grant hay activate để “thử lại”.

## Trace diễn tập

```text
source fresh volume: bootstrap → migrate → grant → synthetic7jobs
completed1 + queued3 + processing3; outbox6/checkpoints2
retained PDF bytes + raw/extraction + review rejection + hold/event
random256KiB padding giúp archive có nhiều age chunks

create actual ocr_backup login             0,473s
  pg_dump PG18 + age1.2.1, plaintext170.997bytes/ciphertext171.229bytes
offline verify --network none              0,289s
fresh target1: bootstrap ONLY → full restore0,405s → exact snapshot check
  initial delay/worker/dispatcher suppression: PASS
  expired lease stale writes: rejected; new token +attempt2; checkpoint reused
  final exhausted lease: failed at3; no fourth attempt
  natural expiry wait ≈41s; timestamp rewrites0/attempt resets0
  terminal checkpoint readers fenced; rows retained2
fresh target2: bootstrap ONLY → full restore0,449s → manifest exact
  runtime SELECT false/PUBLIC EXECUTE false
  explicit SYNTHETIC-ONLY reviewed grant; fresh Redis DBSIZE0; start stub+worker
  durable outbox → Redis Streams → fenced SQL claims → render2pages/job → complete
  final succeeded6/failed1/attempts_sum12/outbox0/checkpoints2
  completed raw SHA256 unchanged; review1/hold1/event1
  Redis entries-added7/length0/pending0; finalfailed attempts3
stop/remove only random rehearsal project containers; preserve volumes/evidence
```

Times là client wall time gồm one-shot container startup, trên7tiny synthetic
jobs/một snapshot. Chúng không là RPO/RTO, sustained latency hay OCR model speed.
Worker dùng canned Rust model stub và explicit full-page strategy. Không chạy
lại private contract PDF hay actual Qwen/layout quality benchmark trong A05.

## Negative tests và findings đã xử lý

| Scenario đã chạy | Outcome |
|---|---|
| Wrong identity / untrusted manifest pin / modified cipher / missing seal | exit1 trước destination credential loading/connect |
| Truncated final age chunk với manifest cipher digest được tạo lại có chủ ý | Full age exit1; direct control chứng minh đã xuất131.072plaintext bytes; CLI không tới DB |
| Private tmpfs64KiB đầy | Redacted stream/sync failure; không restore SQL |
| Create max100bytes | exit1, `.partial` có thể còn; không complete seal |
| Create trùng output directory | exit1; bundle cũ không overwrite |
| Populated restore target | exit1; không clean/drop hoặc grant |
| Unit tool deadline/cancellation, failure hai pipeline stages, descendants | Process groups bị kill; direct children reaped; no successful digest |
| Linux `/dev/full` và sync failure | Không trả thành công |
| RR/RO thiếu, concurrent writer,61field mutations | Gates fail đúng; same exported snapshot không thấy writer commit sau snapshot |
| Restore wrong DB/login, role/GUC/ownership/default-ACL drift, other session/lock | Rejected; repair từng drift case rồi positive accept |
| Shared backup operations lock giữ key3 → migrate | Migrate bị chặn trước tạo history; lock released sau success/checksum-error |

Review tìm và sửa: SQLx kế thừa PG environment khác child tools; missing explicit
`pg_restore --dbname`; age version format có/không `v`; hash finalization;
archive pathname reopen; unsupported source large objects; project-name/healthcheck
target-session collisions. Completion timestamp dùng DB clock nguồn.

Các lỗi phát triển được giữ trong evidence: strict Clippy yêu cầu collapse/range
style; close-session lock test cần chờ server nhận Terminate; synthetic
subscription cleanup phải `slot_name=NONE`; helper trước đó đếm sai checkpoints
(completion giữ rows, terminal reads bị fence). Harness có các lỗi YAML comma,
Docker stdin, XLEN sau XACK+XDEL, Redis JSON shape và TOC early pipe close. Runtime
không sửa để chiều các assertions sai đó. Auxiliary TOC sửa cuối được test riêng
trên cùng archive, không rerun broad suite khi không có source/runtime change.

## Gate còn lại

- Local encrypted volume/keys chưa bảo vệ khỏi mất host. Cần off-host destination,
  independent provenance/key escrow/rotation/restore access và backup retention.
- Logical snapshot không PITR; chưa có production RPO/RTO hay policy đã nghiệm thu.
- Phải reconcile post-snapshot deletion/retention từ nguồn authoritative ngoài
  snapshot trước activation. Restored DB tự nó không biết các deletion sau đó.
- Known-release full schema/body attestation, release rollback, dependency/SBOM
  security checks và larger/corrupt-target/capacity workloads vẫn là gate riêng.
- Independent labels, live v2 adapter, quality/calibration/reviewer identity,
  parser isolation và Mac workload>1.000docs/day chưa tự hoàn tất nhờ A05.

Giữ raw OCR/evidence bất biến, conflicts unselected, uncalibrated confidence=null
và automatic acceptance off. Không có destructive retention enable flag.
