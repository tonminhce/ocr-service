# A03 — Trace thực thi, phân quyền DB và file secrets

Ngày 15/10/2026. Trạng thái: **IMPLEMENTED / VERIFIED TRÊN DEPLOYMENT CÔ LẬP**.
A03 không phải chứng nhận hoàn thành toàn bộ roadmap production. Stack đang chạy
với dữ liệu development vẫn là M1; không adopt DB, rotate credential hay cut over
live trong milestone này. [Hướng dẫn vận hành](a03-operations.md),
[kế hoạch và gates](production-execution-plan.md).

## Kết quả kiểm chứng

| Kiểm chứng | Kết quả cuối |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | **205 passed, 0 failed; 67 ignored chạy riêng** |
| Explicit ignored/integration suite, tuần tự | **67 passed, 0 failed, 0 ignored** |
| Linux ARM64 Docker image `ocr-service:a03` | Build PASS; packaged startup và restricted HTTP flow PASS |
| Production/smoke/verification Compose | Syntax PASS; không in expanded config |
| Role bootstrap / migration / grant | Mỗi thao tác chạy hai lần; không rotate hoặc nhân đôi history |
| Runtime schema guard trước migration | API từ chối startup trước khi mở listener |
| Restricted API + worker + Redis + PG18 | Một synthetic job hai trang succeeded, attempt 1, outbox 0 |
| Review và replay | API token bị 401; review token tạo đúng một event/revision 1 |
| Raw/source integrity | Original upload bytes khớp; raw result bytes trước/sau review bằng nhau |
| Secret delivery thực tế | Nonroot UID10001 đọc được file; chỉ mount đúng secret cần dùng |
| Env/argv/stdout scan | Không chứa giá trị của bộ secret disposable được kiểm tra |
| Redis secret byte validator | **15/15 PASS**:2 valid accepted,13 malformed rejected; pinned-image/network-none fixtures |
| E02 observer probe regression | **7/7 PASS**; actual idle observer smoke ghi riêng ở E02 report |

205 và 67 là hai nhóm không trùng nhau. Không cộng các targeted rerun hoặc M1
191/65 vào chúng. Các shell/Python observer tests được ghi riêng, không tính vào
tổng Rust. Máy-readable evidence và log hashes ở
[verification record](research/a03-verification-2026-10-15.json).

Image inspect trả Linux/arm64 và ID
`sha256:d7a7b7c86c23f362cb8f94f0d1e596adebaa2d03639c32f787f07d49fa5bd2b8`.
Build log còn ghi config digest
`sha256:42d7951a784ce15018be6d381a020d44438258d5af1050de1b50bff24da943f2`.
Sau build chỉ có thay đổi test recovery, Redis launcher bind-mounted và docs;
không có thay đổi Rust runtime trong image. Đây không phải khẳng định build
bit-reproducible của mọi file hiện có trong worktree.

## Trace startup và quyền thực thi

| Code path để review | Vị trí |
|---|---|
| API/worker startup không tự DDL | [API main](../crates/ocr-api/src/main.rs#L27), [worker main](../crates/ocr-worker/src/main.rs#L19) |
| Exact bounded read-only history check | [schema::check](../crates/ocr-core/src/schema.rs#L29) |
| Bootstrap → migrate → grant | [administration.rs](../crates/ocr-core/src/administration.rs#L66), [SQL ACLs](../crates/ocr-core/administration/grants.sql) |
| Env/file resolution và safe DB URI | [secrets.rs](../crates/ocr-core/src/secrets.rs#L68), [postgres.rs](../crates/ocr-core/src/postgres.rs#L28) |
| Restricted credential trace/negative tests | [administration integration](../crates/ocr-core/tests/administration.rs#L46) |
| Honest source retention metadata | [worker.rs](../crates/ocr-worker/src/worker.rs#L358) |
| Redis byte-level validation | [redis-start.sh](../ops/redis-start.sh), [focused fixture harness](../scripts/test_redis_secret_validation.py) |

```text
new isolated PG18/Redis volumes
  → bootstrap ×2 (admin, additive roles, password giữ nguyên)
  → API startup trước migration: reject missing schema
  → migrate ×2 (ocr_migrator → SET ROLE ocr_owner)
  → đúng 5 migration rows, không sửa 0001–0005
  → grant ×2 (admin, reconcile explicit ACLs)
  → schema check bằng API credential: PASS
  → API/worker UID10001 + read-only rootfs + file secrets
```

API/worker chỉ kiểm exact migration version, success và checksum trong một
read-only repeatable-read transaction. Query/lock/total deadlines là 5/2/10 giây.
Missing, pending, dirty, unsupported và checksum mismatch đều bị từ chối; runtime
không tự chạy DDL. History match không chứng minh mọi live column/function đều
chưa bị một administrator sửa.

| Principal | Quyền đã kiểm bằng credential thực |
|---|---|
| `ocr_owner` | NOLOGIN, sở hữu objects |
| `ocr_migrator` | Login riêng; explicit SET ROLE owner để migrate |
| `ocr_api` | Submit/read, append review; UPDATE chỉ `updated_at` để row lock |
| `ocr_worker` | Lease/heartbeat/retry/complete, outbox, insert/read checkpoint |
| `ocr_maintenance`, `ocr_backup` | Read-only trong A03; chưa có deletion operation |

Restricted integration test đi qua **submit → invoker outbox → dispatch → lease →
heartbeat → immutable checkpoint → retry/outbox → complete → review/replay**.
Negative tests thực thi và bị từ chối: CREATE ROLE, DDL/temp table, disable trigger,
SET ROLE owner, sửa migration history, sửa/xóa/truncate review audit, xóa/truncate
jobs. Worker không insert review; API không update result/input/status.

Outbox/audit functions giữ SECURITY INVOKER với fixed search path. Review cần
`SELECT FOR UPDATE`; column grant hẹp cho `updated_at` đáp ứng yêu cầu PostgreSQL
mà không cho API sửa raw/result/status. Admin direct audit UPDATE/DELETE cũng bị
append-only trigger từ chối độc lập với ACL negatives.
[PostgreSQL privileges](https://www.postgresql.org/docs/18/ddl-priv.html).

Bootstrap/grant chặn role attributes/membership/ownership bất ngờ, role/database
GUC overrides và inventory không hỗ trợ trước mutation. Kiểm thử có schema khác,
unknown owner-owned table/type/function/index, trigger bị đổi/disable và RLS/policy.
Reconciliation xóa table/column/default ACL drift đã biết, gồm default PUBLIC
EXECUTE ở mức global. Role names cố định theo cluster; phải dùng dedicated cluster,
không dùng inventory này để adopt shared DB.
[Default privileges](https://www.postgresql.org/docs/18/sql-alterdefaultprivileges.html).

## Packaged HTTP trace: protocol/permissions fixture

Project `rust-ocr-a03-smoke` dùng standalone production Compose cùng explicit
smoke override. API 8280, protected metrics 9291/9292, PG/Redis không publish port.
Model là Rust stub; strategy **full_page_vlm** để test quyền/protocol mà không
load thêm layout model hoặc chạy Qwen trên host đang có nhiều swap.

```text
POST /v1/ocr/jobs                202, Location đúng
GET  .../status                 200, succeeded
POST cùng Idempotency-Key       202, cùng job
GET  .../artifacts/result       200, private/no-store + nosniff
GET  .../artifacts/markdown     200, hai trang fixture
GET  .../artifacts/source       200, exact upload bytes
GET  .../business               200
GET  .../artifacts/layout       404, expected cho full_page fixture
POST .../review với API token   401
POST .../review với review token 200, rejected/revision 1
POST replay cùng review request 200, revision vẫn 1
GET  .../artifacts/result       200, bytes không đổi
SQL: jobs=1 succeeded=1 attempts=1 outbox=0 review_events=1 migrations=5
```

Đây **không phải** proof layout detection hoặc OCR accuracy. Course pipeline vẫn
là mặc định của production file; actual-model/layout regression thuộc
[M1 report](m1-execution-review.md). Không chạy lại private PDF trong A03 và không
dùng canned stub text làm chứng nhận chất lượng.

Harness ban đầu có hai assertion sai: chỉ chấp nhận exact `no-store` thay vì
`private, no-store`, và đòi layout artifact trong explicit full-page profile.
Đã sửa harness theo contract, không sửa API để đáp ứng assertion sai. Cùng
idempotency key được giữ xuyên các lần chạy; SQL xác nhận chỉ một job. Do final
harness quan sát replay của job đã xong, thời gian 0,0508 giây là **replay đến quan
sát terminal**, không phải initial OCR end-to-end latency và không dùng để sizing.

## Secret và log review

Shared loader đọc một trong `NAME` hoặc `NAME_FILE`; hai nguồn cùng tồn tại luôn
fail, kể cả direct empty. Optional direct empty giữ compatibility; empty file
fail. Cap 64KiB, bỏ đúng một LF/CRLF cuối, giữ spaces, từ chối UTF-8/control/newline
không hợp lệ. Unix mở với O_NOFOLLOW/O_NONBLOCK, kiểm opened FD regular để tránh
final symlink/FIFO; parent symlink vẫn được phép. Error/Debug không chứa value,
path hoặc raw I/O source. File được đọc một lần lúc startup, chưa có live rotation.

Provisioner `ocr-secrets init` dùng OS randomness, không overwrite directory,
không in giá trị. Đã tạo thật dưới umask077: directory0700, 18 files0444. File mode
0444 là chủ ý để UID10001 đọc được Compose bind-secret; host access được bảo vệ
bằng private parent. Đây là plaintext file delivery, chưa phải encryption hoặc
managed secret store. API/worker chỉ nhận đúng bốn secret của chúng, không có
admin/migrator/maintenance/backup credential.
[Compose file secrets](https://docs.docker.com/compose/how-tos/use-secrets/).

Deep review phát hiện SQLx0.8.6 log **unknown URL parameter values ngay trong
parser**, trước khi có thể disable statement logging. `postgres::safe_options`
validate URI/query-key allowlist trước driver, bỏ parser cause và disable SQL logs.
Store/admin connection errors cũng trả fixed error, không raw PostgreSQL message.
Tests có trace control và sentinel cho invalid options/unknown key/upstream error.

Bootstrap không tạo client SQL chứa plaintext password: bound transaction-local
settings + static server block format identifier/literal. Các PostgreSQL logging
knobs được hạn chế transaction-locally trước credential path. Test cố ý bật verbose
server logging và gây CREATE ROLE lock-timeout: log capture78.995 bytes không có
sentinel/synthetic passwords/password DDL. Hash được giữ trong machine record.
Điều này không attest cấu hình external pgaudit/proxy; operator phải kiểm logging
ở các lớp đó. [PostgreSQL logging](https://www.postgresql.org/docs/18/runtime-config-logging.html).

Redis password được đọc từ file và ghi tmpfs config0400; không đưa password vào
argv hoặc container env. Final peer review tìm terminal NUL bị BusyBox `read` bỏ
qua; launcher được sửa để validate bytes toàn file trước khi parse. Focused
regression15/15 PASS gồm terminal/embedded NUL, CR, extra lines, invalid hex,
spaces và valid64hex/có terminal LF. Harness dùng file bất biến riêng mỗi case
sau khi một lượt đầu dùng lại file host cho các bind mounts có observation CR
không nhất quán; hai lượt cuối với fixtures tách riêng đều15/15 PASS. Không coi
observation đó là chứng cứ parser production chấp nhận CR. `--validate-only`
chạy đúng validator rồi thoát trước config/server. Exact final kết quả và file
hashes được ghi trong machine record. Healthcheck NOAUTH chỉ chứng minh auth
gate; E2E job mới kiểm authenticated queue IO. Sau fix đã recreate **chỉ Redis
disposable** bằng launcher cuối: health healthy, authenticated PING qua stdin file
trả PONG, captured output không có secret. Không chạy lại model hoặc thêm job cho
check này. Actual packaged HTTP job ở trên được chạy trước fix validator; binary,
credentials và queue semantics không đổi.

## Test failure đã điều tra, không bỏ qua

Full integration lượt đầu có một HTTP test fail: sau khi thả held pool connection,
read budget60ms vẫn có thể trả503 trong lúc host đang build Docker. Test cũ đòi
404 ngay ở request kế tiếp. Không thay runtime timeout hoặc nới bốn assertions
deadline ban đầu. Test dùng cùng app/pool/admission, cho recovery tối đa2s, chỉ
chấp nhận503 `operation_timeout` trước404;429 hoặc error khác vẫn fail. Targeted
rerun0,33s PASS và full67tests sau đó PASS. Peer review xác nhận test vẫn phát hiện
response-admission/byte-reservation leak; nó không chứng minh serializer CPU permit
recovery vì missing-job path không serialize result.

Initial failure log vẫn được giữ và hash riêng; report không chỉ giữ lần xanh.

## Retention, memory và gate tiếp theo

New worker không còn automatic prune loop. Source metadata công bố
`automatic_pruning=false`, `retention_policy=awaiting_maintenance_policy`;
`retention_days` chỉ còn compatibility/configuration value. A03 runtime và
maintenance không có DELETE/TRUNCATE jobs. Điều này chưa triển khai holds hoặc
retention policy, và chưa bảo vệ old privileged deployment chưa cut over.

[A04 preflight](a04-preflight-review.md) xác định hai điểm phải xử lý ở DB: legacy
privileged prune có thể bỏ qua hold; old completion có thể clear input khi source
retention tắt. Hold/release/completion/review/delete phải serialize trên cùng job
row. A04 tooling/policy và A05 restore drill vẫn chưa qua gate.

[E02 audit](e02-memory-audit.md) có read-only allocation-owner evidence và bounded
observer đã kiểm: persistent `layout-detect` giữ khoảng1,58GiB; Rust worker chỉ
khoảng25,6MiB RSS. Cgroup peak1,75/2GiB vẫn chưa đạt headroom gate. CPU arena là
giả thuyết cần A/B, chưa phải nguyên nhân được chứng minh. Không tăng W/R/layout
pool/model budget trong A03/E02; chưa kết luận đạt hơn1.000 docs/ngày.

Independent labels/live v2 exporter, human reviewer identity, parser isolation,
backup/restore và quality/capacity gates vẫn còn. Confidence chưa calibration giữ
null, conflict không tự chọn và automatic acceptance tắt.

Disposable test containers được dừng sau thu evidence; smoke volumes và private
secrets giữ riêng để audit. Không xóa application volumes, đổi native model,
commit/push hoặc ghi lại private document text trong report.

Final independent review đối chiếu32 original/preserved log size+SHA checks,
101 source fingerprints, Redis launcher và E02 evidence: PASS, không còn material
finding trong phạm vi review này. Scan trong memory với18 disposable secret files
và decoded URI passwords không có match trong20 report/record/log files; không
in giá trị. Cleanup được kiểm độc lập:0 test containers,2 smoke volumes giữ lại,
4 application containers vẫn running. Source/code fingerprint và giới hạn của
image được ghi rõ trong verification record.
