# E02 — Measured memory owner, allocation audit and observer

Audit date: 2026-10-15; read-only follow-up: 2026-10-16, local Mac/Docker deployment.
Status: **READ-ONLY AUDIT + OBSERVER VERIFIED; A/B MODE PREPARED, RUN BLOCKED BY
CURRENT HEADROOM**. Allocator A/B, complete workload envelope, developer corpus,
quality parity after optimization, and capacity certification remain unproven.
No model/load was started, no runtime/Compose setting was changed, and no peak was
reset. The existing worker has a 2 GiB limit.

## 1. What the current measurements establish

A read-only idle snapshot after M1 establishes the retained owner:

| Observation | Value |
|---|---:|
| Worker cgroup current | 1,845,551,104 bytes |
| Worker cgroup lifetime peak | 1,876,815,872 bytes |
| Worker cgroup max | 2,147,483,648 bytes |
| `ocr-worker` RSS / HWM | 26,240 / 56,648 KiB |
| `layout-detect` RSS / HWM | **1,657,680 / 1,657,680 KiB** |
| Helper PSS | 1,656,544 KiB |
| Helper private dirty | **1,639,112 KiB** |
| Worker private dirty | 15,852 KiB |
| Cgroup anonymous / file / kernel | 1,703,596,032 / 134,750,208 / 5,730,304 bytes |
| Cgroup swap.current | 0 bytes |
| Cgroup max / oom / oom_kill events | 0 / 0 / 0 |
| Host RAM | 17,179,869,184 bytes (16 GiB) |
| Host swap.used | 10,734.94 MiB at this snapshot |

The idle current is 98.3% of the lifetime peak. The persistent layout helper,
mostly private memory, owns approximately 1.58 GiB; the Rust worker process is
approximately 25.6 MiB RSS. These observations identify a process and retained
memory, **not individual ONNX allocations or their cause**. Docker cgroup totals
also include file cache and kernel accounting, so process RSS is not expected to
equal `memory.current`.

The ONNX CPU arena is the leading allocation hypothesis. The session at
[`model.rs:32`](../crates/ocr-layout/src/model.rs#L32) leaves its arena/memory-pattern
defaults unchanged. [`main.rs:60`](../crates/ocr-layout/src/main.rs#L60) retains that
session across requests. ONNX Runtime documents that CPU arenas are per session,
retain allocations by default, and may be configured/shrunk. This supports an
experiment; it does not prove that disabling an arena reduces this model's peak.
[Official allocator/shrinkage documentation](https://onnxruntime.ai/docs/get-started/with-c.html).

The host already has substantial swap use. No host swap reading here attributes
memory to OCR or proves a per-job regression. Cgroup swap.current is zero while
the Mac host has swap; they describe different layers. Do not infer native MLX
GPU memory from Docker counters or add overlapping host/VM/cgroup/unified-memory
measurements.

## 2. Reproduce the observation safely

The following exact read-only commands were used to inspect the running service.
`ps` is absent inside this runtime image, so `/proc` is the authoritative fallback.
Only process names and numeric counters are read; argv/environment/source text
are never read.

```sh
docker compose exec -T worker sh -c '
  cat /sys/fs/cgroup/memory.current
  cat /sys/fs/cgroup/memory.peak
  cat /sys/fs/cgroup/memory.max
  cat /sys/fs/cgroup/memory.events
  cat /sys/fs/cgroup/memory.stat
  cat /sys/fs/cgroup/memory.swap.current
  for directory in /proc/[0-9]*; do
    test -r "$directory/comm" || continue
    name=$(cat "$directory/comm")
    case "$name" in ocr-worker|layout-detect)
      printf "process=%s\n" "$name"
      sed -n "/^VmPeak:/p; /^VmHWM:/p; /^VmRSS:/p; /^RssAnon:/p; /^RssFile:/p; /^Threads:/p" "$directory/status"
      sed -n "/^Rss:/p; /^Pss:/p; /^Private_Clean:/p; /^Private_Dirty:/p; /^Swap:/p" "$directory/smaps_rollup"
      ;;
    esac
  done'
sysctl -n hw.memsize
sysctl vm.swapusage
vm_stat
memory_pressure -Q
```

The new bounded collector is [`scripts/profile-worker-memory.py`](../scripts/profile-worker-memory.py).
It requires the exact running container name/ID and a private output directory:

```sh
python3 scripts/profile-worker-memory.py \
  --container rust-ocr-worker-1 \
  --output-dir artifacts/e02/example-unique-run \
  --duration-seconds 30 --interval-ms 200
```

Use `docker compose ps -q worker` to resolve the current ID; the script never
guesses or automatically selects a container. `--native-model-pid` and
`--docker-vm-pid` optionally record RSS for explicitly selected host processes;
only numeric RSS is read with `ps -o rss=`. Physical footprint and runtime GPU
allocations remain `null`, with an explicit limitation. No process IDs/job IDs
are emitted as metric labels; per-process trace rows contain whitelisted names
and numeric counters only. This is a local observer, not an application metrics
exporter.

Its constraints:

- Duration 0.2–600 seconds; interval at least 200 ms; at most 3,001 samples.
- One `docker exec` containing a finite shell loop; at most 512 scanned processes
  and 32 observed worker/helper/Poppler processes per sample. Each process has its
  own row. No OCR HTTP requests, jobs, container startup, model loading, env or
  argv inspection, cgroup writes, or concurrency changes.
- Required cgroup v2 current/peak/max/swap, events/events.local, anon/file/kernel/
  shmem, CPU counters and memory pressure are validated. Missing mandatory fields,
  absent worker, protocol truncation, command failure or host observation failure
  returns nonzero. Optional unmeasured host process/GPU metrics remain `null`;
  missing measurements are never zero-filled.
- Optional Poppler/recyclable layout processes may exit between reading their
  name and status. Missing/partial status increments the explicit per-sample
  `observer.optional_processes_unavailable` counter; no zero-RSS process row is
  fabricated. An unreadable process name is outside the identified process set.
  A missing worker row or missing worker/cgroup required fields still fails the
  entire measurement. Process churn cannot emit an incidental shell redirection
  error and invalidate an otherwise complete sample.
- Separate `worker-memory.jsonl`, `host-memory.jsonl`, and `summary.json`.
  `umask 077`, private directory, exclusive file creation and mode 600; existing
  output files/symlinks are not overwritten. Host snapshots are approximately
  every second plus before/after; each contains actual timestamp/counters.
- Container samples use `/proc/uptime` rather than pretending the Docker exec
  client clock is the container clock. Actual sample count/elapsed time are
  reported; collection overhead can reduce count. The lifetime peak stays labeled
  as such and is not reported as a new per-job peak.

### Actual observer verification

Executed against the existing worker, with no workload:

```sh
python3 scripts/profile-worker-memory.py \
  --container rust-ocr-worker-1 \
  --output-dir artifacts/e02/collector-smoke-20261015 \
  --duration-seconds 2 --interval-ms 200
```

Result: exit 0, 10 complete worker samples, 4 host snapshots, elapsed 2.160 s.
Observed current maximum 1,850,068,992 bytes; lifetime peak remained
1,876,815,872 bytes. All memory event counters stayed zero. Host swap.used did not
change within this short observation. Directory mode was 700 and all three files
were mode 600. The helper still held approximately 1.59 GiB RSS. The two-second
result is a collector smoke check; it cannot establish a long-run plateau.

The observer itself adds small transient process/kernel allocations. Its current
maximum is not a new model inference peak. Host swap.used at this later reading
was approximately 10.90 GiB; the earlier snapshot above is kept distinct.

After adding initial/final current and event deltas to the summary, the final
collector revision was run again for two seconds into
`artifacts/e02/collector-smoke-v2-20261015`: exit 0, 10 worker samples, 4 host
snapshots, elapsed 2.137 s, sampled current peak 1,852,547,072 bytes, final current
1,845,497,856 bytes, unchanged lifetime peak and zero deltas for every memory event.
Host swap.used delta was zero. Permissions were checked again (directory 700,
files 600). Additional checks passed: Python syntax, three malformed/missing metric
frames rejected, interval 199 ms rejected before creating output, nonexistent
container returned exit 1 and a failed summary with zero samples. No successful
measurement was recorded for that failure.

Root review found a process-lifecycle race in the first observer: a short-lived
Poppler process could exit after its `comm` was read and cause `set -e` to abort
on status redirection. Status/name capture now uses guarded reads; optional
unavailable observations are counted, while worker/cgroup absence fails closed.
[`scripts/test_profile_worker_memory.py`](../scripts/test_profile_worker_memory.py)
executes the actual probe against synthetic `/proc`/cgroup files and covers
disappeared optional status, terminal partial status, disappearance before name
read, missing worker status/fields, missing cgroup fields and required parser
observability/worker rows. It makes no Docker/model requests.

All seven focused tests passed. The final race-fix probe was also exercised
against the existing worker for two seconds into
`artifacts/e02/collector-race-fix-v2-20261015`: exit 0, 10 samples, elapsed 2.201 s,
zero optional-unavailable observations and zero memory-event deltas. The lifetime
peak was unchanged. Both cgroup and host observation completed without running
OCR. This remains an observer check.

## 2a. 2026-10-16 headroom hold

A fresh read-only snapshot was taken before any allocator experiment. The live
worker cgroup was at 1,721,282,560 bytes current, 1,876,815,872 bytes lifetime
peak, 2,147,483,648 bytes maximum, zero `memory.events` counters, and zero cgroup
swap. Host swap was 12,992.25 MiB used of 14,336 MiB, with 1,343.75 MiB free;
`memory_pressure -Q` reported 27% system-wide free. The live worker container was
not changed and no ONNX session was started.

The one-off `layout-detect` binary now accepts `--cpu-arena on|off`. Omitting the
flag leaves the original `Session::builder()` defaults untouched; `off` explicitly
registers the CPU execution provider with its arena disabled. The worker's normal
helper launch does not pass this flag, so deployment behavior remains arena-on.
The option is only an experiment control, not a promoted runtime setting.

The A/B run was deliberately not started: a fresh ONNX process would add material
memory pressure while the host has only about 1.31 GiB swap free and the live
worker is already near its cgroup peak. The switch has compile/unit coverage, but
there are no arena-off measurements, output-parity results, or performance claims.
Rerun only after headroom is available, one fresh cgroup at a time, using the
baseline and off mode with the same pinned model, inputs, threads, and serialized
outputs. Stop on swap growth, memory events, or non-plateau behavior.

## 3. Simultaneous live buffers and hidden bounds

| Path | Allocations/lifetime that matter | Implication |
|---|---|---|
| [`preparation.rs:439`](../crates/ocr-worker/src/preparation.rs#L439) | Decoded image, optional thumbnail, `to_rgba8()` copy and new RGB coexist. | At 3500², RGB is 36.75 MB and RGBA is 49 MB. A consuming RGB fast path can preserve exact RGB output and avoid an RGBA pass; alpha inputs still need the current white composite. |
| [`layout_runner.rs:407`](../crates/ocr-worker/src/layout_runner.rs#L407) and [`main.rs:146`](../crates/ocr-layout/src/main.rs#L146) | Worker page RGB → scratch PNG → encoded PNG read + helper decode. Worker and helper simultaneously hold page RGB. | `Arc` clones are references, not raster copies; helper decoding is a real second image. Scratch is removed after decode at `main.rs:130`. |
| [`model.rs:49`](../crates/ocr-layout/src/model.rs#L49) | Input tensor is 7.68 MB. `out_masks` is 300×200×200×4 = **48 MB**, plus graph intermediates and outputs. | Small final layout JSON does not imply small graph allocation. |
| [`polygon.rs:99`](../crates/ocr-layout/src/polygon.rs#L99) | A selected mask is expanded to its page-space box and passed to contour tracing. | Contour working raster/points may exceed the small 200² input mask; review stress cases at configured geometry limits. |
| [`preparation.rs:548`](../crates/ocr-worker/src/preparation.rs#L548) | Crop copy and resized destination coexist; optional padding creates another canvas. | Maximum crop source pixels and inference crop pixels bound different allocations. |
| [`inference.rs:334`](../crates/ocr-worker/src/inference.rs#L334), [`inference.rs:658`](../crates/ocr-worker/src/inference.rs#L658) | Regions are buffered and cropped before acquiring the global model slot; up to R JPEG-bearing futures per job may wait. | More workers retain more pages/crops even with a serial model/layout slot. Do not derive memory as model R alone. |
| [`inference.rs:694`](../crates/ocr-worker/src/inference.rs#L694) | JPEG + base64 temporary + data-URL String + JSON Value + reqwest serialized HTTP body. The Value remains scoped across the network wait. | Build the request, explicitly drop the Value, then await sending; encode directly into a pre-sized data-URL String if measured worthwhile. |
| [`inference.rs:438`](../crates/ocr-worker/src/inference.rs#L438) | Clone raw regions, format, construct JSON pages, preserve raw/formatted regions and repeated Markdown. | Remove temporary copies only; raw evidence/output fields are part of the contract. |
| [`inference.rs:494`](../crates/ocr-worker/src/inference.rs#L494), [`post_processing.rs:87`](../crates/ocr-worker/src/post_processing.rs#L87) | Full `serde_json::to_vec` exists only to check a 16 MiB size limit. | A bounded counting writer avoids the additional full result allocation. |
| [`inference.rs:179`](../crates/ocr-worker/src/inference.rs#L179), [`post_processing.rs:68`](../crates/ocr-worker/src/post_processing.rs#L68) | Profile holds input and an OCR clone; post-processing clones source bytes again for digest computation in a blocking task. | Passing the already computed SHA or shared immutable bytes avoids source copies; the private PDF is small, the 50 MiB input envelope is not. |
| [`preparation.rs:820`](../crates/ocr-worker/src/preparation.rs#L820) | `.take(limit+1).read_to_end(Vec)` bounds length, not exact Vec capacity. | Do not describe an encoded length limit as an exact heap reservation ceiling. |

Page iteration is sequential, no all-page raster accumulation was found, and CPU
permits remain inside blocking closures through async cancellation. Those useful
invariants must remain intact during optimization.

## 4. Next experiment and promotion criteria

1. The default-preserving CPU arena experiment switch is implemented in
  `layout-detect --cpu-arena on|off`. The installed `ort 2.0.0-rc.11` exposes
  `ort::ep::CPU::default().with_arena_allocator(false).build()` via
  `SessionBuilder::with_execution_providers`. Test arena-on/pattern-on versus
  arena-off/pattern-on first, one fresh isolated cgroup at a time. Fixed
   1×3×800×800 input makes memory patterns potentially useful; do not disable two
   independent optimizations in one un-attributable experiment.
2. If needed test pattern-off separately. Arena-off can lower idle retention
   without lowering instantaneous activation needs, and may change latency.
   Nothing in this audit establishes the winning policy.
3. Shrink-on-run-end is a later option. The official RunOptions key is
   `memory.enable_memory_arena_shrinkage=cpu:0`; CPU arena must remain enabled.
   Default next-power-of-two growth excludes its initial region from shrink, so
   blindly adding the key may provide little benefit. Shared allocators do not
   share across separate helper processes. [Official run option source](https://github.com/microsoft/onnxruntime/blob/main/include/onnxruntime/core/session/onnxruntime_run_options_config_keys.h).
4. Use the same pinned image/model, threads 2/1, source pixels, preprocessing and
   output serialization. Fresh cgroups make `memory.peak` comparable without
   resetting production counters. Network-none layout-only tests need no Qwen
   requests. Collect ready/idle, three synthetic smoke fixtures, repeated-page
   warm runs, low/medium/high region fixtures, maximum envelope stress and idle
   tail. Do not run two helpers concurrently on this already pressured host.
5. Require identical serialized layout, tensor/crop/JPEG hashes where applicable,
   bounded memory/threads and explainable warm latency changes. After choosing a
   policy, rerun the private PDF end to end and compare source digest plus raw
   Markdown/pages/layout/extraction with M1; record execution policy separately.
6. Stop on OOM/max events, sustained host swap growth, heartbeat failures or
   non-plateau memory. Do not increase W/R, pool, limit or native model memory.
   The proposed 75–80% worker headroom is an experimental gate; proving the full
   workload envelope requires more than one PDF or a short synthetic run.

**Still open:** allocator A/B, measurements over 6–12 development documents,
maximum-envelope stress, reliable native model/Docker VM physical-footprint and
runtime allocation evidence, output parity after any optimization, and E03/E04
arrival-rate/capacity tests. This audit does not clear those gates.
