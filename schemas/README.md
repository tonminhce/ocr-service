# Extraction contracts

## Implemented scoped Rust rental extraction

`rental-extraction-v1.schema.json` describes the implemented `ocr_extraction::ExtractionResult` in `crates/ocr-extraction/src/lib.rs`, embedded under the OCR result's `extraction` property. Its schema version is `rental-extraction/1.0.0`. Validate that property against this schema; it is not the schema for the complete OCR job or `layout.json`.

The current `vi_rental_contract_v1` profile uses explicit Vietnamese rental-contract anchors, explicit landlord/tenant role headings, conservative typed normalization, and identity-card observations. It retains contract values, card values, and combined candidates separately. Identity attachments link only through an exact, unambiguous observed identifier; this is not identity or document authentication. Front/back observations remain separate records. Candidate methods distinguish `explicit_anchor_rule` from `restricted_idvnm_mrz_rule`. The narrowly supported back-side MRZ path checks a document-number check digit and identifier consistency; it does not validate the entire MRZ.

The implemented machine-extraction contract is deliberately smaller than the general design below. It has no automatic acceptance, calibrated confidence, word-level geometry, or generic document registry. Every current extraction result has `decision: "needs_review"`, including supported templates and apparently agreeing fields. The separate [privileged review API](../docs/business-review.md) stores reviewer decisions and corrections in an append-only audit without rewriting that machine result. The profile, normalizer, and review policy versions are pinned by this schema. Numeric extraction confidence is forbidden: field, candidate, and evidence confidence must all be null.

| Runtime field state | Structural requirement |
| --- | --- |
| `found` | A typed selected value and at least one evidence-backed candidate. |
| `missing` | Null selected value, no candidates, and no anchored observations. |
| `unreadable` | Null selected value, no candidates, and at least one anchored observation. A nonempty exact anchor-line quote documents an empty printed/OCR value. |
| `ambiguous` or `conflict` | Null selected value and at least two distinct candidate values. A special `ambiguous` case retains one or more exclusively unverified `ocr_recovery_context` candidates with `recovery_only_observation`; no primary source supports selection. `conflict` identifies incompatible primary source observations. |

Identifiers remain strings, preserving leading zeros. Dates are ISO calendar dates; enable format checking when validating JSON Schema. Money and area use decimal strings. The current rule profile emits explicit `VND`, nullable/month billing periods, and `m2` area units only when the observed text supports them. Suspect Vietnamese spellings and literal `#` characters produce review issues; correction suggestions never replace original OCR.

Evidence names a source block and uses exact, zero-based UTF-8 byte offsets with an exclusive end. `bbox_2d` belongs to the complete source block and retains that source's coordinate convention; it is never a measured box around the extracted word. Layout detection scores remain separately labeled `layout_detection_not_ocr_confidence`. Native PDF text and rendered OCR remain distinct `source` streams.

If an extraction budget is exceeded, the result contains a single `extraction_budget_exceeded` issue, `supported_template: false`, and empty source-block, party, business-field, and identity-document collections. The schema enforces that shape so a partial budget-exhausted result cannot expose selected business values.

### Runtime checks beyond this JSON Schema

Structural validation cannot establish source accuracy or complete business validity. Rust must resolve unique block/party/document references, recompute block SHA-256 and byte lengths from immutable text, verify UTF-8 boundaries and exact quote slices, compare span endpoints, and validate nondegenerate geometry in its source convention. It must verify selected values against candidates, semantic field types, normalized candidate distinctness, source/role attribution, and identifier linkage. A structurally valid conflict can still have duplicate semantic values if those runtime checks are skipped. A hash-shaped string is not proof of provenance, and matching identifiers are not proof of authenticity.

Cross-source disagreements retain each candidate without choosing the contract or card automatically. Observed disagreements require review; they do not alone establish which source is correct, a forged identity, or an invalid contract. Unsupported anchors/templates and text omitted by every input source still need OCR recovery or human review.

### Verification of the implemented schema

On 15 October 2026, `jsonschema.Draft202012Validator` with `FormatChecker` accepted the schema, five synthetic structural field-state cases, and the private real-run artifact's `extraction` property. Nineteen independently mutated synthetic cases were rejected, covering automatic acceptance, numeric confidence at all three levels, invalid state/selection/candidate combinations, missing candidate evidence, numeric identifiers, unsupported geometry/confidence claims, invalid digest shape, zero-based source pages, invalid box tuple length, an invalid calendar date, and an unexpected property. After the restricted MRZ method and fail-closed budgets were added, the fresh private extraction including a `restricted_idvnm_mrz_rule` candidate passed again. An empty budget-exhaustion result passed, and six mutations attempting to retain values, sources, documents, template support, or extra issues were rejected. No real document values are included in these schema fixtures or this documentation.

These checks establish schema shape only. Quote/hash/reference integrity, candidate comparison, normalization semantics, and extraction behavior require the Rust validation and extraction tests.

## Proposed general field extraction envelope (design only)

The two `field-extraction-v1.*` files are **design artifacts for a broader future extraction stage**. They do not describe an implemented HTTP endpoint. The implemented scoped contract above does not implement every registry, review workflow, or field represented in this broader proposal.

- `field-extraction-v1.schema.json` defines a Draft 2020-12 evidence envelope.
- `field-extraction-v1.example.json` contains explicitly fictional Vietnamese text and identifiers. Its model metadata says `synthetic/no-model-used`. It demonstrates a rental-contract section, a separate identity attachment, uncertainty, and a review decision.

The envelope keeps OCR artifacts, logical documents, party roles, field interpretations, validation issues, review decisions, and model/prompt/policy versions separate. Semantic paths for repeated people include stable role IDs, for example `contract.parties.landlord_1.name`; `role_id` references that person. A second landlord must receive another role ID and field path, rather than overwrite the first entry.

### Rules JSON Schema enforces

| Field state | Required behavior |
| --- | --- |
| `found` and `extracted` | Nonempty raw value, typed normalized value, at least one located OCR quote, no unresolved candidates, and no derivation record. |
| `found` and `derived` | Raw value is null; typed normalized value and a derivation rule with input field references are present. |
| `missing` or `unreadable` | Raw/normalized values, confidence, and derivation are null; candidates are empty. Evidence may be present without inventing a value. |
| `ambiguous` or `conflict` | No selected raw/normalized value or confidence; at least two candidates and one validation issue. |

Identifiers are strings, including leading zeros. Money uses a decimal string, nullable currency, and an explicit billing period. A normalized date is a complete ISO date; validators must enable date-format checking, and Rust must still parse the calendar date.

Confidence is null or a score accompanied by `calibration_record_id`. A polygon is null or explicitly marked verified with a verification record. Those fields provide structural requirements; an invented record ID or `verified=true` does not prove anything.

An `accepted` envelope cannot contain ambiguous/conflicting fields or error-level validation issues. Missing optional fields may remain. The schema cannot infer which fields are critical to a particular workflow, so an accepted shape is not sufficient evidence of an acceptable business result.

### Required Rust validation beyond JSON Schema

These obligations remain proposed implementation work:

1. **References and identity.** Reject duplicate JSON map keys before they are silently overwritten. Enforce unique artifact, document, role, region, and candidate IDs in their intended scopes. Resolve every reference. A field's role must belong to its logical document and match the semantic path/profile. Classifications and roles require evidence; do not assume `Bên A` always means landlord across every contract template, or that an attached ID holder is necessarily a contractual party.
2. **Page and region membership.** Original page numbers are one-based. Evidence pages must match their artifacts and belong to their logical documents. Region page numbers must match their scopes. If a field names a region, that region must belong to the field's logical document and contain its supporting evidence. Check shared-page scopes as described below.
3. **Artifact hashes.** Fetch the immutable artifact by ID and verify SHA-256 over its exact original bytes. For inline OCR text in the example, these are the UTF-8 bytes including whitespace and newlines. The schema verifies only the digest's hexadecimal shape. Do not hash reconstructed or normalized text as a substitute for the referenced artifact.
4. **UTF-8 bounds and exact quotes.** Spans are end-exclusive byte offsets within the referenced page OCR artifact. Require `0 <= start < end <= byte_length`, valid UTF-8 character boundaries, and an exact decoded slice equal to `quote`. Offsets into global document text, UTF-16 strings, or normalized text are different and cannot be substituted. Verify all field, candidate, and logical-region quotes. This establishes evidence in OCR text; it does not prove that OCR matches the scanned pixels.
5. **Raw and normalized alignment.** An extracted raw value must be supported by its source evidence and the correct label/role context. Independently validate the semantic field's expected type and normalizer output. Preserve identifier zeros, diacritics, original date spelling, currency uncertainty, and rent-versus-deposit roles. A generic typed object can pass the JSON Schema while still being the wrong type or value for a particular field. Never manufacture missing date components or digits to satisfy a schema.
6. **Derivation provenance.** Resolve input fields, reject cycles, and apply an allowed versioned rule. A rationale string is not a verified derivation. Derived values must remain distinguishable from directly extracted values, and cannot receive fabricated quotes asserting that the inferred value was printed.
7. **Genuine geometry.** Resolve `verification_record_id` to a real coordinate-producing stage or recorded reviewer action. Verify page identity, nondegenerate polygon geometry, point ordering, and transformation from rendered/cropped/rotated images to the original page. Coordinates use `[x,y]` in `[0,1]`, origin top-left. The Markdown-only path provides no measured boxes; keep polygons null until geometry exists.
8. **Known calibration.** Resolve `calibration_record_id` to a trusted record matching the model revision/quantization, field/task, score method, and representative evaluation data. A Qwen self-rating is not calibrated confidence. Missing confidence can remain null, including after a valid human or deterministic policy review.
9. **Critical-field acceptance.** Apply the versioned business profile and policy to every required critical field. Check unreadable/missing fields, conflicting role assignments, identity mismatches, candidate distinctness, and invalid normalization. Validate the review record or deterministic decision source; do not trust an LLM-generated `accepted` decision. A technically valid envelope may still need review or rejection.
10. **Version consistency.** Check the envelope/schema version, business profile, model revision, prompts, normalizer, and policy against the execution that produced the result. Include result-affecting versions/options in extraction caching and idempotency. Preserve original OCR and reviewer corrections separately.

### Multiple documents or ID sides on one page

Page membership is not exclusive. Two logical documents may both list original page 4, provided **validated distinguishable regions** identify their individual scopes. The schema's `logical_documents[].regions` allows a region ID, page number, side classification through `front`/`back`/`unspecified`, and evidence-backed scope. A scope must contain a located OCR quote or a genuine verified polygon; an unlocated empty region is rejected.

Front and back of **one** identity card belong to one logical document with two regions, even if both are scanned onto one physical page. The example now demonstrates this on page 4. Its front/back labels are synthetic text labels, and its scopes are exact UTF-8 text spans; all image polygons remain null.

Two **independent** documents/cards on a shared page require separate logical document IDs and distinguishable scopes. Their fields should name the appropriate region, or the validator must resolve a unique contained region. If regions are absent, the listed pages imply whole-page scope; two such whole-page claims are insufficient for safely attributing values. Do not automatically accept a shared-page extraction when document ownership remains ambiguous.

JSON Schema permits the overlap and validates region shape, but cannot prove that one card's name belongs to a particular region. Text spans can separate OCR content without proving image coordinates or reading order. Geometry or manual review may still be required by the business policy. This is a precise limit of the proposed contract, rather than a reason to invent boxes.

### Verification performed

On 14 October 2026, Python `jsonschema.Draft202012Validator` with a `FormatChecker` validated the schema and synthetic example. The following independently mutated examples were rejected:

- A found extracted field with no evidence.
- An identifier whose normalized value was a JSON number.
- An accepted envelope containing a conflict, with other error causes removed to isolate the decision rule.
- A confidence score without a calibration record ID.
- A logical region containing neither a located text span nor a verified polygon.

A synthetic positive case with two separately scoped logical documents on original page 4 passed structural validation. The persisted example's source hashes, artifact page references, and all exact UTF-8 quote spans, including front/back region scopes, were checked against its inline fictional text.

These checks validate the design's structural behavior. They do not test an implemented extraction API, genuine calibration/geometry registries, business-policy acceptance, or OCR accuracy on real contracts or CCCD documents.
