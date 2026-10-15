# A05 — Vận hành backup mã hóa và restore cách ly

Runbook này dành cho thao tác local bằng Docker với `ocr-backup` và hai file
[compose.backup.yaml](../compose.backup.yaml),
[compose.restore.yaml](../compose.restore.yaml). Công cụ tạo logical dump
PostgreSQL18, mã hóa bằng age1, kiểm tra toàn bộ archive trước khi kết nối đích,
rồi so sánh nội dung DB sau restore. Nó luôn để DB đích cách ly và không tự cấp
quyền runtime hoặc khởi động API/worker.

Đây là tooling và quy trình diễn tập local. Chưa chứng nhận backup production,
PITR, RPO/RTO, bản sao ngoài host, key escrow hoặc khả năng phục hồi sau mất host.
Không dùng việc có một file dump làm bằng chứng các gate đó đã đạt. Việc chọn
policy và đối soát những bản ghi bị xóa sau snapshot vẫn là công việc riêng.

## Phạm vi và điều kiện đầu vào

Nguồn phải là deployment PostgreSQL18 đã qua quy trình hạn quyền trong
[A03 operations](a03-operations.md), với schema ứng dụng được binary hỗ trợ,
bootstrap/migrate/grant đã hoàn tất và credential `ocr_backup` đang có hiệu lực.
`create` kiểm tra actual `session_user=ocr_backup`; không dùng credential owner,
admin hoặc migrator để thay thế. Secret nguồn phải thuộc đúng deployment đang
backup. Mọi migrate/bootstrap/grant trong cửa sổ backup phải dùng binary A05
đã review và cùng cooperative operations lock; binary cũ hoặc SQL thủ công không
được cơ chế này bao phủ. Chạy `ocr-secrets init` ở thư mục khác không rotate role nguồn.

Đích restore là PostgreSQL18 mới, project/volume/secret mới, chưa có bảng ứng
dụng. Đích dùng `ocr_migrator`; bootstrap tạo role trước, nhưng **không chạy
migrate trước full restore**. Dump đã chứa schema và `_sqlx_migrations`. Luồng
này khác luồng triển khai mới thông thường `bootstrap → migrate → grant`.

Người quản trị nguồn và người cung cấp archive/hash phải đáng tin cậy. Snapshot,
khóa bảng và advisory lock phối hợp với các lệnh quản trị của ứng dụng; chúng
không ngăn superuser hoặc người sở hữu DB cố ý bỏ qua khóa, sửa DDL/function hay
thay dữ liệu. Manifest nội dung không chứng minh toàn bộ constraints, index,
trigger binding hoặc ACL. Restore một archive do bên không tin cậy tạo có thể
thực thi SQL của archive; age và SHA256 không biến nó thành SQL an toàn.

Không chỉnh logging/proxy/audit của DB theo cách ghi password hoặc parameter
nhạy cảm. Không dùng `set -x`, không in secret files, không đưa DB URL/age
identity vào argv, không dùng `docker inspect` đầy đủ làm evidence. Chỉ mount
secret cần cho từng lệnh. Stdout có UUID, số lượng, thời điểm và hashes; stderr
là thông tin vận hành. Bảo vệ cả hai dù CLI không in tài liệu OCR thô.

## Chuẩn bị image và thư mục riêng

Chạy từ root repository. Các biến dưới đây là ví dụ; thay project và đường dẫn
bằng giá trị đã xác nhận. Dùng đường dẫn tuyệt đối không có dấu phẩy cho các
bind mount. Không source `.env` của môi trường development. `--env-file
/dev/null` trong các lệnh Compose chủ động tránh đọc file đó.

```sh
set -eu
set -C
umask 077
export BACKUP_IMAGE='ocr-service:backup-a05'
export OCR_SOURCE_PROJECT='ocr-source-reviewed'
export PROD_SECRETS_DIR='/absolute/private/source-secrets'
export OPS_PRIVATE_DIR='/absolute/private/ocr-backup-operations'
export BACKUP_KEY_DIR="$OPS_PRIVATE_DIR/keys"
export BACKUP_VOLUME='ocr-encrypted-backups-local-01'
export BACKUP_SCRATCH_SIZE='512m'
export BACKUP_BUNDLE='snapshot-20261015-01'

mkdir -m 0700 "$OPS_PRIVATE_DIR"
mkdir -m 0700 "$BACKUP_KEY_DIR"
docker build --target backup-ops -t "$BACKUP_IMAGE" .
```

Hai thư mục phải mới; lệnh `mkdir` không chủ động ghi đè chúng. `set -C` chặn
redirect ghi đè receipt/log đã có; mỗi lần thao tác mới cần thư mục evidence
riêng để không làm mất hash tin cậy của lần cũ. `PROD_SECRETS_DIR`
là thư mục nguồn đã có sẵn, chứa `backup_database_url` đang hoạt động cùng các
file mà Compose nguồn yêu cầu. Tag trên dùng cho local; release phải ghi nhận
image bất biến đã được review. Build không có nghĩa đã backup hoặc restore DB.

Image `backup-ops` có `ocr-backup`, `ocr-admin`, `ocr-secrets`, công cụ PG18 và
age. Compose chạy thao tác bằng UID/GID10001, root filesystem read-only, bỏ
capabilities, không restart tự động, giới hạn CPU/memory/PID. `/scratch` phải là
Linux tmpfs riêng, mode0700, thuộc UID đang chạy; CLI từ chối chạy trực tiếp trên
macOS hoặc dùng thư mục disk thông thường làm scratch. Docker cung cấp môi
trường Linux cho Mac.

Tmpfs không phải cam kết dữ liệu chỉ tồn tại trong RAM vật lý: kernel/VM có thể
swap, và Docker Desktop có lớp lưu trữ/VM riêng. Cần đánh giá swap, mã hóa host,
quyền Docker và chính sách host. age identity, passfile và archive plaintext khi
verify/restore đều xuất hiện trong bộ nhớ hoặc tmpfs của thao tác.

## Tạo age key offline

Chỉ thực hiện bước này cho key mới. Lệnh tạo key ghi identity thẳng vào file,
không in identity ra terminal. `--network none` áp dụng sau khi image đã build.

```sh
docker run --rm --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$BACKUP_KEY_DIR,dst=/keys" \
  "$BACKUP_IMAGE" age-keygen -o /keys/identity.txt \
  > "$OPS_PRIVATE_DIR/keygen.stdout" 2> "$OPS_PRIVATE_DIR/keygen.stderr"

docker run --rm --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$BACKUP_KEY_DIR,dst=/keys,readonly" \
  "$BACKUP_IMAGE" age-keygen -y /keys/identity.txt \
  > "$BACKUP_KEY_DIR/recipient.txt" 2> "$OPS_PRIVATE_DIR/recipient.stderr"

chmod 0444 "$BACKUP_KEY_DIR/identity.txt" "$BACKUP_KEY_DIR/recipient.txt"
```

Parent directory0700 bảo vệ file trên host. Mode0444 của từng file là để
Compose file-backed secret được UID10001 đọc: Compose local không remap
UID/mode của file nguồn. Chỉ file được khai báo mới được mount vào container;
không chia sẻ cả thư mục key cho các dịch vụ. Không chuyển các file0444 này sang
parent directory công khai. Xem thêm cách cấp secret trong
[A03 operations](a03-operations.md).

`create` nhận đúng một native recipient `age1…`; `verify`/`restore` nhận đúng một
native identity `AGE-SECRET-KEY-1…`. Comments và dòng trống được phép trong file
đầu vào, nhưng key phải hợp lệ với age. Chưa có hỗ trợ SSH identity, plugin,
passphrase hoặc nhiều recipient trong CLI này. Không đặt identity vào Git hay
cùng vị trí lưu archive. Quyết định escrow, khôi phục khi mất key, rotation và
quyền truy cập key phải được hoàn tất riêng; các lệnh trên chỉ tạo key local.

## Tạo backup từ nguồn đã xác nhận

Xác nhận `OCR_SOURCE_PROJECT` chọn đúng project hiện có và
`PROD_SECRETS_DIR` thuộc nguồn đó. Không chạy `up` toàn bộ Compose hoặc bootstrap
nguồn như một phần của backup. Dùng `--no-deps` để lệnh one-shot không tự khởi
động hay thay thế PostgreSQL, Redis, API hoặc worker.

Ví dụ dưới đây dành cho DB local nhỏ: mỗi stream plaintext/ciphertext tối đa
256MiB, tổng thao tác tối đa600 giây. Ciphertext có overhead; đừng chọn giới hạn
sát kích thước plaintext. Scratch512MiB và memory2GiB trong Compose không đủ cho
archive plaintext tùy ý lớn. Nếu cần tăng giới hạn, review đồng thời kích thước
scratch, memory limit, dung lượng backup volume và deadline; đừng chỉ tăng
`--max-bytes`.

`BACKUP_VOLUME` phải là named volume chuyên cho backup. Với volume mới, Docker
khởi tạo `/backups` từ directory thuộc UID10001 trong image. Nếu volume đã có
owner/nội dung khác và bị permission denied, kiểm tra nguồn gốc của volume hoặc
chọn volume mới; không chạy chown đệ quy trên volume không rõ là gì.

```sh
docker compose --env-file /dev/null -p "$OCR_SOURCE_PROJECT" \
  -f compose.production.yaml -f compose.backup.yaml --profile ops \
  run --rm --no-deps -T backup \
  ocr-backup create \
  --output "/backups/$BACKUP_BUNDLE" \
  --recipient-file /run/secrets/backup_recipient \
  --scratch-dir /scratch --timeout-seconds 600 --max-bytes 268435456 \
  > "$OPS_PRIVATE_DIR/create-receipt.json" \
  2> "$OPS_PRIVATE_DIR/create.stderr"
```

Chỉ tiếp tục nếu process exit0 và stdout hợp lệ có `status=backup_created`.
Giữ receipt trong nơi được bảo vệ độc lập với bundle, qua kênh tin cậy. Ví dụ
đang lưu receipt ngoài named volume để tách quyền sửa; cùng một host vẫn chưa
đáp ứng off-host/disaster recovery. Không chia sẻ receipt công khai.

Luồng tạo backup là:

1. Mở repeatable-read/read-only transaction, giữ operations lock chung và export
   snapshot; manifest nội dung và `pg_dump` đọc cùng snapshot đang được giữ mở.
2. Stream `pg_dump → age → database.dump.age.partial`, tính hash/byte count cho
   cả plaintext và ciphertext; không tạo plaintext dump file ở bước create.
3. Yêu cầu cả hai process exit thành công, sync output, hoàn tất transaction rồi
   công bố archive và manifest đã mã hóa.
4. Ghi `complete.json` cuối cùng và sync directory cùng parent; stdout trả receipt có
   `manifest_ciphertext_sha256` để pin độc lập.

Bundle hoàn tất chứa `database.dump.age`, `manifest.json.age`, `complete.json`.
Một directory tồn tại, file `.partial` hoặc archive riêng lẻ chưa phải backup
hoàn tất. CLI từ chối directory output đã tồn tại và không ghi đè bundle. Khi
create lỗi/timeout, giữ riêng artifact và log để kiểm tra; lần thử tiếp theo dùng
tên bundle mới. Không quảng bá một phần artifact thất bại là backup thành công.

`snapshot_at` và `completed_at` đều dùng đồng hồ DB nguồn. Clock DB dịch lùi
trong thao tác vẫn có thể khiến manifest bị từ chối. Những mốc này là metadata
vận hành, không chứng minh chính xác commit cuối cùng được bao phủ hay data-loss
window. Các write commit sau snapshot không thuộc backup này. Thời gian tạo
dump hoặc kích thước file không tự chứng minh RPO/RTO.

## Pin hash từ receipt tin cậy và verify offline

Lấy hash từ stdout của lần create thành công đã lưu độc lập. Không lấy hash từ
`complete.json` nằm cạnh archive rồi coi đó là nguồn tin cậy: kẻ thay cả bundle
có thể thay luôn seal. age mã hóa và kiểm tra toàn vẹn nhưng không ký danh tính
người tạo. Nếu bundle/receipt được chuyển qua hệ thống khác, phải xác thực kênh
và quyền sửa của receipt/hash tại hệ thống đó.

Ví dụ dùng Python trên host để đọc receipt mà không in nội dung ra terminal:

```sh
TRUSTED_MANIFEST_SHA256="$(python3 -c '
import json, re, sys
with open(sys.argv[1], encoding="utf-8") as receipt_file:
    receipt = json.load(receipt_file)
if receipt.get("status") != "backup_created":
    raise SystemExit("receipt is not a successful create")
value = receipt["manifest_ciphertext_sha256"]
if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
    raise SystemExit("receipt hash is invalid")
print(value)
' "$OPS_PRIVATE_DIR/create-receipt.json")"
export TRUSTED_MANIFEST_SHA256

docker compose --env-file /dev/null -p "$OCR_SOURCE_PROJECT" \
  -f compose.production.yaml -f compose.backup.yaml --profile ops \
  run --rm --no-deps -T backup-verify \
  ocr-backup verify \
  --bundle "/backups/$BACKUP_BUNDLE" \
  --identity-file /run/secrets/backup_identity \
  --manifest-sha256 "$TRUSTED_MANIFEST_SHA256" \
  --scratch-dir /scratch --timeout-seconds 600 --max-bytes 268435456 \
  > "$OPS_PRIVATE_DIR/verify-receipt.json" \
  2> "$OPS_PRIVATE_DIR/verify.stderr"
```

Service `backup-verify` có `network_mode: none`, chỉ mount identity và backup
volume read-only, không nhận DB credential. Exit0 phải đi cùng
`status=encrypted_archive_verified`, `database_connected=false` và
`ready_for_service=false`. Verify kiểm pinned manifest ciphertext, decrypt
manifest, xác thực format/content manifest, hash ciphertext, rồi decrypt **toàn
bộ** archive ra private tmpfs và so plaintext hash/size. Đọc archive dùng cùng
opened file descriptor đã hash, không mở lại input pathname cho age.

age có thể phát plaintext sớm rồi báo lỗi ở chunk cuối. Vì vậy không pipe kết
quả decrypt trực tiếp vào `pg_restore`: CLI phải đợi age exit thành công và mọi
hash/limit hợp lệ trước khi restore có quyền kết nối DB. Verify thành công vẫn
chưa chứng minh SQL dump restore được hoặc app sẵn sàng phục vụ.

## Restore vào PostgreSQL mới trong quarantine

Chọn project chưa từng dùng và giữ nguyên `BACKUP_VOLUME`, `BACKUP_KEY_DIR`,
`BACKUP_BUNDLE`, `TRUSTED_MANIFEST_SHA256` của backup đã verify. Không trỏ Compose
restore vào volume hiện tại. Không dùng lệnh `down -v` trên project nguồn để
"dọn chỗ" cho restore.

```sh
export OCR_RESTORE_PROJECT='20261015-01'
export OCR_RESTORE_COMPOSE_PROJECT="rust-ocr-restore-$OCR_RESTORE_PROJECT"
export RESTORE_SECRETS_DIR="$OPS_PRIVATE_DIR/restore-secrets"

docker run --rm --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$OPS_PRIVATE_DIR,dst=/provision" \
  "$BACKUP_IMAGE" ocr-secrets init /provision/restore-secrets \
  > "$OPS_PRIVATE_DIR/restore-secrets-init.stdout" \
  2> "$OPS_PRIVATE_DIR/restore-secrets-init.stderr"
```

`ocr-secrets init` không kết nối service, không in giá trị và từ chối directory
đã tồn tại. Nếu thất bại giữa chừng, giữ partial directory để kiểm tra và chọn
directory mới; không coi partial secrets là bộ credential hợp lệ. Bộ secret mới
này chỉ dành cho DB đích; không copy lên nguồn. File inference placeholder được
tạo cùng bộ secrets không dùng trong quarantine này.

Trước khi chạy, xác nhận volume `${OCR_RESTORE_COMPOSE_PROJECT}_restore-postgres` chưa
tồn tại hoặc chọn project mới. File restore chỉ có PostgreSQL, bootstrap và
restore service trên network `quarantine` internal; không publish DB port, không
có API/worker/Redis/model. Backup volume là external/read-only ở service restore.

```sh
docker compose --env-file /dev/null -p "$OCR_RESTORE_COMPOSE_PROJECT" \
  -f compose.restore.yaml up -d --wait postgres

docker compose --env-file /dev/null -p "$OCR_RESTORE_COMPOSE_PROJECT" \
  -f compose.restore.yaml run --rm --no-deps -T bootstrap \
  ocr-admin roles bootstrap \
  > "$OPS_PRIVATE_DIR/restore-bootstrap.stdout" \
  2> "$OPS_PRIVATE_DIR/restore-bootstrap.stderr"

docker compose --env-file /dev/null -p "$OCR_RESTORE_COMPOSE_PROJECT" \
  -f compose.restore.yaml run --rm --no-deps -T restore \
  ocr-backup restore \
  --bundle "/backups/$BACKUP_BUNDLE" \
  --identity-file /run/secrets/backup_identity \
  --manifest-sha256 "$TRUSTED_MANIFEST_SHA256" \
  --expected-empty-database ocr \
  --scratch-dir /scratch --timeout-seconds 600 --max-bytes 268435456 \
  > "$OPS_PRIVATE_DIR/restore-receipt.json" \
  2> "$OPS_PRIVATE_DIR/restore.stderr"
```

`ocr` là tên DB cố định trong Compose restore và secrets do provisioner tạo.
`--expected-empty-database` bắt buộc; CLI đối chiếu actual database/login,
PostgreSQL18, inventory rỗng, role configuration và ACL trước khi restore. Nếu
tự tạo cấu hình khác, phải review đồng bộ DB name, credentials và argument;
không truyền một tên dự đoán để vượt preflight.

Không chạy `ocr-admin schema migrate`, `roles grant`, API hoặc worker trong
chuỗi lệnh trên. `pg_restore` chạy `--single-transaction --exit-on-error`, không
dùng clean, không khôi phục owner/ACL và dùng role `ocr_owner`. Sau đó CLI lấy
content manifest mới và so sánh với manifest nguồn. Dòng thành công là
`status=restored_content_verified`, kèm `runtime_granted=false`,
`ready_for_service=false`, `retention_apply_enabled=false` và yêu cầu nguồn
đối soát post-snapshot deletions bên ngoài.

Preflight giữ một dedicated connection và advisory lock tới khi verify xong.
Đây là khóa phối hợp, không phải hàng rào với một administrator khác. Không cho
người/dịch vụ khác truy cập target trong diễn tập. Full restore có thể chứa dữ
liệu nhạy cảm; network internal không thay thế quyền Docker/host.

## Đọc kết quả và xử lý thất bại

| Kết quả | Ý nghĩa và bước tiếp theo |
| --- | --- |
| Create exit0 + `backup_created` | Giữ receipt/hash độc lập; chạy verify và restore drill. Chưa phải chứng nhận production backup. |
| Verify exit0 + `encrypted_archive_verified` | Ciphertext/plaintext/manifest nhất quán; chưa kết nối DB và chưa kiểm app readiness. |
| Restore exit0 + `restored_content_verified` | Nội dung trong manifest khớp snapshot nguồn; DB vẫn cách ly, chưa cấp quyền runtime. |
| Sai/mất key, pinned hash không khớp, archive hỏng hoặc vượt giới hạn | Dừng. Restore phải thất bại trước kết nối đích ở giai đoạn xác thực archive. Không sửa manifest/hash để ép qua. |
| Source schema/role/version không hỗ trợ, lock timeout hoặc DDL đang chạy | Dừng và review nguồn/config. Không đổi sang admin để bỏ qua điều kiện. |
| Target không rỗng, role/ACL/version không hợp lệ | Dừng; giữ target cách ly. Chọn target mới đúng preconditions thay vì clean/adopt/migrate cho qua. |
| Disk/tmpfs đầy, child process lỗi, sync lỗi hoặc deadline | Không có kết quả thành công hợp lệ. Bảo vệ log/artifact, review resource budget và thử bằng bundle/target mới. |
| Restore hoàn tất SQL nhưng content verification/logging thất bại | DB có thể đã chứa dữ liệu. Không grant hoặc mở traffic; giữ cách ly và điều tra. |

Deadline/cancellation không chứng minh restore đã rollback. Khi COMMIT đã gửi,
process có thể mất kết nối hoặc bị hủy sau lúc server commit nhưng trước khi
CLI nhận kết quả. Kể cả restore exit khác0 cũng phải coi target là trạng thái
chưa xác định và giữ cách ly. Không retry lên target đó như thể nó còn rỗng;
lần diễn tập mới dùng project/volume/secret mới và giữ evidence của lần cũ.

CLI dùng lỗi cố định đã redact, không in child stderr hoặc error chi tiết của
driver. Không bật log bí mật để "debug nhanh". Evidence cần lưu: image identity,
commit/source manifest của build, phiên bản công cụ, stdout/stderr bảo vệ, exit
code, timestamps, byte counts, content roots/counts, cấu hình resource và nhận
xét restore. Đo thời gian create/verify/restore thực tế với dữ liệu đại diện;
không suy ra capacity hoặc RTO từ một fixture nhỏ.

`--timeout-seconds` nhận1–3600, mặc định600; `--max-bytes` nhận1–1TiB, mặc định
10GiB và áp dụng riêng cho plaintext/ciphertext. Những trần parser này không
phải khả năng xử lý đã được chứng minh. Content scan có budget riêng, vì vậy
tăng timeout ngoài không đảm bảo một DB lớn sẽ qua. Restore cần đủ tmpfs cho
archive plaintext và memory headroom; giữ file scratch đến khi restore/verify
hoàn tất. Process được giám sát theo group với deadline và cleanup khi lỗi/hủy;
không dựa vào việc terminal đã đóng để quyết định trạng thái DB.

## Các gate trước khi cho target phục vụ

Không có bước tự động mở traffic trong runbook này. Trước một cutover cần
review độc lập ít nhất các điều kiện sau:

- Schema/inventory/grants của bản build được hỗ trợ, cấu hình secret đích, queue
  và worker lease/outbox recovery; chứng minh bằng release rehearsal riêng.
- Policy/class/retain-until/review grace và legal hold còn hiệu lực. Snapshot
  khôi phục cả trạng thái cũ; thời gian retention phải được đánh giá theo thời
  điểm hiện tại, không chỉ thời điểm backup.
- Nguồn authoritative về những bản ghi đã bị xóa sau snapshot, nằm ngoài chính
  snapshot đang restore, có integrity/access/retention policy riêng. Đối soát
  trước khi cho phép đọc hoặc xử lý lại record để tránh hồi sinh dữ liệu đã xóa.
  DB vừa restore không tự biết các deletion xảy ra sau backup. Hiện công cụ này
  chưa triển khai nguồn đó hay quy trình reconciliation được chứng nhận.
- RPO/RTO đã chọn và đo được, lịch backup, off-host copy/restore, lỗi mất host,
  thiếu key và key escrow/rotation. Logical dump này không có WAL archive hoặc
  point-in-time recovery.
- Chính sách thời hạn lưu/hủy historical backups, receipts và logs; xóa một row
  ở DB đang chạy không xóa các bản sao trong archive cũ.

Thiếu các gate đó thì giữ `ready_for_service=false`. Destructive retention vẫn
bị vô hiệu hóa; hướng dẫn hiện hành nằm trong
[retention operations](retention-operations.md). Không tự thêm grant, bật API,
restore đè volume nguồn hoặc xóa volume để biến một diễn tập local thành
production cutover.
