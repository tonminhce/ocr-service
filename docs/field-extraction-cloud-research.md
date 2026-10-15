# Production field extraction: cloud patterns and a Rust service contract

Research date: **14 October 2026**. Sources below are official vendor documentation and API references. This is read-only research: no paid API calls were made and no contract or identity-document bytes were uploaded. Proposed Rust behavior is explicitly distinguished from vendor capabilities; this document does not claim that field extraction has been implemented or evaluated.

## Main finding

OCR Markdown is a transcription output. Production field extraction adds a versioned semantic schema, document/party roles, typed normalization, traceable evidence, validation, uncertainty handling, and evaluation. The useful patterns from cloud products are those contracts and controls, rather than assuming their language support or accuracy transfers to Qwen3.5-4B in 4-bit precision.

For a Vietnamese rental-contract bundle with a CCCD attachment on page 4, the extraction problem is larger than finding every label called `Họ tên`, `Địa chỉ`, or `Số`. The contract's landlord/tenant roles, the property's address, and the attachment holder's identity must remain distinct. An identity-card holder cannot become the landlord or tenant simply because their name appears later in the file.

## Actual language and model boundaries

| Product and mode | Officially documented fit for Vietnamese | Material limitation |
| --- | --- | --- |
| Google Enterprise Document OCR / Form Parser | Vietnamese `vi` is listed. | Generic key-value detection does not establish rental-contract party roles or a custom business schema. |
| Google Custom Extractor, non-generative custom models | Vietnamese appears in the processor's language list. | Requires the appropriate custom model/dataset; support differs by training mode. |
| Google Custom Extractor, generative foundation models | **Only English is officially supported** in the processor notes. | Do not turn multilingual OCR support into a claim that generative Vietnamese extraction is officially supported. |
| Azure Document Intelligence v4.0 custom classifier / custom neural printed text | Both lists explicitly include Vietnamese `vi`. | The custom-neural handwriting list does **not** include Vietnamese. |
| Azure prebuilt ID, v4.0 `2024-11-30` | Documentation describes worldwide identification documents, including identification cards in other regions. | That broad scope is not a published accuracy guarantee for a particular Vietnamese CCCD generation, scan, or front/back arrangement. Validate the precise input type. |
| Amazon Textract text/forms | English, French, German, Italian, Portuguese, and Spanish; **Vietnamese is not listed**. | Queries are English-only; AnalyzeID supports US passports and US driver licenses. Use its response design as an example, rather than recommending it as supported Vietnamese CCCD extraction. |

Sources: [Google processor list and mode-specific language notes](https://docs.cloud.google.com/document-ai/docs/processors-list), [Azure custom-model language tables](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/language-support/custom?view=doc-intel-4.0.0), [Azure v4 ID model scope](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/prebuilt/id-document?view=doc-intel-4.0.0), [AWS documented limits and languages](https://docs.aws.amazon.com/textract/latest/dg/limits-document.html).

Version discipline matters: Google uses the v1 API plus separately versioned processors, Azure examples here use the GA API `2024-11-30`, and AWS's current documentation describes operations whose returned model version should be recorded. Azure's ID page contains older-version material alongside its newer v4 worldwide scope; apply the v4-specific scope rather than its legacy US-only passages.

## What the vendors' output contracts preserve

**Google:** `Document.Entity` separates the schema `type`, original `mentionText`, optional `normalizedValue`, `confidence`, text anchors, page anchors, and nested `properties`. A page anchor can contain multiple references/polygons. Confidence is optional and ranges from 0 to 1; normalized values can be absent when conversion/type support is unavailable. [Document v1 reference](https://docs.cloud.google.com/document-ai/docs/reference/rest/v1/Document).

The schema declares entity properties, semantic value types, and occurrence/cardinality rules. This allows a repeated party/contact/item to be represented as a collection rather than overwriting one scalar field. Google also distinguishes direct extraction from derived values: derived fields infer information that is not explicitly written, and their text/page anchors are not populated. Signature presence is a derived visual feature, not evidence that the signer's identity or legal authorization was verified. [DocumentSchema v1](https://docs.cloud.google.com/document-ai/docs/reference/rest/v1/DocumentSchema), [derived fields and signature detection](https://docs.cloud.google.com/document-ai/docs/ce-derived-signature).

**Azure:** the GA analysis result represents fields with a semantic `type`, original `content`, a corresponding typed value, `spans`, `boundingRegions`, and optional confidence. Types include strings, dates, numbers, integers, currency/address structures, arrays, and nested objects. Its result also keeps document types and page/word structures. [Get Analyze Result, API 2024-11-30](https://learn.microsoft.com/en-us/rest/api/aiservices/document-models/get-analyze-result?view=rest-aiservices-v4.0%20(2024-11-30)).

**AWS:** form extraction returns connected `KEY_VALUE_SET` blocks with `KEY`/`VALUE` roles and links to underlying word blocks. The pair's confidence is shared by its key and value; word confidence is different. Query responses carry an alias, answer, confidence, and location; no answer produces a blank response. These are detection results rather than an automatically application-specific typed schema. [Form data relationships](https://docs.aws.amazon.com/textract/latest/dg/how-it-works-kvp.html), [Queries](https://docs.aws.amazon.com/textract/latest/dg/queryresponse.html).

The lesson is to preserve source text, interpretation, structure, and evidence separately. A normalized date must not replace the scanned spelling; a field's semantic correctness must not be represented by the OCR confidence of its characters.

## Splitting a bundle and keeping roles correct

Google splitters represent document classification and page membership through entities/page anchors; their page references are **zero-based**, and the JSON representation can omit a zero-valued page field. Azure custom classification can return page ranges for multiple document instances. In v4, splitting defaults to `none`; request `auto` to split a mixed bundle, or `perPage` when each page is independently a document. [Google splitters](https://docs.cloud.google.com/document-ai/docs/splitters), [Azure custom classification](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/train/custom-classifier?view=doc-intel-4.0.0).

AWS queries accept an explicit `Pages` selection; omitting it targets page 1, not the entire bundle. This is another reason page scope must be part of an extraction contract. [AWS Query API](https://docs.aws.amazon.com/textract/latest/APIReference/API_Query.html).

Recommended behavior for this service:

1. Keep a stable original page number, and record logical sections such as `rental_contract` and `identity_attachment`. Do not discard the original-to-section mapping.
2. Extract contract parties with explicit roles from headings/clauses such as `Bên A` and `Bên B`. Support multiple parties and representatives through arrays, rather than assuming exactly one person per role.
3. Extract the CCCD holder into a separate attachment entity. Preserve card-side/page scope and unreadable or absent fields.
4. Reconcile the attachment with a contract party using explicit matching evidence. Treat discrepancies as conflicts and an unlinked attachment as unlinked, rather than quietly preferring the last occurrence.
5. Keep `property.address`, a party's declared residential address, and an ID's printed address as separate fields. Repeated identical strings are not proof that their semantic roles are interchangeable.

Azure custom neural extraction supports repeating/tabular structures and cross-page tables, but its documented ordinary-field limitation is that values split across page boundaries are not recognized. It also limits overlapping labels to two fields per token. Those are model-specific constraints, not general permission to merge every cross-page mention. [Custom neural capabilities and limitations](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/train/custom-neural?view=doc-intel-4.0.0).

## Confidence, validation, and human review

Google evaluates predictions against annotated test documents using per-label precision/recall/F1. A threshold can increase precision while reducing recall; its automatically selected threshold maximizes F1, which may differ from the business goal for a critical identity or payment field. Repeated-label matching and fuzzy matching affect results, so record matching rules alongside metrics. [Google evaluation](https://docs.cloud.google.com/document-ai/docs/evaluate).

Azure documents estimated confidence at word, field, and document/structure levels and advises examining the underlying OCR as well as field extraction. Not every field has a confidence value, and custom neural/generative models do not provide training accuracy scores. A training score from visually similar forms is not proof of performance on new scans. [Azure accuracy and confidence](https://learn.microsoft.com/en-us/azure/ai-services/document-intelligence/concept/accuracy-confidence?view=doc-intel-4.0.0).

AWS advises application-specific thresholds and human scrutiny for uncertain results. Its A2I integration can trigger review for missing important keys, pair/word confidence conditions, and sampled high-confidence outputs. Sampling accepted outputs is useful for finding systematic mistakes that a confidence threshold misses. [AWS best practices](https://docs.aws.amazon.com/textract/latest/dg/textract-best-practices.html), [A2I activation rules](https://docs.aws.amazon.com/sagemaker/latest/dg/a2i-json-humantaskactivationconditions-textract-example.html).

Do not build a new dependency on Google's former managed Human in the Loop product: its deprecation page lists HITL as deprecated from January 16, 2024. A review workflow can still be implemented in the calling application, with evidence and corrections stored separately. [Google deprecations](https://docs.cloud.google.com/document-ai/docs/deprecation).

For our Qwen pipeline, a model-generated `0.97` is not an evaluated probability. Return confidence as `null` until a score exists with a stated meaning and validation. Even vendor scores should be checked on our Vietnamese corpus before adopting thresholds; the same numeric score across providers is not evidence of comparable performance. Route critical missing, unreadable, conflicting, or unsupported fields for review. Store reviewer corrections as corrections with actor/time/evidence, without rewriting the immutable original OCR.

## Proposed extraction contract for the Rust service

This is a proposal, not current API output:

```json
{
  "schema_version": "rental_contract_v1",
  "logical_documents": [
    {"id": "contract", "kind": "rental_contract", "pages": [1, 2, 3]},
    {"id": "attachment_1", "kind": "identity_attachment", "pages": [4]}
  ],
  "parties": [
    {
      "party_id": "party_a_1",
      "role": "landlord",
      "name": {
        "status": "found",
        "raw_value": "<exact OCR text>",
        "normalized_value": "<validated text>",
        "method": "extract",
        "confidence": null,
        "evidence": [
          {
            "logical_document_id": "contract",
            "page": 1,
            "source": "ocr_text",
            "quote": "<exact source quote including role context>",
            "span": {"start": 0, "end": 0, "unit": "utf8_bytes"},
            "polygon": null
          }
        ]
      }
    }
  ],
  "identity_attachments": [],
  "conflicts": [],
  "review_required": false,
  "extraction_metadata": {
    "model_checkpoint": "<exact checkpoint and revision>",
    "ocr_prompt_version": "<version>",
    "extraction_prompt_version": "<version>",
    "normalizer_version": "<version>"
  }
}
```

The placeholder span is illustrative and must be replaced with verified offsets, not emitted as `0..0` for a real field. Specify the page-number convention and offset unit once; Vietnamese Unicode makes unlabelled byte/character/UTF-16 offsets unsafe to exchange. Verify Rust UTF-8 boundaries before slicing. Exact quotes/spans ground a result in the OCR text; they do **not** verify that OCR text against pixels.

Use `found`, `missing`, `unreadable`, and `ambiguous` states rather than treating every absence as an empty string. Keep competing candidates and their evidence when role or value selection is ambiguous. Put inferred values under `method=derived` with derivation inputs; never attach fabricated direct-extraction evidence to an inferred value.

Represent identity/document/phone/account numbers as strings to preserve leading zeros. Represent money as a validated decimal string plus currency and billing period; distinguish rent, deposit, utilities, and totals. Preserve date spelling and only produce an ISO date when parsing is unambiguous. Avoid guessing a missing day/month, adding a currency solely from the document's language, or treating signature presence as identity verification.

The earlier Markdown-only path has no measured polygons. The default Rust layout pipeline added on 15 October 2026 provides detector region polygons and raw text, but still has no per-word OCR confidence or typed field evidence links. Its detector score is a layout-class score, not the probability that an identifier was read correctly. Extraction should reference measured regions; use null where field-specific geometry/confidence is unavailable. Schema-constrained JSON helps output shape; it does not prove a field value, role assignment or source quote. Verify types, allowed keys, source scope, exact evidence and semantic consistency in Rust after generation.

## Production acceptance and next implementation step

Create a representative labeled Vietnamese test set with varied rental templates, multiple parties/representatives, attached IDs, scan blur/rotation, handwritten blanks, repeated values, missing fields, cross-page clauses, and deliberately conflicting identities. Separate evaluation documents from prompt examples or training data.

Measure per-field exact-match precision/recall, role attribution errors, missing-field hallucinations, evidence correctness, and normalization correctness. Separately measure the rate of documents whose **all critical fields** are correct, review volume, and errors among auto-accepted documents. Use strict matching for identifier digits; retain diacritics and distinguish reasonable name/address normalization from exact source transcription. Track results per template, scan quality, and document kind, not only a global average.

Keep OCR output immutable, and version extraction schema, model revision/quantization, prompts, normalization, and validation policy. Any extraction cache/idempotency fingerprint must include options that affect the result, so the same document processed with a different schema/model does not accidentally replay old fields.

The practical next step is a second extraction stage over page-scoped OCR plus attachment classification, followed by deterministic Rust validation and an evidence-rich JSON result. Establish those semantics and a small labeled benchmark before claiming production reliability or selecting a cloud replacement. No provider was called on the user's documents during this research.
