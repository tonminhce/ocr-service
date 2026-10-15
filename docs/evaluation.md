# Field evaluation and bounded end-to-end benchmarking

`ocr-eval` is a Rust tool for scoring **independent manual labels** against saved predictions and for running bounded API load experiments. It does not fit confidence scores, activate automatic acceptance, generate ground truth from predictions, or change worker/model concurrency.

The checked-in corpus under `crates/ocr-eval/fixtures/synthetic` deliberately contains a wrong rent, an unsupported deposit candidate and a correctly retained date conflict. Its purpose is to test evaluator arithmetic and integrity checks. **It is synthetic, manually assembled data, not an OCR/model accuracy benchmark.**

## Run the synthetic integrity example

```sh
cargo run --locked -p ocr-eval -- score \
  crates/ocr-eval/fixtures/synthetic/manifest.json --split test
```

The expected rent precision/recall are zero; tenant birth-date conflict recall is one. These deliberately small results verify mechanics only. Regenerate the committed synthetic files after an intentional fixture/schema change:

```sh
cargo run --locked -p ocr-eval --example synthetic_fixture
```

## Create a private labelled corpus

Put private source documents, saved predictions and the manifest together below an ignored directory such as `artifacts/evaluation/`. Keep this directory and generated reports access controlled. Use opaque IDs instead of people's names in manifests. Scoring reports contain aggregate counts and semantic field names, not extracted values or evidence quotes. Manifests and prediction files can contain personal data.

Each manifest uses schema `ocr-eval-manifest/1.0.0`, a dataset name, a `synthetic` boolean and 1–1000 documents. A document has these fields:

| Field | Contract |
| --- | --- |
| `id` | Unique opaque ID, 1–128 ASCII letters/digits/underscore/hyphen/period |
| `split` | `dev`, `calibration`, or `test` |
| `group_id` | Related documents, revisions, images of the same source and near-duplicates share this ID |
| `source_path` | Relative path to the original file, rooted at the manifest directory |
| `source_sha256` | Lowercase SHA-256 of the exact source bytes; verified before use |
| `result_path` | Optional relative saved prediction path; required for scoring |
| `result_sha256` | Required with `result_path`; freezes the exact prediction artifact |
| `labels` | Manually supplied map from stable business paths to expected state/value sets |
| `roles` | Optional manually supplied party-ID → role expectations |

The loader rejects exact duplicate source hashes, duplicate IDs and any group crossing splits. It rejects absolute paths, traversal and symlinks escaping the manifest directory. Manifest size is limited to 8 MiB, source files to 32 MiB and prediction files to 16 MiB. Each document has at most 128 labels and 16 role labels. Declared source hashes and prediction hashes are verified; embedded source hashes in full OCR/API outputs must also agree. Saved compact projections without an embedded source hash still require an operator-attested source/prediction mapping; an artifact hash alone cannot establish that relationship. Exact hashing cannot detect a resized scan or edited near-duplicate, so grouping remains an explicit curation responsibility.

Use `dev` to change parsing rules/prompts/recovery triggers. Reserve `calibration` for fitting a future confidence/acceptance model and choosing thresholds. Keep `test` untouched until changes and policies are frozen. Every page, revision, attachment and transformed copy from a related source belongs in the same split. Stratify document families, scanners, language, blur, handwriting, blanks, conflicts and unsupported templates. Have annotators inspect the original rendered source independently from model predictions and adjudicate disagreements. Do not mark a source-level inconsistency as an OCR error.

Labels use the same typed normalized values as extraction. Examples:

```json
{
  "landlord.birth_date": {
    "state": "found",
    "values": [{"type": "date", "value": "1980-01-01"}]
  },
  "tenant.birth_date": {
    "state": "conflict",
    "values": [
      {"type": "date", "value": "2000-02-02"},
      {"type": "date", "value": "2001-03-03"}
    ],
    "evidence_gold": [
      {"page_number": 1, "source": "region_ocr", "quote": "02/02/2000"}
    ]
  },
  "monthly_rent": {
    "state": "found",
    "values": [{"type": "money", "amount": "1000000", "currency": "VND", "period": "month"}]
  },
  "deposit_amount": {"state": "missing", "values": []}
}
```

`found` requires exactly one expected value. `conflict`/`ambiguous` require 2–16 distinct normalized values. `missing`/`unreadable` require no expected values. IDs remain strings. Supported paths are the global rent/deposit/property fields and `landlord`/`tenant` name, identifier, dates, residence/contact address, phone and bank account. Type/field mismatches are rejected.

Saved predictions can be a full polled API job, an OCR result with `extraction`, an `ExtractionResult`, or a compact `BusinessResult`. Raw extraction is converted using the same `ocr-core` business projection as the API. Unknown schemas, dangling evidence references, inconsistent evidence hashes/spans and numeric uncalibrated field/evidence confidence are rejected. Layout detection scores remain separate and are never used as field correctness probabilities.

## Understand the score report

For every labelled semantic field the report gives state accuracy counts and **exact normalized state-plus-value-set** counts. Text comparison collapses whitespace and lowercases while preserving accents; IDs remain strings; ISO dates must be calendar-valid; decimal amounts remove insignificant zeros while retaining currency, period and unit. There is no fuzzy name/address matching, accent removal, locale guessing or automatic legal correction.

A true positive is an exact labelled value-bearing field decision, including its state. A wrong predicted value-bearing decision is a false positive; a missed/wrong expected value-bearing decision is a false negative. Thus a wrong value counts as both FP and FN. Correctly absent fields are recorded in exact/state counts, outside precision/recall's value-bearing denominator. Precision or recall with no denominator is `null`, not an invented perfect score. Reports also retain these separate measures:

- Conflict **state recall** versus **exact conflict recall**: recognizing disagreement does not prove that the retained candidates are right.
- Unexpected normalized candidate values, plus fields labelled missing that still have candidates. These counts apply only to manually labelled fields; they do not establish hallucination rate for unlabelled fields.
- Role correctness when explicit party-ID role labels are provided.
- Evidence availability and structural integrity, separately from manual exact quote/page/source matching. A valid hash and UTF-8 span cannot prove the quote matches visible pixels or belongs to the right semantic field. Manual quote matching is an annotation check, not cryptographic/pixel verification.
- Human approval coverage and observed human-approved error rate. Human corrections do not inflate machine scores: raw states/candidates are scored independently. Approved conflicts/ambiguous fields without an adjudicated single truth are excluded from the approved-error denominator.

The machine acceptance policy remains disabled. A human-approved error rate describes the measured review process; it is not machine auto-acceptance precision. A tiny sample with no observed errors cannot justify a low error-rate guarantee. Before setting acceptance thresholds, collect independently adjudicated calibration data, fit and evaluate the calibration model, predeclare field risk tolerances and report confidence intervals on the held-out test split. This tool deliberately does not invent probabilities from detector scores or LLM self-ratings.

## Run an end-to-end API benchmark

Benchmarking submits actual source files and may invoke the configured real OCR model. It requires `OCR_API_TOKEN` in the environment and accepts no token command-line flag. Each run uses fresh idempotency keys. Use a manifest whose sources are actual supported PDF/PNG/JPEG/WebP documents; the text-only synthetic scorer fixture is unsuitable for a real inference benchmark. A benchmark manifest may omit labels and prediction paths.

```sh
cargo run --release --locked -p ocr-eval -- benchmark \
  artifacts/evaluation/manifest.json --split test \
  --base-url http://127.0.0.1:8080 --clients 1 \
  --worker-concurrency 1 --region-concurrency 1 \
  --deadline-seconds 600 --profile vi_rental_contract_v1
```

The client is bounded to 1–4 concurrent documents, 5–3600 seconds per document, four concurrent input read/hash tasks, 15-second HTTP requests and fixed response byte limits. It polls only `/v1/ocr/jobs/{id}/status`, every 500 ms while a job is queued/processing. It never downloads large OCR/layout artifacts during throughput measurement. Failed/rejected submissions, server job failures and timeouts remain in the report. Success latency p50/p95 uses nearest rank; throughput counts only successful documents per measured wall-clock minute. Latencies include input reading/hash verification, upload, queueing, inference, post-processing, persistence and polling. Polling resolution and client input I/O therefore affect results.

`--worker-concurrency` and `--region-concurrency` are **operator-declared metadata**. The report sets `runtime_configuration_verified=false`; this client does not silently change settings or claim that they were verified. Set the actual Docker worker/model configuration separately, record the image/model/layout revisions and host configuration, and attach the corresponding metrics snapshot to each experiment. Compare a baseline W=1/R=1 against bounded alternatives under the same document corpus and backend. Keep repetitions and warm-up behavior consistent. Do not compare a warm native Metal run directly with a cold GPU vLLM run as if backend and resource conditions were identical.

After a timeout or client cancellation, already accepted durable server jobs continue. Per-document report entries retain accepted job UUIDs for inspection. Wait for them to finish before starting another configuration so runs do not overlap. Correlate results with [stage metrics](observability.md), model queue/latency/tokens, host RSS/CPU/GPU and error rates. Higher concurrency is useful only when successful throughput improves within the required memory and tail-latency budgets. This runner does not implement server-side cancellation, continuous arrival-rate generation, distributed load, confidence calibration or automatic tuning.

## Verification and primary sources

Tests cover split leakage, duplicate identities, path/symlink escape, invalid/empty datasets, input size/hash changes, malformed evidence, arbitrary confidence rejection, exact conflict scoring, human corrections, and a real local HTTP fixture proving bounded client concurrency and compact status polling. HTTP fixtures do not run OCR inference and their throughput must not be presented as model performance.

The methodology is informed by [Google Document AI field evaluation](https://docs.cloud.google.com/document-ai/docs/evaluate), its separation of precision/recall and confidence thresholds, and scikit-learn's guidance on [data leakage](https://scikit-learn.org/stable/common_pitfalls.html) and [group-aware evaluation](https://scikit-learn.org/stable/modules/cross_validation.html). These motivate independent labels and split isolation; the exact metric contract above belongs to this evaluator and is not claimed to reproduce a vendor's scoring implementation.

## Explicit v2 evaluation

`score-v2 MANIFEST --split dev|calibration|test` uses the independent source-grounded contract in [annotation-guide.md](annotation-guide.md) and [evaluation-manifest-v2.schema.json](../schemas/evaluation-manifest-v2.schema.json). V1 `score`/`benchmark` semantics are preserved. V2 requires a machine-only prediction sidecar with actual page transforms, independent entity anchors, per-candidate source identity and transcript spans; runtime v1 artifacts are not silently upgraded. The guide documents the real-data and adapter gates still open.
