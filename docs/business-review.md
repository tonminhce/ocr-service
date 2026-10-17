# Compact business API and durable human review

The original `GET /v1/ocr/jobs/{id}` contract is compatible and can still return
large results. New clients should poll `/status`, retrieve `/business` once,
and fetch original OCR/evidence artifacts only when needed. These are additive
views; neither corrections nor approval modify raw OCR, Markdown, layout,
extraction candidates, or the source PDF.

## Read endpoints

All require the existing `OCR_API_TOKEN` bearer credential. The deployment has
one shared service trust domain. These endpoints do not provide tenant isolation.

| Endpoint suffix under `/v1/ocr/jobs/{id}` | Response |
| --- | --- |
| `/status` | Job status/attempts/times/profile/error, result availability, latest review revision/status. Its SQL explicitly excludes `result` and `input`; polling does not decompress or transfer OCR JSON. |
| `/business` | `ocr-business/1.0.0`: compact aggregate fields, candidates grouped by typed value **within each field**, a deduplicated evidence registry, role evidence references, issue codes, and current human review snapshot. Requires successful structured extraction. |
| `/artifacts/source` | Original PDF/image if its input bytes were retained; safe fixed attachment filename. |
| `/artifacts/markdown` | Original raw Markdown, `text/markdown`. |
| `/artifacts/layout` | Original layout JSON. |
| `/artifacts/evidence` | Original full extraction JSON, including separate contract/card observations. |
| `/artifacts/diagnostics` | Post-processing diagnostics/native comparison if present. |
| `/artifacts/result` | Original complete OCR result JSON. |

Successful responses have `Cache-Control: private, no-store` and
`X-Content-Type-Options: nosniff`. Artifact downloads use `Content-Disposition:
attachment` with fixed filenames, never the uploaded filename. Unknown, missing,
unavailable and pruned artifacts return 404; no filesystem paths are accepted.
Source responses are bounded at 50 MiB; other artifact/view JSON at 16 MiB.
By default, eight large read operations per API process are admitted
concurrently (`RESPONSE_LARGE_CONCURRENCY`, returning 429 when busy), while
compact status responses use a separate small-response admission budget
(`RESPONSE_SMALL_CONCURRENCY`, default 32) so polling is not starved by large
artifact reads. Projection and large JSON serialization run on blocking tasks so
they do not occupy Tokio asynchronous executor threads; a shared
`RESPONSE_SERIALIZATION_CONCURRENCY` semaphore (default 4 large slots, plus a
fixed two small-serialization slots) bounds that CPU work, and the blocking
closure owns its permit until it exits, including after its request waiter is
cancelled. Large responses reserve their full byte budget on admission before
database reads; the reservation is trimmed to the bytes actually serialized
once the body owns them, and egress copies fixed 16 KiB chunks. This admission
limit bounds active handlers, not all bytes already buffered by slow network
clients, whose transfer is bounded separately by the body timeout.

For exactly one landlord and tenant, field keys are stable semantic paths such
as `tenant.birth_date`, `landlord.id_number`, `monthly_rent`, `deposit_amount`,
`property_address`. Duplicate or unknown roles use `party:<id>.<field>` and
cannot pass the current approval policy. Evidence IDs are SHA-256 of the complete
original evidence object, including its source, UTF-8 span and block geometry.
`source_sha256` binds API business output to the immutable uploaded-byte digest; extraction-only replay has null because it lacks original bytes.
The registry groups identical evidence, and candidate grouping preserves all
origins, methods and evidence references. It does not merge party scopes.

`state` always reports the **machine's original state**. `value` is the original
selected value until a human correction exists; `correction` then records the
explicit human replacement and its cited evidence. `approval` distinguishes
`requires_review`, `human_corrected_requires_review`, and `human_approved`.
Candidates and raw state remain visible after a human decision. A recovery-only proposal remains `ambiguous` with no selected value even when it contains just one candidate; `recovery_only_observation` explains the special case. It requires explicit evidence-linked correction before approval. Recovery context never counts as independent first-pass corroboration. A machine
`found` field never grants approval. Every machine decision remains
`needs_review`, even after a separately recorded human decision.

Reason codes add distinctions without changing the extraction profile:
`confidence_uncalibrated`, `source_conflict`, `observed_empty_value`,
`observed_placeholder`, `invalid_or_unsupported_format`, and state summaries.
An observed empty anchor describes text evidence only; it is **not** proof that
the pixels in the source were blank. Confidence remains uncalibrated; no score
or detector probability has been promoted to extraction accuracy.

## Privileged review boundary

Review is disabled (404) unless both `OCR_REVIEW_TOKEN` and
`OCR_REVIEW_PRINCIPAL` are configured. Empty values disable the pair. The
standalone environment-only default is therefore disabled; the production
Compose deployment configures both from secret files, which enables the
privileged review endpoints there. The review
token must differ from the service token and contain at least 32 printable ASCII
characters without spaces. The configured principal contains 1..128 UTF-8 bytes
without control characters. The ordinary OCR service credential cannot mutate
or read the review audit API.

This credential authenticates a **review service principal**, not an individual
human. Request JSON cannot assert an authenticated actor; unknown top-level
fields are rejected. In deployment, a trusted review application must authenticate
its human operators and enforce its own access rules before using this privileged
credential. Per-user OIDC, tenant authorization, a review UI, and multi-person
approval are not implemented. Keep these credentials in a secret manager outside
source control; review stays disabled until both credentials are actually
configured in the deployment environment.

`GET /v1/ocr/jobs/{id}/review?after_revision=0` returns the current snapshot and
up to 100 append-only events, with `next_after_revision` and `has_more` for
pagination. Each event includes its trusted principal, database timestamp,
revision, action, reason, canonical request and resulting status. Events do not
repeat cumulative snapshots in the response; the latest snapshot appears once.

`POST /v1/ocr/jobs/{id}/review` requires a JSON body of at most 64 KiB:

```json
{
  "request_id": "11111111-1111-4111-8111-111111111111",
  "expected_revision": 0,
  "action": "correct",
  "reason": "Compared the conflicting birth date with the original attachment.",
  "corrections": {
    "tenant.birth_date": {
      "value": {"type": "date", "value": "2004-01-04"},
      "evidence_refs": ["ev_<use-an-existing-reference-from-this-field>"]
    }
  }
}
```

Actions are `correct`, `approve`, `reject`, and `reopen`:

- `correct` requires 1..64 known field paths. Each value must match the field type
  and carry 1..32 unique existing references from that field's own evidence.
  Values are validated conservatively: real ISO calendar dates, numeric 9/12-digit
  identity strings, bounded text, nonnegative plain decimal VND amounts with the
  expected period, and explicit m2 area units. The corrected value is a human
  assertion supported by cited OCR/native text; the service does not prove it
  visually or verify identity authenticity.
- `approve` accepts no corrections in that request. It requires a supported
  template, exactly two parties with one unique landlord and tenant, explicit
  corrections for **every aggregate conflict or ambiguous field**, and resolved,
  typed, evidence-linked values for both parties' name, identity number, birth
  date, issue date and residence address, plus monthly rent and property address.
  Other unresolved optional fields remain visible. This is a fixed, conservative
  profile policy, not a general legal approval or signature verification.
- `reject` records rejection without deleting observations. `reopen` revokes
  approval/rejection. Any later correction also returns status to `needs_review`;
  a separate explicit approval is required again. Corrections accumulate, and
  replacing a previous correction creates another revision.

`expected_revision` is an optimistic concurrency check. Two requests expecting
revision N cannot both create N+1. Stale revisions and a reused request ID with a
different canonical request/principal return 409. Replaying an identical request
ID returns **that original event's snapshot**, even if a later revision exists;
read `/status` or `/review` for the latest state. Requests are capped at 60 KiB
serialized, reasons at 2000 bytes, cumulative snapshots at 900 KiB, and 10,000
revisions per job. Four review operations per API process are admitted; others
receive 429. Review write transactions have a 2-second PostgreSQL lock timeout
and a 5-second statement timeout.

Events commit transactionally and are immutable through the application and a
PostgreSQL trigger. They are **not tamper-proof against the database owner**.
Retention deletes the job and cascades its review history. Review does not touch
`jobs.updated_at`, so it does not extend source retention; external archiving or
a separate legal retention policy is needed where records must outlive OCR jobs.

The supplied contract's source contains a blank landlord date of birth and
cross-source identity discrepancies. It must continue to require review; the
service does not invent missing required values to make approval pass.

## Verification

Unit tests cover value normalization boundaries, evidence scoping, candidate
and span deduplication, unresolved critical fields and conflict approval guards.
PostgreSQL tests use isolated schemas and cover revision races, request replay,
append-only protection, unchanged original results, cascading retention, explicit
correction → separate approval → reopening, and compact status against malformed
legacy result JSON. HTTP tests cover privileged authentication, disabled review,
forged actor rejection, payload bounds, response privacy/attachment headers,
compact views and artifacts, replay, and stale-revision responses.

```sh
cargo test -p ocr-core -p ocr-api
TEST_DATABASE_URL=postgres://ocr:ocr-local@localhost:5432/ocr \
  cargo test -p ocr-core --test review -p ocr-api --test http -- --ignored
```
