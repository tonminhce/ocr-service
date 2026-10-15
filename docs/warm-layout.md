# Persistent Rust layout helpers

The consumer retains a bounded `LayoutPool` across documents. Each helper loads
and checks the pinned PP-DocLayoutV3 model once, retains its ONNX session, and
handles pages sequentially. There is no Python layout worker or layout HTTP
service. The original `layout-detect --image ...` command remains supported.

`LAYOUT_POOL_SIZE` defaults to **1**, accepts 1–8, and cannot exceed
`WORKER_CONCURRENCY`. This is a process bound per consumer replica, independent
of regional OCR concurrency. Each session keeps the existing ONNX intra-op=2,
inter-op=1 configuration. Increasing pool size increases resident model/session
memory and CPU demand; choose it using measured throughput, latency and memory.
ONNX documents the interaction between session thread pools and parallel
sessions in its [threading guide](https://onnxruntime.ai/docs/performance/tune-performance/threading.html).

## Isolation and cancellation

A fair Tokio semaphore controls acquisition; each actor has a one-item channel
and one private scratch directory. Requests contain a protocol version, a
monotonic ID, a PNG basename and expected dimensions. The helper rejects paths
outside its scratch directory, symlinks, unsupported versions and mismatched
image dimensions. Frames are NDJSON: at most 4 KiB/request and 1 MiB/response;
helper stderr is drained with a cumulative 64 KiB lifetime limit. Model weights
and SHA-256 stay pinned as before. No API credentials or document text are sent
in the protocol.

Timeout, lease/job cancellation, a dropped caller future, helper EOF/crash,
malformed or oversized output and invalid geometry retire that helper. The
actor kills **and waits for** the process before releasing its pool permit.
The next request starts a fresh process. Idle helper exits are also reaped.
Before retaining each page's layout, the consumer counts both active and
discarded detections against `MAX_REGIONS_PER_DOCUMENT` and enforces a separate
cumulative **4 MiB serialized layout metadata** budget. Exact JSON counting runs
in a bounded blocking task through a counting writer, without allocating a
second serialized page. Large discarded-only pages cannot bypass these limits;
the final complete-result limit remains 16 MiB.
PNG encoding runs on the bounded blocking pool; cancellation cannot stop a
running encoding closure, so its CPU permit and scratch directory remain owned
until the closure finishes.

This deliberately goes beyond only setting `kill_on_drop`: Tokio documents
that processes normally outlive dropped handles and that explicit waiting is
needed for reaping. See [Tokio Child](https://docs.rs/tokio/latest/tokio/process/struct.Child.html)
and [fair semaphore acquisition](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html).
A retained `LayoutPool` exposes `shutdown().await` for graceful process cleanup
before runtime exit. Dropping its final owner signals cleanup while the runtime
is still running; abrupt runtime/process termination cannot provide the same
awaited-cleanup guarantee.

## Measurements

`detect_with_timings` returns independent queue, cold startup, PNG encoding and
request/inference durations. Startup is zero for reused sessions. The worker's
metrics can report cold cost separately from warm inference latency.

The local layout-only benchmark verifies identical output across cold and warm
calls and prints timing metadata, never source pixels or transcription:

```sh
DATABASE_URL=postgres://unused \
LAYOUT_BINARY="$PWD/target/release/layout-detect" \
LAYOUT_MODEL_PATH="$PWD/artifacts/models/pp-doclayout-v3/model.onnx" \
LAYOUT_MODEL_SHA256=98798e25d88318d44c68cb0d63242026b6006246bd83390365d894d459efb43c \
ORT_DYLIB_PATH=/absolute/path/to/libonnxruntime.dylib \
cargo run --release --locked -p ocr-worker --example benchmark_layout_pool -- \
  /absolute/path/to/page.png 5
```

This compares sequential layout calls, including request PNG encoding. It does
not call Qwen/vLLM, measure document-level throughput or establish OCR accuracy.
A warm layout improvement must not be reported as the same end-to-end speedup.

The 2026-10-15 native macOS release diagnostic used ONNX Runtime 1.23.2,
one 1449×2048 saved page (35 regions), five warm calls and five independent
cold helpers. Cold median was **1.580 s**, warm median **1.195 s** (1.32×
layout-only speedup); the first ready handshake took 0.642 s. Every output was
byte-identical. These small-sample timings were collected during development
and are not a latency/throughput SLA. Machine-readable evidence is in the
ignored local artifact `artifacts/benchmarks/layout-pool-native.json`.

Tests use executable protocol fixtures to cover process reuse, pool bound,
queued cancellation, in-flight cancellation, aborted callers, deadline/restart,
oversized/malformed/wrong-ID responses and cleanup of idle processes. The
existing pipeline fixtures still verify geometry and class/task validation.
