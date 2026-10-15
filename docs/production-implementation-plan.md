# Production implementation plan

Started 15 October 2026. Target: Rust API/consumer, Docker, Redis delivery and PostgreSQL source of truth. Preserve raw course-style layout/crop/recognition output and existing API compatibility.

## Delivery sequence

1. **Consumer contract and review:** compact status/business responses, separately fetched source/artifacts, evidence references, precise reason codes, privileged append-only review/corrections with optimistic revisions.
2. **Measurement:** bounded Prometheus metrics, stage latency, queue age, outcomes, retries, model/cache/recovery counters; separate protected exporters, no document text in metrics.
3. **Detector lifecycle:** warm bounded Rust helper pool, pinned model initialized once per helper, request limits, cancellation/crash kill/reap and restart.
4. **Retry efficiency and recovery:** lease-fenced PostgreSQL region checkpoints with explicit deployment revision, bounded crop recovery preserving first-pass raw output and disagreements.
5. **Evaluation:** Rust offline corpus runner/scorer, development/calibration/test separation, exact field/role/state/evidence and selective-acceptance reporting; reproducible concurrency sweep with explicit model/backend configuration.
6. **Integration and validation:** workspace checks, real PostgreSQL/Redis tests, Linux Docker build, actual PDF run and measured before/after where comparable; source/privacy invariants and review APIs tested.

Optional model KIE will be evaluated as a separate profile/candidate source rather than replacing scoped rules blindly. Automatic acceptance remains disabled until a representative independently labeled corpus and calibrated policy establish its error target. No fabricated confidence or production accuracy claim will substitute for this evidence.

## Model KIE capability check

A local synthetic-only probe on the deployed Qwen3.5-4B / vLLM Metal 0.31.0
returned the expected `{value, block_id}` JSON with `finish_reason=stop` for
both `structured_outputs.json` and `response_format.json_schema` (512-token
budget, thinking disabled). The first exploratory probe had failed JSON parsing;
that alone did not establish missing backend support. The successful probe is
saved in ignored `artifacts/research/structured-output-probe.json`.

This demonstrates the two example requests working. It does not establish
grammar enforcement, factual extraction, role attribution or confidence
calibration. Generative KIE is not enabled in the worker. A separately labeled
comparison must measure its evidence accuracy and errors before deployment.
[vLLM structured output documentation](https://docs.vllm.ai/en/latest/features/structured_outputs/)
describes the supported request forms for the upstream server;
[vLLM Metal](https://github.com/vllm-project/vllm-metal) is a separate hardware
plugin and must be capability-tested in its deployed configuration. Bounded
blocking CPU work follows [Tokio's synchronous/asynchronous boundary](https://tokio.rs/tokio/topics/bridging).

## Completion tracking

- [x] Compact contract, artifact access, durable review
- [x] Metrics and stage timing
- [x] Warm detector pool
- [x] Fenced checkpoints and bounded recovery
- [x] Evaluation and benchmark tools
- [x] Full integration, documentation and runnable instructions

Completed implementation and integration evidence is recorded in [production-pass-validation.md](production-pass-validation.md). A passing software test or a successful run on one PDF does not establish general field accuracy. Automatic acceptance/calibration and deployment-specific load targets remain gated on independently labeled data and measurement.
