use crate::normalize::{date_span, decimal, text_key};
use crate::*;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

static LINE_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n|\\n").unwrap());
static SIGNATURE_DESIGNATORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:bên\s+[a-z][0-9]*[\s*/(),:–—-]*)+$").unwrap());
static PARTY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bbên\s+([a-z](?:[0-9]+)?)\b").unwrap());
static MRZ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^IDVNM([0-9]{9})([0-9])([0-9]{12})<<[0-9]$").unwrap());
static MONTHLY_PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:/\s*(?:tháng|month)\b|\s+(?:mỗi|hàng)\s+tháng\b)").unwrap()
});
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{9}(?:[0-9]{3})?\b").unwrap());
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\+?[0-9][0-9 .-]{7,18}[0-9]").unwrap());
static ACCOUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{6,30}\b").unwrap());
static AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([0-9][0-9.,]*)\s*(VND\b|VNĐ\b|đồng\b|₫)").unwrap());
static AREA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([0-9][0-9.,]*)\s*(m²|m2|m\^2)(?:\b|$)").unwrap());

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Id,
    Date,
    Phone,
    Account,
    Money,
    Rent,
    Area,
}
struct Rule {
    field: &'static str,
    re: Regex,
    kind: Kind,
}
fn rule(field: &'static str, anchor: &str, kind: Kind) -> Rule {
    Rule {
        field,
        re: Regex::new(&format!(r"(?i)^\s*[#*>\-\s]*(?:{anchor})\s*:\s*")).unwrap(),
        kind,
    }
}
static PARTY_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule("name", r"Họ và tên(?:\s*/\s*Full name)?", Kind::Text),
        rule(
            "id_number",
            r"CCCD\s*/\s*CMND số|CCCD số|CMND số|Số giấy tờ|Số(?:\s*/\s*No\.)?",
            Kind::Id,
        ),
        rule(
            "birth_date",
            r"Ngày sinh(?:\s*/\s*(?:Giới tính|Date of birth))?",
            Kind::Date,
        ),
        rule("issue_date", r"Ngày cấp(?:\s*/\s*Nơi cấp)?", Kind::Date),
        rule(
            "residence_address",
            r"Địa chỉ thường trú|Nơi thường trú(?:\s*/\s*Place of residence)?|Thường trú",
            Kind::Text,
        ),
        rule("contact_address", r"Địa chỉ liên hệ", Kind::Text),
        rule(
            "phone",
            r"Điện thoại(?:\s*/\s*Email)?|Số điện thoại",
            Kind::Phone,
        ),
        rule(
            "bank_account",
            r"Tài khoản nhận tiền|Số tài khoản",
            Kind::Account,
        ),
    ]
});
static GLOBAL_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            "monthly_rent",
            r"Giá thuê|Tiền thuê(?: nhà)?(?: hàng tháng)?",
            Kind::Rent,
        ),
        rule(
            "deposit_amount",
            r"Tiền (?:đặt )?cọc|Số tiền (?:đặt )?cọc",
            Kind::Money,
        ),
        rule(
            "property_address",
            r"Địa chỉ (?:Nhà thuê|nhà cho thuê|tài sản cho thuê|Nhà(?:\s*\(theo địa giới mới\))?)|Nhà thuê tại|Bên [a-z][0-9]* đồng ý cho Bên [a-z][0-9]* thuê (?:toàn bộ )?nhà ở tại địa chỉ",
            Kind::Text,
        ),
        rule("property_area", r"Diện tích(?: sử dụng)?", Kind::Area),
    ]
});
static AREA_INLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bdiện tích(?: sử dụng)?\s*:?[ \t]*").unwrap());
static BACK_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:Ngày, tháng, năm\s*/\s*Date, month, year|Ngày cấp\s*/\s*Date of issue|Date of issue)\s*:\s*").unwrap()
});
static SUSPECT: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        ("cản cứ pháp lý", "CĂN CỨ PHÁP LÝ"),
        ("bán giao", "bàn giao"),
        ("thú cung", "thú cưng"),
        ("bởi thường thiệt hại", "bồi thường thiệt hại"),
        ("tuần thứ pháp luật", "tuân thủ pháp luật"),
        ("tạm dùng", "tạm dừng"),
        ("chấm dút hợp đồng", "chấm dứt hợp đồng"),
    ]
    .into_iter()
    .map(|(p, replacement)| (Regex::new(&format!("(?i){p}")).unwrap(), replacement))
    .collect()
});

fn issue(code: &str, path: Option<String>, message: &str, evidence: Vec<Evidence>) -> Issue {
    Issue {
        code: code.into(),
        severity: "review".into(),
        field_path: path,
        message: message.into(),
        suggested_correction: None,
        evidence,
    }
}

pub(crate) fn recognition_recovery_reason(text: &str) -> Option<&'static str> {
    if text.trim().is_empty() {
        Some("empty_recognition")
    } else if text.len() <= 128 * 1024 && SUSPECT.iter().any(|(pattern, _)| pattern.is_match(text))
    {
        Some("suspect_vietnamese_phrase")
    } else {
        None
    }
}
fn evidence(block: &SourceBlock, start: usize, end: usize) -> Evidence {
    Evidence {
        block_id: block.id.clone(),
        page_number: block.page_number,
        region_index: block.region_index,
        source: block.source.clone(),
        byte_start: start,
        byte_end: end,
        quote: block.text[start..end].into(),
        bbox_2d: block.bbox_2d,
        geometry_scope: "source_block".into(),
        detector_score: block.detector_score,
        detector_score_kind: "layout_detection_not_ocr_confidence".into(),
        confidence: None,
    }
}
fn lines(text: &str) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut start = 0;
    for m in LINE_BREAK.find_iter(text) {
        result.push((start, m.start()));
        start = m.end();
    }
    result.push((start, text.len()));
    result
}
fn trimmed_span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let raw = &text[start..end];
    let raw = raw.trim_start_matches(|c: char| c.is_whitespace() || c == '*');
    let start = end - raw.len();
    let raw = raw.trim_end_matches(|c: char| c.is_whitespace() || c == '*');
    (start, start + raw.len())
}
fn defaults() -> BTreeMap<String, FieldResult> {
    [
        "name",
        "id_number",
        "birth_date",
        "issue_date",
        "residence_address",
        "contact_address",
        "phone",
        "bank_account",
    ]
    .into_iter()
    .map(|k| (k.into(), FieldResult::default()))
    .collect()
}
fn add_candidate(field: &mut FieldResult, value: FieldValue, ev: Evidence, origin: &str) {
    field.candidates.push(Candidate {
        raw_value: ev.quote.clone(),
        value,
        origin: origin.into(),
        method: "explicit_anchor_rule".into(),
        confidence: None,
        evidence: vec![ev],
    });
}
fn value_key(value: &FieldValue) -> String {
    match value {
        FieldValue::Text { value } => text_key(value),
        _ => serde_json::to_string(value).unwrap(),
    }
}
fn primary_candidate(candidate: &Candidate) -> bool {
    candidate
        .evidence
        .iter()
        .any(|ev| ev.source != "ocr_recovery_context")
}

fn recovery_only(field: &FieldResult) -> bool {
    !field.candidates.is_empty()
        && field
            .candidates
            .iter()
            .all(|candidate| !candidate.evidence.is_empty() && !primary_candidate(candidate))
}

fn resolve(field: &mut FieldResult, cross_source: bool) {
    let unique: BTreeSet<_> = field
        .candidates
        .iter()
        .map(|c| value_key(&c.value))
        .collect();
    field.selected = None;
    field.state = match unique.len() {
        0 => {
            if field.observations.is_empty() {
                FieldState::Missing
            } else {
                FieldState::Unreadable
            }
        }
        1 => {
            if let Some(primary) = field
                .candidates
                .iter()
                .find(|candidate| primary_candidate(candidate))
            {
                field.selected = Some(primary.value.clone());
                FieldState::Found
            } else {
                // Expanded crops may include adjacent fields or invented new text.
                // A sole alternative cannot become a selected business value.
                FieldState::Ambiguous
            }
        }
        _ => {
            if cross_source
                && field
                    .candidates
                    .iter()
                    .filter(|candidate| primary_candidate(candidate))
                    .map(|c| &c.origin)
                    .collect::<BTreeSet<_>>()
                    .len()
                    > 1
            {
                FieldState::Conflict
            } else {
                FieldState::Ambiguous
            }
        }
    };
}
#[allow(clippy::too_many_arguments)] // One anchored rule application carries its source, scope, and issue sink.
fn parse(
    rule: &Rule,
    block: &SourceBlock,
    value_start: usize,
    end: usize,
    origin: &str,
    fields: &mut BTreeMap<String, FieldResult>,
    issues: &mut Vec<Issue>,
    path: &str,
) {
    let (start, end) = trimmed_span(&block.text, value_start, end);
    let raw = &block.text[start..end];
    let field = fields.entry(rule.field.into()).or_default();
    // An anchor with no usable value is not evidence that the field was absent.
    if raw.is_empty()
        || raw
            .chars()
            .all(|c| c.is_whitespace() || c == '#' || c == '_' || c == '.')
    {
        let line_start = lines(&block.text)
            .into_iter()
            .find(|(s, e)| *s <= value_start && value_start <= *e)
            .map_or(value_start.min(end), |(s, _)| s);
        field.observations.push(evidence(block, line_start, end));
        push_issue(
            issues,
            issue(
                "anchored_value_unreadable",
                Some(format!("{path}.{}", rule.field)),
                "Explicit field anchor has no usable value; do not invent a replacement.",
                vec![evidence(block, line_start, end)],
            ),
        );
        return;
    }
    let mut span = (start, end);
    let parsed = match rule.kind {
        Kind::Text => {
            let cut = raw.find(';').unwrap_or(raw.len());
            span.1 = start + cut;
            let (s, e) = trimmed_span(&block.text, span.0, span.1);
            span = (s, e);
            let value = &block.text[s..e];
            if value.contains('#') || value.is_empty() {
                None
            } else {
                Some(FieldValue::Text {
                    value: value.into(),
                })
            }
        }
        Kind::Id => DIGITS.find(raw).map(|m| {
            span = (start + m.start(), start + m.end());
            FieldValue::Identifier {
                value: m.as_str().into(),
            }
        }),
        Kind::Phone => PHONE.find(raw).map(|m| {
            span = (start + m.start(), start + m.end());
            FieldValue::Identifier {
                value: m
                    .as_str()
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '+')
                    .collect(),
            }
        }),
        Kind::Account => ACCOUNT.find(raw).map(|m| {
            span = (start + m.start(), start + m.end());
            FieldValue::Identifier {
                value: m.as_str().into(),
            }
        }),
        Kind::Date => date_span(raw).and_then(|(s, e, date)| {
            span = (start + s, start + e);
            date.map(|value| FieldValue::Date { value })
        }),
        Kind::Money | Kind::Rent => AMOUNT.captures(raw).and_then(|c| {
            let amount = decimal(c.get(1)?.as_str())?;
            let full = c.get(0)?;
            span = (start, start + full.end());
            let period_match = MONTHLY_PERIOD.find(&raw[full.end()..]);
            let monthly = period_match.is_some();
            if let Some(period) = period_match {
                span.1 = start + full.end() + period.end();
            }
            if matches!(rule.kind, Kind::Rent) && !monthly {
                return None;
            }
            Some(FieldValue::Money {
                amount,
                currency: "VND".into(),
                period: monthly.then(|| "month".into()),
            })
        }),
        Kind::Area => AREA.captures(raw).and_then(|c| {
            let m = c.get(0)?;
            span = (start, start + m.end());
            Some(FieldValue::Area {
                amount: decimal(c.get(1)?.as_str())?,
                unit: "m2".into(),
            })
        }),
    };
    if let Some(value) = parsed {
        add_candidate(field, value, evidence(block, span.0, span.1), origin);
    } else {
        field.observations.push(evidence(block, start, end));
        push_issue(
            issues,
            issue(
                "invalid_or_unsupported_value",
                Some(format!("{path}.{}", rule.field)),
                "Value cannot be normalized conservatively (format, currency, period, calendar validity, or placeholder).",
                vec![evidence(block, start, end)],
            ),
        );
    }
}

fn heading(line: &str) -> Option<(String, PartyRole)> {
    let line = line.trim().trim_start_matches(['#', '*', '-', ' ']);
    if line.len() > 160 || !line.to_lowercase().starts_with("bên ") {
        return None;
    }
    let captures = PARTY.captures(line)?;
    let designator = captures.get(1)?.as_str().to_uppercase();
    let lower = line.to_lowercase();
    let role = if lower.contains("bên cho thuê") {
        PartyRole::Landlord
    } else if lower.contains("bên thuê") {
        PartyRole::Tenant
    } else {
        PartyRole::Unknown
    };
    if role == PartyRole::Unknown
        && !SIGNATURE_DESIGNATORS.is_match(line.trim_end_matches('*').trim())
    {
        return None;
    }
    Some((designator, role))
}

const MAX_ISSUES: usize = 512;

fn push_issue(issues: &mut Vec<Issue>, item: Issue) {
    if issues.len() < MAX_ISSUES - 1 {
        issues.push(item);
    } else if !issues
        .iter()
        .any(|i| i.code == "extraction_budget_exceeded")
    {
        issues.push(issue(
            "extraction_budget_exceeded",
            None,
            "Review issue budget exceeded; no business values may be consumed.",
            Vec::new(),
        ));
    }
}

fn fail_budget(result: &mut ExtractionResult, reason: &str) {
    result.supported_template = false;
    result.source_blocks.clear();
    result.parties.clear();
    result.fields.clear();
    result.identity_documents.clear();
    result.issues = vec![issue(
        "extraction_budget_exceeded",
        None,
        reason,
        Vec::new(),
    )];
    result.limitations.push("Source manifest and business extraction omitted because a bounded extraction budget was exceeded.".into());
}

fn is_identity_block(text: &str) -> bool {
    let lower = text.to_lowercase();
    ((lower.contains("citizen identity card") || lower.contains("căn cước công dân"))
        && (lower.contains("full name") || lower.contains("số / no")))
        || (lower.contains("personal identification")
            && (lower.contains("idvnm") || lower.contains("ngày, tháng, năm")))
}

/// All checks precede source hashing, manifests and repeated evidence cloning.
fn preflight(blocks: &[SourceBlock]) -> Result<(), &'static str> {
    if blocks.len() > 2048 {
        return Err("Maximum 2048 source blocks exceeded; extraction omitted.");
    }
    let mut bytes = 0usize;
    let mut line_count = 0usize;
    let mut hashes = 0usize;
    for block in blocks {
        if block.id.len() > 256 || block.source.len() > 128 {
            return Err("Source identifier length budget exceeded; extraction omitted.");
        }
        if block.text.len() > 128 * 1024 {
            return Err("Maximum 128 KiB per block exceeded; extraction omitted.");
        }
        bytes = bytes.saturating_add(block.text.len());
        if bytes > 1024 * 1024 {
            return Err("Maximum 1 MiB total text exceeded; extraction omitted.");
        }
        line_count = line_count.saturating_add(LINE_BREAK.find_iter(&block.text).count() + 1);
        if line_count > 10_000 {
            return Err("Maximum 10000 logical lines exceeded; extraction omitted.");
        }
        hashes = hashes.saturating_add(block.text.bytes().filter(|b| *b == b'#').count());
        if hashes > 256 {
            return Err("Maximum 256 literal hash symbols exceeded; extraction omitted.");
        }
    }
    let mut alerts = hashes;
    let mut anchors = 0usize;
    let mut identities = 0usize;
    let mut role_headings = 0usize;
    let mut designators = BTreeSet::new();
    let mut per_field: BTreeMap<&str, usize> = BTreeMap::new();
    for block in blocks {
        identities += usize::from(is_identity_block(&block.text));
        if identities > 32 {
            return Err("Maximum 32 identity observations exceeded; extraction omitted.");
        }
        alerts += usize::from(block.detector_score.is_some_and(|v| v < 0.65));
        for (pattern, _) in SUSPECT.iter() {
            alerts = alerts.saturating_add(pattern.find_iter(&block.text).count());
            if alerts > 384 {
                return Err("Lexical review alert budget exceeded; extraction omitted.");
            }
        }
        for (start, end) in lines(&block.text) {
            let line = &block.text[start..end];
            if let Some((designator, role)) = heading(line)
                && role != PartyRole::Unknown
            {
                role_headings += 1;
                if role_headings > 128 {
                    return Err("Maximum 128 explicit role headings exceeded; extraction omitted.");
                }
                designators.insert(designator);
                if designators.len() > 16 {
                    return Err("Maximum 16 parties exceeded; extraction omitted.");
                }
            }
            for rule in PARTY_RULES.iter().chain(GLOBAL_RULES.iter()) {
                if rule.re.is_match(line) {
                    anchors += 1;
                    let count = per_field.entry(rule.field).or_default();
                    *count += 1;
                    if *count > 64 {
                        return Err(
                            "Maximum 64 candidate anchors per semantic field exceeded; extraction omitted.",
                        );
                    }
                }
            }
            let inline_area = AREA_INLINE.is_match(line)
                && !GLOBAL_RULES
                    .iter()
                    .any(|rule| rule.field == "property_area" && rule.re.is_match(line));
            let back_date = BACK_DATE.is_match(line)
                && !PARTY_RULES
                    .iter()
                    .any(|rule| rule.field == "issue_date" && rule.re.is_match(line));
            for field in [
                inline_area.then_some("property_area"),
                back_date.then_some("issue_date"),
            ]
            .into_iter()
            .flatten()
            {
                anchors += 1;
                let count = per_field.entry(field).or_default();
                *count += 1;
                if *count > 64 {
                    return Err(
                        "Maximum 64 candidate anchors per semantic field exceeded; extraction omitted.",
                    );
                }
            }
            if line.trim_start().starts_with("IDVNM") {
                anchors += 1;
                let count = per_field.entry("id_number").or_default();
                *count += 1;
                if *count > 64 {
                    return Err("Maximum 64 identifier anchors exceeded; extraction omitted.");
                }
            }
            if anchors > 256 {
                return Err("Maximum 256 semantic anchors exceeded; extraction omitted.");
            }
        }
    }
    Ok(())
}

pub(crate) fn extract(blocks: &[SourceBlock]) -> ExtractionResult {
    let mut result = ExtractionResult {
        schema_version: SCHEMA_VERSION.into(), profile_version: PROFILE_VERSION.into(), normalizer_version: NORMALIZER_VERSION.into(),
        policy_version: POLICY_VERSION.into(), supported_template: false, decision: Decision::NeedsReview,
        source_blocks: Vec::new(),
        parties: Vec::new(), fields: ["monthly_rent", "deposit_amount", "property_address", "property_area"].into_iter().map(|f| (f.into(), FieldResult::default())).collect(),
        identity_documents: Vec::new(), issues: Vec::new(),
        limitations: vec!["Scoped explicit-anchor rules; other templates and unrecognized anchors require review.".into(),
            "OCR and native PDF text remain uncalibrated observations; evidence proves text provenance, not visual correctness.".into(),
            "Whole-block geometry only; detector scores are not OCR or field confidence.".into(),
            "No authenticity, legal validity, digital-signature verification, or automatic acceptance.".into()],
    };
    if let Err(reason) = preflight(blocks) {
        fail_budget(&mut result, reason);
        return result;
    }
    result.source_blocks = blocks
        .iter()
        .map(|b| SourceReference {
            block_id: b.id.clone(),
            page_number: b.page_number,
            region_index: b.region_index,
            source: b.source.clone(),
            text_sha256: format!("{:x}", Sha256::digest(b.text.as_bytes())),
            text_bytes: b.text.len(),
        })
        .collect();
    let mut seen_ids = BTreeSet::new();
    let invalid = blocks.iter().any(|b| {
        b.page_number == 0
            || !seen_ids.insert(b.id.as_str())
            || b.id.is_empty()
            || b.detector_score
                .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
            || b.bbox_2d
                .is_some_and(|[x1, y1, x2, y2]| x1 >= x2 || y1 >= y2)
    });
    if invalid {
        push_issue(
            &mut result.issues,
            issue(
                "invalid_source_blocks",
                None,
                "Source block IDs must be unique/nonempty, pages 1-based, detector scores finite within [0,1], and boxes nonempty; extraction stopped.",
                Vec::new(),
            ),
        );
        return result;
    }
    push_issue(
        &mut result.issues,
        issue(
            "uncalibrated_extraction_requires_review",
            None,
            "Every result requires review until a labeled, held-out evaluation supports an acceptance policy.",
            Vec::new(),
        ),
    );
    let mut streams: BTreeMap<&str, Vec<&SourceBlock>> = BTreeMap::new();
    let mut identity_blocks = BTreeSet::new();
    for (index, block) in blocks.iter().enumerate() {
        streams.entry(&block.source).or_default().push(block);
        if block
            .detector_score
            .is_some_and(|s| !s.is_finite() || s < 0.65)
        {
            push_issue(
                &mut result.issues,
                issue(
                    "low_layout_detection_score",
                    None,
                    "Layout detection score is low; it is not recognition confidence and does not prove clipping.",
                    vec![evidence(block, 0, block.text.len())],
                ),
            );
        }
        for (start, _) in block.text.match_indices('#') {
            let line_start = block.text[..start].rfind('\n').map_or(0, |p| p + 1);
            let prefix = &block.text[line_start..start];
            let remainder = &block.text[start..];
            let hashes = remainder.chars().take_while(|c| *c == '#').count();
            if prefix.trim_matches('#').trim().is_empty() && remainder[hashes..].starts_with(' ') {
                continue;
            }
            push_issue(
                &mut result.issues,
                issue(
                    "ambiguous_hash_symbol",
                    None,
                    "Literal # may be present in the source, a blank placeholder, or a recognition error; no unit/value substitution was made.",
                    vec![evidence(block, start, start + 1)],
                ),
            );
        }
        for (pattern, suggestion) in SUSPECT.iter() {
            for m in pattern.find_iter(&block.text) {
                let mut item = issue(
                    "suspect_vietnamese_phrase",
                    None,
                    "Conservative lexical alert; suggestion is not applied and may be inappropriate in context.",
                    vec![evidence(block, m.start(), m.end())],
                );
                item.suggested_correction = Some((*suggestion).into());
                push_issue(&mut result.issues, item);
            }
        }
        let lower = block.text.to_lowercase();
        if (lower.contains("citizen identity card") || lower.contains("căn cước công dân"))
            && (lower.contains("full name") || lower.contains("số / no"))
            || lower.contains("personal identification")
                && (lower.contains("idvnm") || lower.contains("ngày, tháng, năm"))
        {
            identity_blocks.insert(index);
        }
    }
    let card_ids: BTreeSet<_> = identity_blocks
        .iter()
        .map(|i| blocks[*i].id.as_str())
        .collect();
    let mut parties: BTreeMap<String, Party> = BTreeMap::new();
    let mut title = false;
    for stream in streams.values() {
        let mut active: Option<String> = None;
        let mut ended = false;
        for block in stream {
            if card_ids.contains(block.id.as_str()) {
                active = None;
                continue;
            }
            let lower = block.text.to_lowercase();
            title |= lower.contains("hợp đồng thuê");
            for (start, end) in lines(&block.text) {
                let line = &block.text[start..end];
                if let Some((designator, role)) = heading(line) {
                    if role == PartyRole::Unknown {
                        active = None;
                        ended = true;
                        continue;
                    }
                    let id = format!("contract_party_{}", designator.to_lowercase());
                    let entry = parties.entry(id.clone()).or_insert_with(|| Party {
                        id: id.clone(),
                        designator,
                        role: role.clone(),
                        role_evidence: Vec::new(),
                        contract_fields: defaults(),
                        identity_fields: defaults(),
                        fields: defaults(),
                        linked_identity_documents: Vec::new(),
                    });
                    if entry.role != role {
                        entry.role = PartyRole::Unknown;
                        push_issue(
                            &mut result.issues,
                            issue(
                                "conflicting_party_role",
                                Some(id.clone()),
                                "The same party designator has incompatible or unspecified roles.",
                                vec![evidence(block, start, end)],
                            ),
                        );
                    }
                    entry.role_evidence.push(evidence(block, start, end));
                    active = Some(id);
                    ended = false;
                    continue;
                }
                // A section heading ends the party scope, preventing later named
                // signatories or payment details from being assigned to the last party.
                let clean = line.trim().trim_start_matches('#').trim().to_lowercase();
                if clean.starts_with("điều ")
                    || clean.starts_with("tài sản cho thuê")
                    || clean.starts_with("nội dung hợp đồng")
                {
                    ended = true;
                }
                for rule in GLOBAL_RULES.iter() {
                    if let Some(m) = rule.re.find(line) {
                        parse(
                            rule,
                            block,
                            start + m.end(),
                            end,
                            "contract",
                            &mut result.fields,
                            &mut result.issues,
                            "fields",
                        );
                    }
                }
                if let Some(m) = AREA_INLINE.find(line)
                    && !GLOBAL_RULES
                        .iter()
                        .any(|r| r.field == "property_area" && r.re.is_match(line))
                {
                    let rule = GLOBAL_RULES
                        .iter()
                        .find(|r| r.field == "property_area")
                        .unwrap();
                    let cut = line[m.end()..]
                        .find(';')
                        .map(|p| start + m.end() + p)
                        .unwrap_or(end);
                    parse(
                        rule,
                        block,
                        start + m.end(),
                        cut,
                        "contract",
                        &mut result.fields,
                        &mut result.issues,
                        "fields",
                    );
                }
                if !ended && let Some(id) = &active {
                    let party = parties.get_mut(id).unwrap();
                    for rule in PARTY_RULES.iter() {
                        if let Some(m) = rule.re.find(line) {
                            parse(
                                rule,
                                block,
                                start + m.end(),
                                end,
                                "contract",
                                &mut party.contract_fields,
                                &mut result.issues,
                                &format!("parties.{id}.contract_fields"),
                            );
                        }
                    }
                }
            }
        }
    }
    for party in parties.values_mut() {
        for f in party.contract_fields.values_mut() {
            resolve(f, false);
        }
    }
    let mut documents = Vec::new();
    // Identity front/back observations remain separate. Back-side linkage uses
    // a strict supported MRZ structure, never proximity or matching names.
    for &index in &identity_blocks {
        let block = &blocks[index];
        let lower = block.text.to_lowercase();
        let front = lower.contains("citizen identity card") || lower.contains("full name");
        let mut doc = IdentityDocument {
            id: format!("identity_{}", index + 1),
            source_block_ids: vec![block.id.clone()],
            side: if front { "front" } else { "back" }.into(),
            fields: defaults(),
            linkage: IdentityLinkage {
                state: "unmatched".into(),
                party_ids: Vec::new(),
                method: "exact_unambiguous_identifier_only".into(),
                evidence: Vec::new(),
            },
        };
        for (start, end) in lines(&block.text) {
            let line = &block.text[start..end];
            for rule in PARTY_RULES
                .iter()
                .filter(|r| !matches!(r.kind, Kind::Phone | Kind::Account))
            {
                if let Some(m) = rule.re.find(line) {
                    parse(
                        rule,
                        block,
                        start + m.end(),
                        end,
                        "identity_card",
                        &mut doc.fields,
                        &mut result.issues,
                        &format!("identity_documents.{}", doc.id),
                    );
                }
            }
            if !front && let Some(m) = BACK_DATE.find(line) {
                let date_rule = PARTY_RULES
                    .iter()
                    .find(|r| r.field == "issue_date")
                    .unwrap();
                parse(
                    date_rule,
                    block,
                    start + m.end(),
                    end,
                    "identity_card",
                    &mut doc.fields,
                    &mut result.issues,
                    &format!("identity_documents.{}", doc.id),
                );
            }
        }
        for f in doc.fields.values_mut() {
            resolve(f, false);
        }
        documents.push(doc);
    }
    for doc in &mut documents {
        if doc.side == "back" && doc.fields["id_number"].candidates.is_empty() {
            let block = blocks
                .iter()
                .find(|b| doc.source_block_ids.contains(&b.id))
                .unwrap();
            for (s, e) in lines(&block.text) {
                let line = &block.text[s..e];
                let trimmed = line.trim();
                // Scoped observed IDVNM layout: document-number tail (9), its
                // check digit (1), full identifier (12), <<, composite digit (1).
                // We validate the document-number check digit; we do not claim
                // to validate the entire MRZ or establish authenticity.
                let Some(c) = MRZ.captures(trimmed) else {
                    continue;
                };
                let tail = c.get(1).unwrap().as_str();
                let check = c.get(2).unwrap().as_str().as_bytes()[0] - b'0';
                let full = c.get(3).unwrap();
                let sum: u32 = tail
                    .bytes()
                    .zip([7u32, 3, 1].into_iter().cycle())
                    .map(|(digit, weight)| u32::from(digit - b'0') * weight)
                    .sum();
                if sum % 10 != u32::from(check) || !full.as_str().ends_with(tail) {
                    continue;
                }
                let offset = s + line.len() - line.trim_start().len() + full.start();
                add_candidate(
                    doc.fields.get_mut("id_number").unwrap(),
                    FieldValue::Identifier {
                        value: full.as_str().into(),
                    },
                    evidence(block, offset, offset + full.as_str().len()),
                    "identity_card",
                );
                if let Some(candidate) = doc
                    .fields
                    .get_mut("id_number")
                    .unwrap()
                    .candidates
                    .last_mut()
                {
                    candidate.method = "restricted_idvnm_mrz_rule".into();
                }
            }
            resolve(doc.fields.get_mut("id_number").unwrap(), false);
        }
        let id_field = &doc.fields["id_number"];
        if id_field.state == FieldState::Found {
            let key = value_key(id_field.selected.as_ref().unwrap());
            let matching: Vec<_> = parties
                .values()
                .filter(|p| {
                    p.contract_fields["id_number"]
                        .candidates
                        .iter()
                        .any(|candidate| value_key(&candidate.value) == key)
                })
                .map(|p| p.id.clone())
                .collect();
            doc.linkage.party_ids = matching.clone();
            doc.linkage.evidence = id_field
                .candidates
                .iter()
                .flat_map(|c| c.evidence.clone())
                .collect();
            for id in &matching {
                doc.linkage.evidence.extend(
                    parties[id].contract_fields["id_number"]
                        .candidates
                        .iter()
                        .flat_map(|c| c.evidence.clone()),
                );
            }
            if matching.len() == 1
                && parties[&matching[0]].contract_fields["id_number"].state == FieldState::Found
            {
                doc.linkage.state = "linked".into();
                let party = parties.get_mut(&matching[0]).unwrap();
                party.linked_identity_documents.push(doc.id.clone());
                for (key, field) in &doc.fields {
                    let target = party.identity_fields.entry(key.clone()).or_default();
                    target.candidates.extend(field.candidates.clone());
                    target.observations.extend(field.observations.clone());
                }
            } else if !matching.is_empty() {
                doc.linkage.state = "ambiguous".into();
            }
        } else if id_field.state == FieldState::Ambiguous {
            doc.linkage.state = "ambiguous".into();
        }
        if doc.linkage.state != "linked" {
            push_issue(
                &mut result.issues,
                issue(
                    if doc.linkage.state == "ambiguous" {
                        "identity_linkage_ambiguous"
                    } else {
                        "identity_linkage_unmatched"
                    },
                    Some(format!("identity_documents.{}", doc.id)),
                    "Identity attachment was not assigned: exact, unique document identifier evidence is required; names alone are insufficient.",
                    doc.linkage.evidence.clone(),
                ),
            );
        }
    }
    result.supported_template = title
        && parties.values().any(|p| p.role == PartyRole::Landlord)
        && parties.values().any(|p| p.role == PartyRole::Tenant)
        && parties.values().all(|p| p.role != PartyRole::Unknown);
    if !result.supported_template {
        push_issue(
            &mut result.issues,
            issue(
                "unsupported_or_incomplete_template",
                None,
                "Required rental-contract title and explicit landlord/tenant role headings were not all recognized. Extracted values are unaccepted proposals.",
                Vec::new(),
            ),
        );
    }
    for (id, party) in &mut parties {
        for f in party.identity_fields.values_mut() {
            resolve(f, false);
        }
        for key in defaults().keys() {
            let mut field = party.contract_fields[key].clone();
            field
                .candidates
                .extend(party.identity_fields[key].candidates.clone());
            field
                .observations
                .extend(party.identity_fields[key].observations.clone());
            resolve(&mut field, true);
            if matches!(field.state, FieldState::Conflict | FieldState::Ambiguous)
                && !recovery_only(&field)
            {
                push_issue(
                    &mut result.issues,
                    issue(
                        if field.state == FieldState::Conflict {
                            "cross_source_field_conflict"
                        } else {
                            "multiple_field_values"
                        },
                        Some(format!("parties.{id}.fields.{key}")),
                        "Incompatible observations were retained; no value was selected.",
                        field
                            .candidates
                            .iter()
                            .flat_map(|c| c.evidence.clone())
                            .collect(),
                    ),
                );
            }
            if [
                "name",
                "id_number",
                "birth_date",
                "issue_date",
                "residence_address",
            ]
            .contains(&key.as_str())
                && matches!(field.state, FieldState::Missing | FieldState::Unreadable)
            {
                push_issue(
                    &mut result.issues,
                    issue(
                        "critical_field_unresolved",
                        Some(format!("parties.{id}.fields.{key}")),
                        "Required review field is absent or unreadable in the recognized scope.",
                        field.observations.clone(),
                    ),
                );
            }
            party.fields.insert(key.clone(), field);
        }
    }
    for field in result.fields.values_mut() {
        resolve(field, false);
    }
    result.parties = parties.into_values().collect();
    result.identity_documents = documents;
    let mut recovery_issues = Vec::new();
    for (path, field) in result
        .fields
        .iter()
        .map(|(key, field)| (format!("fields.{key}"), field))
        .chain(result.parties.iter().flat_map(|party| {
            party
                .fields
                .iter()
                .map(move |(key, field)| (format!("parties.{}.fields.{key}", party.id), field))
        }))
        .chain(result.identity_documents.iter().flat_map(|document| {
            document.fields.iter().map(move |(key, field)| {
                (
                    format!("identity_documents.{}.fields.{key}", document.id),
                    field,
                )
            })
        }))
    {
        if recovery_only(field) {
            recovery_issues.push(issue("recovery_only_observation", Some(path), "Only an expanded-crop recovery context proposed this value. No first-pass source supports automatic selection; explicit evidence-linked correction is required.", field.candidates.iter().flat_map(|candidate| candidate.evidence.clone()).collect()));
        }
    }
    for item in recovery_issues {
        push_issue(&mut result.issues, item);
    }
    if result
        .issues
        .iter()
        .any(|i| i.code == "extraction_budget_exceeded")
    {
        fail_budget(
            &mut result,
            "Maximum 512 review issues exceeded; all business output omitted.",
        );
    }
    result
}
