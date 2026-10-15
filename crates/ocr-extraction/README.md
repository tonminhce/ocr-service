# Scoped rental-contract extraction

`extract(&[SourceBlock])` returns typed candidate fields and evidence for the
`vi_rental_contract_v1` profile. It accepts OCR and optionally native PDF text
blocks; it never mutates either source. Rust worker/API integration supplies and
retains the actual blocks referenced by the returned source manifest.

This is an explicit-anchor baseline, not a general contract model or a measured
production-accuracy claim. Every result has `decision: needs_review` and null
field/candidate confidence. `supported_template` means the required rental title
and explicit landlord/tenant headings were recognized, not that content is
correct or accepted. Unrecognized templates produce a review issue.

## Covered fields

- Explicit party-role headings, including reversed A/B roles; signature labels
  alone do not change roles or reopen a party-detail section.
- Party name, national identifier, birth/issue dates, residence/contact address,
  phone and bank-account number under recognized anchors.
- Explicit property address, area with a recognized square-metre unit, monthly
  rent with VND and monthly period, and deposit with VND.
- Bilingual ID-card front/back blocks. Attachments link only through an exact
  unique contract identifier. Name-only matching and proximity are not used.
- A restricted IDVNM back-side pattern supports nine identifier-tail digits,
  their check digit, the full 12-digit identifier and `<<` plus a final digit.
  The tail check digit and agreement with the full identifier are checked. This
  does not validate the complete MRZ or document authenticity. Other formats
  remain unmatched for review.

Before hashing sources or building evidence, extraction enforces 2048 blocks,
1 MiB total text, 128 KiB per block, 10000 logical lines, 256 literal hashes,
256 semantic anchors, 64 anchors per field, 16 party designators, 128 explicit
role headings and 32 identity observations. Source ID/name lengths and lexical
alert counts are also bounded. At most 512 review issues are retained. Any
budget failure returns `needs_review`, one `extraction_budget_exceeded` issue,
and empty business fields, parties, identity documents and source manifest.
There is no partial accepted extraction or silent truncation.

Input blocks must have unique nonempty IDs, 1-based pages, finite optional
layout-detection scores in `[0,1]`, and nonempty optional rectangles. Rules
process each `source` stream independently in supplied reading order. Source
names must therefore identify a coherent text stream (for example `ocr` and
`native_pdf`), not individual lines. The caller supplies ordered blocks.

Evidence offsets are exact UTF-8 byte offsets, with the end exclusive. Candidate
raw values are copied from those spans. Evidence rectangles and scores describe
the entire source block, not a word box or recognition confidence. Blank anchored
values retain the complete nonempty anchor line as an unreadable observation.

Contract and identity observations remain separate. Aggregate incompatible
values are `conflict` with no selected value; disagreements within one origin
are `ambiguous`. Address comparison only folds case, whitespace and ASCII
punctuation: it does not assert that different address strings describe different
physical locations. A reviewer must resolve administrative renaming and other
semantic equivalences. Calendar-valid dates become ISO strings; identifiers
retain leading zeros and money uses decimal strings, not floats.

Literal `#` generates an ambiguous-symbol review issue. It can be in the source
or be an OCR defect; no `#` to m²/m³ replacement occurs. A short conservative
Vietnamese phrase list emits suggestions without applying them. There is no
invention of missing dates, automatic signature verification, semantic document
authentication, or automatic downstream acceptance.

## Replay

```sh
cargo run -p ocr-extraction --example extract -- /private/blocks.json > /private/extraction.json
cargo test -p ocr-extraction
```

The example accepts a JSON array of `SourceBlock` objects from a file or stdin.
Do not commit private source blocks or extraction outputs. Tests use fabricated
identities and cover role changes, blank values, invalid dates, decimal money,
UTF-8 evidence, cross-source conflicts, ambiguous/unmatched linkage and malformed
MRZ identifier runs.
