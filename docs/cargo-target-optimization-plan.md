# Kế hoạch tối ưu Cargo target: dung lượng và vòng lặp phát triển

Ngày 16/10/2026. Baseline: `69c55d3c72348e6c7fa2caa57e0af4201f6e056d`, branch `codex/rust-ocr-service`, native Cargo/rustc `1.98.1`.

**Trạng thái: audit read-only hoàn tất; lượt thực thi đã tới resource preflight và NO-GO.** [Execution trace](cargo-target-optimization-execution-trace.md) ghi idle gate không đạt; cleanup, profile changes và compilation benchmark chưa thực hiện. Đây là tối ưu build/cache trên máy phát triển, không phải bằng chứng tăng OCR throughput, giảm RSS production hoặc hoàn tất các gate trong [production execution plan](production-execution-plan.md). Không thay release profile, W/R, model, containers, volumes hoặc dữ liệu OCR.

## 1. Vì sao target lớn khoảng 42 GiB?

### Số đo thực tế

`du -x -k -d 2 target` ghi nhận các giá trị sau; GiB = 2³⁰ bytes, không phải GB thập phân.

| Phạm vi | Allocated GiB | Nhận định |
| --- | ---: | --- |
| Toàn bộ `target` | 42.24 | Cache và output build native |
| `target/debug` | 41.25 | Khoảng 97.6% tổng dung lượng |
| `target/release` | 0.99 | Không phải nguyên nhân chính |
| `target/debug/incremental` | 20.11 | Attribution của lần duyệt `du`, không phải số sẽ thu hồi khi xóa riêng |
| `target/debug/deps` | 20.07 | Attribution sau hardlink dedup trong lượt `du`, không phải standalone footprint; chứa dependencies/test executables/metadata/objects |
| `target/debug/examples` | 0.78 | Fixtures/probes/examples đã biên dịch |

Các dòng con thuộc các dòng cha; không cộng tất cả dòng trong bảng. Hardlinks giữa các thư mục làm attribution thay đổi theo thứ tự duyệt. Audit toàn cây ghi nhận 372,097 file entries nhưng chỉ 128,752 inodes riêng; không phát hiện hardlink ra ngoài `target` hoặc `.dSYM` bundles. APFS clones/snapshots và open file descriptors chưa được kiểm kê, nên allocated bytes không đảm bảo bằng thay đổi free space sau cleanup.

### Nguyên nhân đã có bằng chứng

1. Workspace sáu crates đã trải qua nhiều vòng check/test/build/example. Repo chỉ tùy chỉnh release bằng `lto = "thin"` và `strip = true`; chưa có dev/test override.
2. Cargo dev mặc định có full debuginfo và incremental; test kế thừa dev. Trên macOS, Cargo mặc định `split-debuginfo = "unpacked"` khi bật debug. Vì vậy object/debug data là phần đáng xét trước, không chỉ executable cuối. [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)
3. Có nhiều feature/profile variants: Serde `rc`, serde_json `raw_value`, image formats cũ/mới và reqwest multipart. Ví dụ SQLx PostgreSQL có khoảng 30 `.rlib` variants, worker có bảy. Hash khác nhau không tự chứng minh artifact thừa hoặc có thể xóa an toàn theo tên.
4. Trong 2,136 fingerprint JSON được kiểm, chỉ thấy một rustc fingerprint. Không có bằng chứng quy nguyên nhân cho nhiều native toolchains. Mtime hoặc tên hash không đủ để kết luận stale.
5. Chín direct native executables chỉ khoảng 168.7 MiB; xóa riêng các binary này không giải quyết 42 GiB. Phần lớn dung lượng nằm ở compilation artifacts, không phải ứng dụng deploy lớn 42 GiB.

`target` được Git ignore và Docker build context loại trừ. Docker build cache/images và `~/.cargo` là các phạm vi riêng, không được cộng vào hoặc xóa để giải thích số đo này. Dev/test cùng nằm dưới `debug`; release/bench nằm riêng. [Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html)

## 2. Dọn được bao nhiêu, mất gì?

Full-tree inode audit tính allocated bytes chỉ khi toàn bộ hardlinks của inode nằm trong phạm vi sẽ bị gỡ:

| Phương án tương lai | Upper estimate thu hồi | Chi phí/rủi ro |
| --- | ---: | --- |
| Chỉ incremental | 13.42 GiB | Lần biên dịch incremental kế tiếp phải tạo lại cache; dependencies/binaries hiện có được giữ |
| Toàn bộ debug | 41.25 GiB | Mất dev/test/example cache và binaries; cần rebuild, có thể mất artifacts dùng tái hiện lỗi |
| Chỉ release | 0.99 GiB | Ít lợi ích, mất release artifacts; không đề xuất |
| Toàn bộ target | 42.24 GiB | Cold rebuild mọi native profile; không đề xuất mặc định |

Incremental có footprint 20.11 GiB nhưng khoảng 6.69 GiB còn hard-linked ở nơi khác; **không hứa thu hồi 20 GiB khi chỉ xóa incremental**. Các phương án thay thế nhau, không cộng mức tiết kiệm. Đây là ước tính upper bound theo allocated files, không phải cam kết `df`. Cache sẽ tăng trở lại nếu workflow/profile không đổi. Chuyển thư mục sang nơi khác trên cùng volume không giải phóng dung lượng.

Snapshot 23:09 giờ địa phương có khoảng 208.9 GiB available. Đây không phải tình trạng disk-full khẩn cấp tại thời điểm audit; phải đo lại trước thực thi. Các snapshot free-space trước đó khác nhau; audit này không dọn gì và không xác định nguyên nhân thay đổi bên ngoài.

## 3. Lộ trình thực hiện, thứ tự và điểm dừng

| Pha | Công việc | Đầu ra / gate | Trạng thái |
| --- | --- | --- | --- |
| P0 | Audit inode, profiles, feature variants và consumers | Baseline có caveats, không nhầm logical/physical bytes | Hoàn tất read-only |
| P1 | Chọn phạm vi cleanup nếu cần và bảo toàn evidence | Người dùng xác nhận phạm vi; consumers đã quiesce | Giữ nguyên target, không cleanup trong lượt thực thi |
| P2 | A/B giảm dev debuginfo, giữ incremental | Công bằng cùng workload; kiểm disk/time/debuggability | Preflight NO-GO; chưa chạy compiler |
| P3 | A/B incremental on/off riêng | Chọn theo edit/rebuild cost, không chỉ disk | Chưa thực thi |
| P4 | Áp dụng đúng một thay đổi đã chứng minh | Harness qua, trace và local commit | Chưa thực thi |
| P5 | Đánh giá cache growth sau workflow thực tế | Budget có số đo, cleanup có chủ đích | Chưa thực thi |

### P1 — Cleanup có chủ đích, không phá evidence

1. Mặc định giữ nguyên cache cho tới khi chọn scope/giờ thực thi. Nếu cần lấy lại dung lượng trước benchmark, ưu tiên incremental-only, kỳ vọng tối đa khoảng 13.4 GiB thay vì xóa toàn debug.
2. Kiểm tra lại Cargo/rustc/rustdoc/clippy, app binaries và users của target. Snapshot không thấy native build/service đang chạy nhưng có rust-analyzer/proc-macro servers; tạm dừng background checks qua IDE trước cleanup, không kill rộng theo tên process. Không dừng live stack.
3. Ghi HEAD, clean/dirty state, Cargo.lock hash, `rustc -vV`, Cargo version, target triple, profile/features, flags và jobs đã chọn. Chỉ lưu thông tin allowlisted, không dump environment hoặc expanded Compose.
4. Kiểm tra các probe/test commands tham chiếu hashed `.rlib`/test binaries. Bảo toàn direct binaries/examples thực sự cần, source hashes, logs và frozen Linux exports trong private ignored evidence trước debug cleanup. Copy executable giữ artifact bytes/hash nhưng không đảm bảo macOS unpacked DWARF/LLDB còn dùng được: giữ referenced objects cần thiết hoặc chấp nhận rebuild debug artifacts. Mtime không phải source attestation. Không sao chép toàn bộ cache để rồi làm disk tăng gấp đôi.
5. Incremental-only không có stable Cargo clean flag chuyên biệt: nếu chọn, dùng thao tác một lần có kiểm tra canonical repo path, directory type/symlink và scope cố định. Không thêm maintenance daemon/script chung chỉ cho một lần dọn; không dùng glob xóa `.rcgu.o` trong deps.
6. Nếu người dùng chọn xóa debug, preview bằng Cargo trước; dev và test dùng chung thư mục nên cả hai sẽ chịu ảnh hưởng. Lệnh dưới **chỉ là bước dự kiến, chưa chạy**:

```sh
cargo clean --dry-run --profile dev \
  --target-dir /Users/tonminh-mac/Documents/GitHub/ocr-service/target
```

`--dry-run` chỉ liệt kê/tóm tắt; `cargo clean` không tùy chọn xóa toàn target. Chỉ bỏ `--dry-run` sau khi scope đã xác nhận và preview phù hợp; không tự mở rộng sang release. [Cargo clean](https://doc.rust-lang.org/cargo/commands/cargo-clean.html)

7. Đo lại unique allocated bytes và `df` sau cleanup; ghi khác biệt, không coi upper estimate là kết quả. Recovery của cache là rebuild theo manifest đã ghi, không phải khôi phục bit-identical binaries. Cleanup hoàn toàn không thay hoặc xác nhận provenance của runtime images.

### P2 — Ứng viên profile tối thiểu trước

Thử đúng một thay đổi trong isolated target directory bằng Cargo config override; **chưa sửa Cargo.toml trong đợt planning này**. Nếu qua gate mới áp dụng tại workspace root:

```toml
[profile.dev]
debug = "line-tables-only"
```

Giữ incremental, split-debuginfo, opt-level, codegen-units, panic và release như cũ. Không cần lặp lại `[profile.test]`. Line tables hỗ trợ backtrace file/line nhưng mất variable/type debugging; `debug = 1` là ứng viên riêng nếu cần thêm module metadata, không khôi phục full locals. Phải kiểm backtrace/LLDB thực tế. [Cargo debug settings](https://doc.rust-lang.org/cargo/reference/profiles.html#debug)

Profile đổi có thể tạo thêm artifact variants; **không tự giảm dung lượng cache cũ**. Không đo bằng cách thêm candidate vào target 42 GiB rồi so tổng. Không tắt mọi debug info hoặc bật strip trong dev chỉ để có con số nhỏ.

### P3 — Chỉ sau P2 mới thử incremental on/off

So sánh profile P2 đã chọn với cùng profile nhưng incremental off; giữ mọi yếu tố khác cố định. Ghi nhận codegen-units mặc định thay đổi theo incremental; đó là hệ quả của phương án off, không phải thí nghiệm thuần cache khi chưa kiểm soát yếu tố này. Nếu cần phân tích sâu mới thêm lượt cố định codegen-units. Không đồng thời đổi jobs hoặc linker.

Chỉ chọn off cho local khi lợi ích disk bù được edit/rebuild cost đo thật. CI runner dùng một lần và máy developer có edit loop là hai quyết định khác nhau. Không đặt global `CARGO_INCREMENTAL=0` cho mọi repo.

### P4/P5 — Adoption nhỏ, giới hạn cache có căn cứ

- Trước mọi source-symbol change phục vụ probe: GitNexus upstream impact và báo blast radius; HIGH/CRITICAL phải cảnh báo trước. Không sửa code production chỉ để làm benchmark thuận tiện.
- Nếu thắng, patch profile tối thiểu, không thêm profile hierarchy/plugin/cache server. Rollback cấu hình là revert thay đổi profile; artifacts cần rebuild riêng.
- Chuẩn hóa workflow trong một repo target directory; ghi explicit target/profile/features và flags. Không ép feature union hoặc `--all-features` để tránh variants vì có thể đổi hành vi/compile scope.
- Theo dõi sau ba chu kỳ check → targeted test → workspace verification có edit thực tế. Mức cảnh báo đề xuất ban đầu: target trên 20 GiB cần xem growth, trên 30 GiB cần review; không phải quota bắt buộc hay ngưỡng tự xóa. Chốt budget cuối sau A/B và nhu cầu evidence.
- Không clean trước mỗi build. Cache hết hạn chỉ dọn khi không có consumers và đã kiểm scope/evidence; không tạo scheduler trong đợt này. Chạy targeted checks trong iteration, full verification ở milestone; không bỏ required gates để tiết kiệm disk.

## 4. Thiết kế benchmark công bằng và bounded

### Điều kiện và giới hạn

- Dùng source snapshot hiện tại + lockfile/toolchain/host target giống nhau; dependency registry đã sẵn sàng. Kiểm soát config/flags allowlisted và background IDE checks. Không dùng số 42 GiB tích lũy nhiều lịch sử làm đối chứng cho một candidate mới build một lần.
- Mỗi variant dùng fresh owned ignored target directory riêng; không hardlink/copy cache từ baseline sang candidate. Tối đa hai experiment directories tổng cộng: baseline A và candidate B; chạy tuần tự, kiểm headroom trước từng lượt. Mỗi cold repeat bắt đầu từ directory rỗng, rồi unchanged/edit steps tái dùng đúng directory của repeat đó. Không tự xóa directory không do lượt thử tạo.
- Khởi đầu `jobs = 1` cho cả hai phía để bảo vệ headroom; không tăng thread/runtime concurrency. Đây không cấm compiler parallelism nội bộ và không phải tối ưu multithreading của OCR.
- Fresh Cargo cache không có nghĩa cold filesystem cache. Không purge OS cache; ghi rõ hiệu ứng OS cache và đổi thứ tự A/B giữa các lần đo.
- Ngưỡng dừng đề xuất: free disk dưới 30 GiB, sustained swap/memory pressure tăng, OOM hoặc ảnh hưởng live service. Phải kiểm memory pressure thực tế; free disk lớn không đồng nghĩa đủ RAM. Tạm dừng, không tăng resource limits hoặc chuyển sang build song song.
- Tối đa ba paired repeats cho variant còn khả thi; chạy một pilot trước để định budget time/disk. Không chạy full matrix nhiều giờ khi chưa xem kết quả pilot. Logs/timings chỉ lưu private ignored artifacts.

### Cùng một command suite, ba tình huống

| Tình huống | Công việc giống nhau giữa A/B | Cần ghi |
| --- | --- | --- |
| Cold compilation | `cargo check --workspace --all-targets --locked`, `cargo test --workspace --locked --no-run`, rồi build các fixture examples thực sự dùng | Wall/user/sys, peak RSS của command, timings, disk sau từng bước |
| Unchanged | Lặp lại suite không sửa nguồn | Cache-hit/no-op time, bất thường rebuild |
| Small edit | Cùng một thay đổi nhỏ đại diện đã review ở workspace crate; rebuild targeted và dependent tests; restore sau lượt | Edit-to-test latency, crates invalidated, disk growth |

Truyền cùng `--target-dir` của variant và cùng `-j 1` cho mỗi Cargo command. Candidate P2 dùng thêm `--config 'profile.dev.debug="line-tables-only"'` nhất quán cho cả suite; baseline không có override. Ghi mọi override từ IDE/environment và xác nhận effective debuginfo flags của cả dev lẫn test compilation bằng allowlisted compiler/profile evidence; chỉ ghi intended config chưa đủ chứng minh test đã kế thừa đúng. Không dump environment/build-script output để lấy bằng chứng. Dùng `/usr/bin/time -l` và Cargo `--timings` cho compilation; peak RSS của command không phải tổng peak memory toàn compiler process tree hoặc worker production.

Một suite có check rồi test đo cold workflow từ target rỗng, không phải từng command đều cold. Tách compilation khỏi test execution; cached artifacts của bước trước là một phần workflow phải giống nhau. Không chỉ dùng `cargo check` để tuyên bố giảm test/example codegen. Small edit phải cùng nội dung/điểm đo, không lấy no-op touch làm đại diện mọi thay đổi thực tế. Chỉ sửa disposable frozen source snapshot cho thí nghiệm, không sửa rồi restore main checkout; giữ nguyên unrelated dirty changes và kiểm source/lock hashes sau mỗi lượt.

Mỗi report có: commit/source/lock hashes, toolchain/triple, variant/config/features/jobs, command exit codes, thời gian từng bước, median/range qua repeats, unique allocated bytes, hardlink attribution, `df`, memory-pressure observations, file/line backtrace và LLDB results. Không cộng RSS process chồng lấn hoặc tổng `du` từ nhiều lượt riêng có hardlinks. Thiếu sample thì ghi exploratory, không xuất p95 đáng tin cậy.

### Tiêu chí chọn và harness

Các ngưỡng dưới là **đề xuất trước thử nghiệm**, chưa phải kết quả hoặc SLA:

1. Ít nhất 25% giảm unique target allocation sau cùng suite/edits so với fresh baseline; số GiB tuyệt đối ghi sau đo, không hứa 42 → 5 GiB.
2. Median small-edit latency không chậm quá 15%; unchanged/cold times và memory phải được báo riêng. Nếu noise/range lớn thì chưa kết luận. Không lấy disk win che warm-build regression.
3. Synthetic tests giữ nguyên kết quả; backtrace còn file/line, LLDB đáp ứng nhu cầu đã chọn. Nếu cần full variable/type inspection thì không promote line-tables-only làm mặc định cho workflow đó.
4. Không có OOM/sustained pressure hoặc biến động live stack. Peak memory/time tăng cần review, không tự tăng jobs/memory để làm đẹp kết quả.
5. Chạy format, strict Clippy và workspace tests sau final profile selection; explicit integration chỉ trên disposable fixtures/services với pre/postflight. Mọi tests không chạy phải ghi rõ, không kế thừa counts của milestone trước.
6. Trước local commit cấu hình: hai source comment guards, `git diff --check`, GitNexus `detect_changes({scope: "compare", base_ref: "codex/rust-ocr-service"})`, thêm staged-scope review để bao phủ file mới. Chỉ commit code/config/docs đã review; không artifacts/private data, không push.

## 5. Chưa làm và không nên làm trước số đo

- Không xóa `.rcgu.o` thủ công hoặc strip artifacts đang dùng rồi hứa LLDB không đổi. macOS unpacked debug cần được đánh giá cùng object artifacts.
- Không chuyển ngay sang packed split-debuginfo: trên macOS có thể tạo `.dSYM` và thêm `dsymutil`; phải đo tổng footprint/link time trước, không giả định packed tiết kiệm hơn. [rustc split-debuginfo](https://doc.rust-lang.org/rustc/codegen-options/index.html#split-debuginfo)
- Không đổi release LTO/strip, thêm fat LTO, giảm codegen-units, tắt overflow/debug assertions, đổi linker hoặc bật native CPU flags để xử lý disk cache dev.
- Không thêm sccache, shared cache, dep-only overrides, toolchain pin mới hay module refactor khi chưa có nhu cầu đo được. Chỉ xét bước sau nếu P2/P3 không đủ.
- Không xóa `~/.cargo`, Docker images/build cache, volumes, PDF/labels/OCR evidence hay frozen binaries. Chúng không thuộc cleanup scope này.
- Không kết luận source/runtime có memory leak từ dung lượng target. Production throughput tiếp tục theo E02/E04: representative memory, bounded concurrency, arrival/deadline và quality gates riêng.

## 6. Evidence và completion boundary

Private ignored evidence: `artifacts/cargo-target-audit-2026-10-16/disk-audit.json` và `inode-audit.json`. File đầu chứa `du` cùng class scan chỉ trong deps/examples; class totals đó không phải phân hoạch disjoint toàn target. File thứ hai ghi full-target last-link estimates. Audit dùng filesystem metadata, không đọc lại giá trị PDF/OCR riêng tư.

Đợt đầu hoàn tất **chẩn đoán + kế hoạch**. Lượt thực thi tiếp theo đã kiểm source/toolchain/host và dừng tại [idle resource gate](cargo-target-optimization-execution-trace.md), không nới ngưỡng sau khi đo. Việc dọn cache, compilation benchmark, kết quả tiết kiệm và adoption đều còn mở. Bước tiếp theo: điều phối host workload rồi chạy lại cùng preflight; khi GO mới tạo frozen source/cache và pilot P2. Incremental-only cleanup vẫn là lựa chọn riêng, không được thực hiện trong lượt này. Không có cleanup hoặc build ngầm sau gate failure.
