# Local Qwen3.5-4B OCR on Apple Silicon

The API, Rust worker/layout detector, Redis, and PostgreSQL stay in Linux Docker containers. Real inference runs on the host Mac with the official vLLM Metal plugin; the Linux Docker VM cannot access Apple's Metal GPU. Redis Streams carries job IDs, while PostgreSQL retains source bytes, jobs, results, and the recurring delivery outbox. See [queue and persistence](redis-postgres.md) for recovery behavior.

The selected checkpoint is `mlx-community/Qwen3.5-4B-MLX-4bit`, a 4-bit MLX conversion of Qwen3.5-4B with vision weights, pinned at revision `32f3e8ecf65426fc3306969496342d504bfa13f3`. The public API alias is `Qwen/Qwen3.5-4B`. Native image support is experimental in vLLM Metal 0.31.0; do not treat a successful smoke test as production quality proof. [Pinned support matrix](https://raw.githubusercontent.com/vllm-project/vllm-metal/v0.31.0/docs/supported_models.md).

## Reproduce installation

Prerequisites: an Apple Silicon Mac with macOS 15+, Docker Desktop, `uv`, and native Python 3.12. The installer uses an isolated environment at `~/.local/share/ocr-service/vllm-metal-0.31.0`, installs the matching official vLLM/core wheels, and caches the approximately 3 GB model in Hugging Face's local cache. It does not modify the Rust API/worker into Python programs.

```sh
./scripts/install-vllm-metal-macos.sh

# For a fresh setup; preserve an existing .env containing your settings.
cp .env.macos.example .env
```

Replace the API/model credential placeholders with different random values and set a random hex `REDIS_PASSWORD`; the example expands it into matching host/container Redis URLs. `.env` is ignored by Git. Preserve existing private credentials when running the following commands.

```sh
# Terminal 1: native GPU inference. First startup can take several minutes.
./scripts/serve-vllm-metal-macos.sh

# Terminal 2: Docker application services, without the demo override.
bash scripts/setup-layout-model.sh
docker compose up --build -d --wait
set -a
. ./.env
set +a
./scripts/real-e2e.sh
```

The inference server binds to `127.0.0.1:8000` by default. If the Docker worker cannot reach the host, set `VLLM_BIND_ADDR=0.0.0.0` before starting it; keep `VLLM_API_KEY` configured. The worker calls `http://host.docker.internal:8000/v1`, while host-side checks use `http://127.0.0.1:8000/v1`.

## Local budgets

The local profile processes one document and one recognition request at a time. The model context is 4,096 tokens with up to 2,048 output tokens per region. The earlier full-page runs used 1 MP and 1.5 MP model image caps; the higher cap corrected one shortened identifier. The current macOS profile uses 1,572,864 pixels; the serving script falls back to 1,048,576 when no value is configured. These settings fit the tested 16 GiB M4 Mac; OCR quality still needs a labeled corpus. A truncated region fails the complete job explicitly. Higher image/output/context limits need additional memory.

The Rust worker renders PDF pages at 200 DPI, capped at 3,500 pixels on the longest edge. PP-DocLayoutV3 runs inside a Rust native helper with the ONNX Runtime CPU library packaged in Docker. It produces learned reading order and mask polygons; Rust then makes task-specific JPEG crops with a 2,000-pixel edge bound before sending them to Qwen. The model processor applies its own pixel cap to each crop. Model export uses a separate, pinned Python environment once; there is no Python consumer or Python layout server in the Docker stack.

The model server disables hybrid prefix caching because that support remains experimental, disables thinking for transcription, and uses the native multimodal path. The host's RAM also serves macOS, other apps, Docker, and model caches; runtime profiling may shrink its KV budget or reject startup when there is insufficient free memory.

The macOS example sets `TABLE_CROP_PADDING_PIXELS=112`: Rust adds a white border only to Table crops after the course crop/resize and before JPEG encoding. The default service value is 0. Results record the value and padding stage in `recognition_preprocessing`, and nonzero padding adds `+table-padding` to the layout profile. This is an explicit recognition enhancement; detector boxes/polygons and Text/Formula crops keep the course contract. The [layout report](layout-e2e-report.md) documents the strict Table hallucination/truncation failure and isolated padding comparison. One improved crop does not establish full-document or general table accuracy.

The worker's 300-second request timeout and 1,200-second document timeout are longer than the demo defaults. Redis commands have a separate 2-second deadline and independent backoff; PostgreSQL lease heartbeats continue during OCR. Compose accepts `WORKER_STOP_GRACE_PERIOD=1210s` so a shutdown can drain a document within that budget. Model installation and serving are host processes; `docker compose down` stops the application, Redis, and database. Stop a foreground inference server with Ctrl-C.

## User documents

Submit a PDF through the same API used by other services. The default `course_layout_vlm` strategy renders every page, detects layout, and sends eligible cropped regions to the local model. It does not use an existing PDF text layer as OCR output. The original upload is retained in PostgreSQL for course jobs alongside its SHA-256; the caller's file is preserved. New worker binaries do not prune automatically while hold-aware maintenance is being implemented; the legacy `RETENTION_DAYS` setting does not schedule deletion. Results contain Markdown, normalized boxes/polygons, raw and formatted region text, model/profile metadata and coverage counts. Actual-model results and comparisons stay in ignored `artifacts/`, especially for documents containing personal details.

The default `OCR_RECOGNIZE_IMAGES=false` skips detected images/charts and emits positional placeholders. To read text inside outermost image/chart containers, set `OCR_RECOGNIZE_IMAGES=true` in `.env`, then recreate the worker with `docker compose up -d --no-build worker`. The result records the enhanced image-text profile, combined with `+table-padding` when enabled, and `image_ocr_policy.name: "outermost_containers_v1"`. An image/chart box at least 98% covered by a larger image/chart box receives no separate OCR call; equal areas choose the earlier detection index. Nested regions retain placeholders, null raw content, and `task_type: "skip"`. Each page lists their original detection indices in `image_ocr_skipped_nested_regions`, so a nonzero `coverage.skipped_regions` count is expected even with the override enabled. Disabled image OCR records `course_skip_all` and an empty nested-index list.

The earlier all-image implementation correctly read critical card fields but invented QR/fingerprint narrative from nested crops. Keep that result identified as an older failure case. The current policy avoids separate prompts for those nested image regions while retaining them inside the outer crop; the [layout report](layout-e2e-report.md) records actual run outcomes. Other course-abandoned classes remain visible in `discarded`. Inspect coverage and policy metadata instead of assuming a completed job read every visible word accurately.

```sh
curl --fail-with-body http://localhost:8080/v1/ocr/jobs \
  -H "Authorization: Bearer $OCR_API_TOKEN" \
  -H 'Idempotency-Key: my-document-v1' \
  -F 'file=@/absolute/path/document.pdf'
```

Poll `/v1/ocr/jobs/<returned-id>` with the same bearer credential. The API can accept durable jobs during a Redis outage, but delivery waits for Redis recovery; the worker does not fall back to direct database polling. API readiness checks PostgreSQL only. See the current [layout end-to-end report](layout-e2e-report.md), the earlier [full-page baseline](real-e2e-report.md), and [runtime research](vllm-metal-research.md). `scripts/real-e2e.sh` verifies the configured worker strategy and exact text; a failed assertion remains a failed quality test even if the durable job completed. Native integration tests require both `TEST_DATABASE_URL` and `REDIS_URL` for the running Docker services.
