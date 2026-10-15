# Structured extraction in the Rust worker

The optional `vi_rental_contract_v1` profile implements a scoped KIE stage after the existing layout/recognition/formatter pipeline. It runs in Rust with PostgreSQL persistence and Redis delivery. It uses deterministic, explicit-anchor rules; it does not invoke a second generative model. It is a review-oriented implementation, not a claim of general production accuracy.

## Request and persisted contract

`POST /v1/ocr/jobs` accepts one `file` and an optional `extraction_profile` (`none` by default). The profile is stored with the job and participates in idempotency: changing it under the same key returns 409. Existing jobs migrate to `none`. The original input is retained for extraction jobs until configured terminal retention, including the legacy full-page strategy.

The completed result adds:

- `post_processing.blocks`: complete text blocks with source IDs, one-based pages, optional layout indices, native/OCR provenance and whole-block coordinates.
- `post_processing.source_text_sha256`: exact UTF-8 block hashes. `original_sha256` covers uploaded bytes.
- `post_processing.native_comparison`: geometry-matched alternatives and native text outside OCR coverage, without rewriting raw text.
- `post_processing.raw_ocr_quality`: original OCR blocks and review alerts for suspicious phrases, placeholder symbols and low layout scores.
- `post_processing.markdown`: a native-text projection on screened pages and OCR on others, for review. The extraction evidence still includes both streams.
- `extraction`: versioned typed fields, scoped parties, identity documents, candidates, evidence, issues and `decision=needs_review`.

`result.layout`, per-page OCR and `result.markdown` remain unchanged. Execution status and business review decisions are independent. Other services must check `extraction.decision`; automatic acceptance remains disabled. The [durable review API](business-review.md) records a separate human correction/approval state.

## Evidence and normalization

Explicit headings establish landlord/tenant roles. Designators alone do not imply roles. Later signature headings cannot erase those roles or reopen party-details scope. Contract and identity-card observations stay separate. An attachment is linked only through an exact, unambiguous identifier; matching names alone is insufficient. The supported back-side MRZ rule checks its restricted structure and document-number checksum; it is not a general MRZ verifier or authenticity check.

Fields use `found`, `missing`, `unreadable`, `ambiguous` or `conflict`. Conflicts have no selected value and retain each candidate. An exclusively recovery-context proposal remains `ambiguous` and unselected even when only one candidate exists; `recovery_only_observation` requires explicit human correction. Repeated expanded-crop context does not count as independent primary-source corroboration. Dates must parse as calendar dates. Identifiers retain leading zeroes. Money uses decimal strings with explicit VND currency and, for monthly rent, an explicit monthly period. No currency, digits, dates or units are invented.

Each evidence quote is checked against a real block using end-exclusive UTF-8 byte offsets. The worker independently verifies references, page/source membership, hashes, geometry and field-state consistency before committing the result. The [runtime schema](../schemas/rental-extraction-v1.schema.json) checks shape and state constraints; it cannot establish that text matches scanned pixels.

Confidence remains null. Layout detection scores are labelled as such. Whole-block boxes are never presented as precise word/field boxes. A source `#` may be a literal placeholder or OCR error: it triggers review and is not changed into m²/m³. Lexical suggestions are alerts, not applied corrections.

## Native PDF text and bounded work

Poppler `pdftotext -bbox-layout -cropbox` supplies native words/geometry in a bounded subprocess. The parser rejects entities, nonfinite geometry, invalid coordinates and tiny lines. Same-baseline fragments are joined into rows. Native text becomes parallel evidence only when both native-to-OCR and OCR-to-native token-set agreement reach 0.65 and the native page has at least 10 unique tokens. All OCR blocks are retained, including raster objects on mixed pages. Divergent extracted values remain unresolved candidates.

This screening is heuristic: hidden text of normal size, damaged encodings, complex columns, rotated fragments and misleading native layers can still escape detection. Every page assessment explicitly records `pixel_verified=false` and review required. Visible signature names/times can be recovered as text; cryptographic signature verification is a separate, unimplemented stage.

Subprocess output, lifetime, source count/text and serialized results have independent bounds. Extraction runs on a bounded blocking pool so SQL heartbeats and job deadlines continue. Over-budget extraction returns no business values with an explicit `extraction_budget_exceeded` review issue. It never truncates candidates into a falsely selected value.

The extraction preflight allows at most 2,048 blocks, 1 MiB total text, 128 KiB per block, 10,000 logical lines, 256 hash symbols, 256 semantic anchors, 64 anchors per field, 16 parties and 32 identity observations. Review issues are capped at 512; exceeding that cap clears business output. Native comparison has a separate 100,000-pair budget, reported as `comparison_budget_exceeded` rather than silently emitting a partial comparison.

## Replay without repeating OCR

```sh
cargo run --locked -p ocr-worker --example postprocess_layout -- \
  /absolute/path/original.pdf /absolute/path/ocr-result.json > /private/structured-result.json
```

This runs the same Rust post-processing against an existing result and its original input. It is not a new model-inference run or an HTTP job. For end-to-end execution, submit the profile through the API after rebuilding Docker.

## Remaining deployment and accuracy work

See the [production-readiness and performance audit](extraction-production-readiness.md) for the current score/JSON findings, actual concurrency, offline post-processing diagnostic and prioritized changes backed by primary sources.

Evaluate a labeled held-out collection across template variants, scans, multiple cards/parties and missing/contradictory values. Measure exact per-field values, role assignment, evidence accuracy, conflict recall and unsafe selections. Current rules intentionally leave unsupported structures unresolved; no one-document result establishes production accuracy. Compact business/artifact endpoints, durable review corrections/audit, a conservative human approval policy, bounded expanded-crop observations, metrics and evaluation tooling are implemented. A reviewer UI, precise field crops, tenant ownership, native pixel verification and an independently evaluated generative KIE profile remain future work. See [checkpoints/recovery](checkpoints-recovery.md), [evaluation](evaluation.md), and [current validation](validation.md).

## Earlier validation, before the production implementation pass

See [current validation](validation.md) for the later implementation and fresh checks.

- Formatting and strict workspace Clippy passed. The full normal suite passed 94 tests; the separate integration suite passed all 33 PostgreSQL/Redis/Poppler tests, including API profile persistence, idempotency, legacy migration, actual worker profile execution and retained input.
- The final Linux ARM64 Docker image built and the four Compose services ran. A fresh private four-page PDF job traversed HTTP submission, PostgreSQL/outbox, Redis Streams, Rust detector/worker, actual Qwen3.5-4B inference and result polling. It succeeded on attempt 1 in 169.03 seconds with 86 regional recognition requests.
- The polled job validated against OpenAPI and its extraction against the runtime JSON Schema. All 194 source-block hashes and 126 evidence spans were checked. The retained 637,111-byte original matched its recorded SHA-256; the job outbox and Redis stream/pending count were empty after commit.
- Two explicit party roles, monthly rent, deposit and property address were extracted. The tenant's birth date, issue date and residence address each remained a two-value cross-source conflict with no selected value. The blank landlord birth date and ambiguous area remained unreadable. Critical candidate values matched the separately reviewed replay.
- Seven suspect Vietnamese phrase alerts remained attached to raw OCR. Native text recovered both visible signature timestamps, filtered tiny signature anchors, and supplied a reviewer projection while raw OCR stayed unchanged. Separate source inspection confirmed literal hash placeholders and a blank birth-date value in the original; those were not automatically repaired.

Private outputs are ignored by Git under `artifacts/user-pdf/`: `extraction.json`, `ocr-result-structured-e2e.json`, `postprocessed.md` and `structured-e2e-validation.json`. The sample establishes this execution and these targeted checks, not held-out field accuracy or production acceptance.
