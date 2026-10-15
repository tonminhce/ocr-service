# Region retry checkpoints and bounded recovery

Both features are opt-in. The recognizer, consumer and post-processing remain
Rust. PostgreSQL owns durable job and checkpoint state; Redis Streams carries
notifications. Neither feature changes the first-pass raw OCR/Markdown/layout.

## Retry checkpoints

```dotenv
INFERENCE_REVISION=your-immutable-model-runtime-configuration-revision
REGION_CHECKPOINTS_ENABLED=true
```

The worker fingerprints the actual crop JPEG, task, model name, endpoint,
prompt/sampling versions, output budget, detector hash, rendering/crop settings,
and `INFERENCE_REVISION`. An exact match within the **same uploaded job** can
reuse successful recognition after a transient failure or worker restart.
There is no shared cross-job or cross-caller cache.

Set the deployment revision to identify weights, tokenizer, chat template,
runtime and model-server settings. Change it whenever these change, even if
the served model alias stays the same. The service cannot discover hidden
server-side changes automatically. A nonempty bounded ASCII revision is required
when checkpoints are enabled.

Reads and writes require the current, unexpired PostgreSQL lease token. Writes
lock the parent job before checking the token, so an old worker cannot populate
the cache after another worker takes over. Stored text has its own SHA-256;
a corrupt entry is ignored. Immutable cache entries are bounded to 128 KiB
each, 4096 entries and 16 MiB text per job. The checkpoint table cascades when
retention deletes the job. Full caches skip further saves; a database failure
or lost lease fails the attempt rather than bypassing fencing. Calls have a
three-second database timeout.

Checkpoints avoid repeated model work. Rendering, layout and extraction still
run again. Execution remains at least once: a crash between model response and
checkpoint commit can repeat a call. A corrupt immutable entry currently remains
a miss until job retention removes it. Credentials are never fingerprinted.

Lease renewal and checkpoint processing are polled concurrently. Renewal must
not stop polling a checkpoint transaction that already holds the same job row.
A deterministic PostgreSQL lock-graph regression reproduces the old deadlock
and verifies completion in one attempt with one model call after the fix.
Renewal, success and failure writes also acquire the job lock first, then check
the current wall-clock expiry in a separate statement; merely checking a
predicate before a lock wait can wrongly authorize a lease that expired while
waiting. Those writes use transaction-local two-second lock and five-second
statement timeouts.

## Recovery observations

```dotenv
REGION_RECOVERY_MAX_REGIONS=4
REGION_RECOVERY_EXPANSION_PIXELS=16
```

The default budget is zero. The maximum is 16 extra text regions per document;
padding is 1..128 rendered-page pixels. Recovery targets an empty text response
or a bounded list of suspicious Vietnamese OCR phrases. A low detector score
or literal `#` alone is not a trigger. The worker expands the detected rectangle,
clips it to the page and uses the same bounded model admission/checkpoint path.

`result.recovery.observations` records the original page/region, expanded box,
trigger, method and alternative text or failure. `automatically_selected=false`
and `original_unchanged=true` describe this policy. Recovery does not rewrite
`raw_regions`, formatted layout or Markdown. A failed recovery leaves the
original result usable; job cancellation still stops processing. Alternative
text shares the document's 4 MiB transcription budget.

For structured extraction, successful alternatives enter a separate complete
`ocr_recovery_context` stream. Reading order and role context are retained so a
cropped date cannot silently inherit another party's scope. The original OCR
and native PDF streams remain separate. Recovery-only field candidates require
review and are unselected; different observations remain unresolved. Matching
retries are not independent corroboration. Expanded crops can include adjacent
text, so a human correction must still check the original document.

This feature does not recover entirely missed detector regions, verify blank
pixels, validate digital signatures, establish factual accuracy, or select an
identity value when contract and attached ID disagree. Model-based KIE remains
a separately evaluated future candidate source; this implementation uses the
explicit `vi_rental_contract_v1` rules.

## Concurrency and measurement

`WORKER_CONCURRENCY` bounds documents and CPU preparation within a worker;
`LAYOUT_POOL_SIZE` bounds warm detector helpers and must not exceed it.
`REGION_CONCURRENCY` is the shared model-request bound across that process,
including recovery and full-page calls. Cache hits bypass model admission.
Multiple replicas multiply this bound. Start with the backend's actual capacity
(the supplied Mac profile uses one model sequence), then measure successful
throughput, p95 latency, failures, memory and field quality before increasing it.

CPU crop/format/native-text/extraction work uses bounded blocking tasks;
asynchronous HTTP waits use Tokio. A cancelled blocking task retains its permit
until the bounded CPU work exits. More operating-system threads do not increase
a saturated model server's capacity. See [metrics](observability.md),
[warm detector measurements](warm-layout.md) and [evaluation](evaluation.md).

Synthetic PostgreSQL tests cover expired/stale leases, cache corruption,
cross-job isolation, per-job budgets and retention. A synthetic model failure
test saves two completed crops, fails the third, then confirms a retry makes
four total upstream calls instead of six. A changed inference revision forces
fresh calls. These are reliability checks, not OCR accuracy measurements.
