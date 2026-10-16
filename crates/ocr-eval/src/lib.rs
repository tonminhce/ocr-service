//! Offline evaluation of manually labelled documents. No acceptance policy or
//! numeric confidence calibration is inferred from a dataset or synthetic run.
pub mod arrival;
pub mod benchmark;
pub mod v2;
use anyhow::{Context, Result, bail, ensure};
use chrono::NaiveDate;
use ocr_core::{
    projection::{BusinessField, BusinessResult},
    review::ReviewSnapshot,
};
use ocr_extraction::{Evidence, ExtractionResult, FieldState, FieldValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Calibration,
    Test,
}
impl Split {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "dev" => Ok(Self::Dev),
            "calibration" => Ok(Self::Calibration),
            "test" => Ok(Self::Test),
            _ => bail!("split must be dev, calibration or test"),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub dataset_name: String,
    pub synthetic: bool,
    pub documents: Vec<Document>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub id: String,
    pub split: Split,
    /// Related contracts/pages/near-duplicates belong to one split.
    pub group_id: String,
    pub source_sha256: String,
    pub source_path: String,
    #[serde(default)]
    pub result_path: Option<String>,
    #[serde(default)]
    pub result_sha256: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, Label>,
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub state: FieldState,
    #[serde(default)]
    pub values: Vec<FieldValue>,
    /// Manually checked source quotes. None means semantic evidence correctness
    /// has not been labelled and must not be inferred from a structural check.
    #[serde(default)]
    pub evidence_gold: Option<Vec<GoldEvidence>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldEvidence {
    pub page_number: u32,
    pub source: String,
    pub quote: String,
}

pub struct Dataset {
    pub manifest: Manifest,
    base: PathBuf,
}
impl Dataset {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("manifest does not exist")?;
        let base = path
            .parent()
            .context("manifest has no parent directory")?
            .to_path_buf();
        let manifest: Manifest = serde_json::from_slice(&read_bounded(&path, MAX_MANIFEST_BYTES)?)
            .map_err(|_| anyhow::anyhow!("manifest JSON is invalid"))?;
        ensure!(
            manifest.schema_version == "ocr-eval-manifest/1.0.0",
            "unsupported manifest schema"
        );
        ensure!(
            !manifest.dataset_name.is_empty() && manifest.dataset_name.len() <= 128,
            "invalid dataset name"
        );
        ensure!(
            !manifest.documents.is_empty() && manifest.documents.len() <= 1000,
            "dataset must contain 1..1000 documents"
        );
        let mut ids = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        let mut groups = BTreeMap::new();
        for document in &manifest.documents {
            ensure!(
                opaque_id(&document.id) && opaque_id(&document.group_id),
                "document/group IDs must be bounded opaque identifiers"
            );
            ensure!(ids.insert(&document.id), "duplicate document ID");
            ensure!(
                valid_sha(&document.source_sha256),
                "source_sha256 must be lowercase hexadecimal"
            );
            ensure!(
                hashes.insert(&document.source_sha256),
                "duplicate source hash: duplicate documents cannot enter evaluation"
            );
            if let Some(split) = groups.insert(&document.group_id, document.split) {
                ensure!(split == document.split, "group crosses dataset splits");
            }
            let source = safe_resolve(&base, &document.source_path)?;
            ensure!(
                digest(&read_bounded(&source, MAX_SOURCE_BYTES)?) == document.source_sha256,
                "source hash mismatch"
            );
            ensure!(
                document.result_path.is_some() == document.result_sha256.is_some(),
                "result_path and result_sha256 must be supplied together"
            );
            if let Some(result) = &document.result_path {
                safe_resolve(&base, result)?;
                ensure!(
                    document.result_sha256.as_deref().is_some_and(valid_sha),
                    "result_sha256 must be lowercase hexadecimal"
                );
            }
            ensure!(
                document.labels.len() <= 128 && document.roles.len() <= 16,
                "document label budget exceeded"
            );
            for (path, label) in &document.labels {
                ensure!(valid_field_path(path), "unsupported business field path");
                let count = label.values.len();
                ensure!(
                    match label.state {
                        FieldState::Found => count == 1,
                        FieldState::Conflict | FieldState::Ambiguous => (2..=16).contains(&count),
                        FieldState::Missing | FieldState::Unreadable => count == 0,
                    },
                    "label state/value shape is inconsistent"
                );
                let values: BTreeSet<_> = label
                    .values
                    .iter()
                    .map(normalized_value)
                    .collect::<Result<_>>()?;
                ensure!(
                    values.len() == count,
                    "label has duplicate normalized values"
                );
                ensure!(
                    label
                        .values
                        .iter()
                        .all(|value| value_type_matches(path, value)),
                    "label value type does not match business field"
                );
                if let Some(gold) = &label.evidence_gold {
                    ensure!(
                        !gold.is_empty() && gold.len() <= 16,
                        "manual evidence must contain 1..16 quotes"
                    );
                    ensure!(
                        gold.iter().all(|e| e.page_number > 0
                            && !e.source.is_empty()
                            && e.source.len() <= 64
                            && !e.quote.is_empty()
                            && e.quote.len() <= 4096),
                        "invalid manual evidence label"
                    );
                }
            }
            ensure!(
                document.roles.iter().all(|(id, role)| opaque_id(id)
                    && matches!(role.as_str(), "landlord" | "tenant" | "unknown")),
                "invalid role expectation"
            );
        }
        Ok(Self { manifest, base })
    }
    pub fn selected(&self, split: Split) -> Result<Vec<&Document>> {
        let documents: Vec<_> = self
            .manifest
            .documents
            .iter()
            .filter(|d| d.split == split)
            .collect();
        ensure!(
            !documents.is_empty(),
            "requested split contains no documents"
        );
        Ok(documents)
    }
    pub fn source_file(&self, document: &Document) -> Result<PathBuf> {
        safe_resolve(&self.base, &document.source_path)
    }
    pub fn source_size(&self, document: &Document) -> Result<u64> {
        let metadata = std::fs::metadata(self.source_file(document)?)?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_SOURCE_BYTES as u64,
            "source file exceeds its byte budget"
        );
        Ok(metadata.len())
    }
    pub fn source(&self, document: &Document) -> Result<Vec<u8>> {
        let bytes = read_bounded(&self.source_file(document)?, MAX_SOURCE_BYTES)?;
        ensure!(
            digest(&bytes) == document.source_sha256,
            "source changed after manifest validation"
        );
        Ok(bytes)
    }
    pub fn result(&self, document: &Document) -> Result<BusinessResult> {
        let path = document
            .result_path
            .as_deref()
            .context("score requires result_path for each selected document")?;
        let bytes = read_bounded(&safe_resolve(&self.base, path)?, MAX_RESULT_BYTES)?;
        ensure!(
            document.result_sha256.as_deref() == Some(digest(&bytes).as_str()),
            "prediction artifact hash mismatch"
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("result JSON is invalid"))?;
        for source_hash in [
            value.get("sha256"),
            value.get("source_sha256"),
            value.get("source").and_then(|v| v.get("sha256")),
            value
                .get("result")
                .and_then(|v| v.get("source"))
                .and_then(|v| v.get("sha256")),
        ]
        .into_iter()
        .flatten()
        .filter(|v| !v.is_null())
        {
            ensure!(
                source_hash.as_str() == Some(document.source_sha256.as_str()),
                "prediction source hash does not match manifest"
            );
        }
        load_business(&bytes)
    }
}
fn opaque_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn valid_field_path(path: &str) -> bool {
    let field = if let Some((role, field)) = path.split_once('.') {
        if !matches!(role, "landlord" | "tenant") {
            return false;
        }
        return matches!(
            field,
            "name"
                | "id_number"
                | "birth_date"
                | "issue_date"
                | "residence_address"
                | "contact_address"
                | "phone"
                | "bank_account"
        );
    } else {
        path
    };
    matches!(
        field,
        "monthly_rent" | "deposit_amount" | "property_address" | "property_area"
    )
}
fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn value_type_matches(path: &str, value: &FieldValue) -> bool {
    match (path.rsplit('.').next().unwrap_or(path), value) {
        ("birth_date" | "issue_date", FieldValue::Date { .. }) => true,
        ("id_number" | "phone" | "bank_account", FieldValue::Identifier { .. }) => true,
        ("monthly_rent", FieldValue::Money { period, .. }) => period.as_deref() == Some("month"),
        ("deposit_amount", FieldValue::Money { period, .. }) => period.is_none(),
        ("property_area", FieldValue::Area { .. }) => true,
        (
            "name" | "residence_address" | "contact_address" | "property_address",
            FieldValue::Text { .. },
        ) => true,
        _ => false,
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path).context("dataset file could not be inspected")?;
    ensure!(metadata.is_file(), "dataset input must be a regular file");
    ensure!(
        metadata.len() <= maximum as u64,
        "dataset file exceeds byte budget"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // If a regular file is replaced with a FIFO between stat and open,
        // opening must remain nonblocking. Do not follow a replacement symlink.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .context("dataset file could not be opened")?;
    ensure!(
        file.metadata()?.is_file(),
        "dataset input must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .context("dataset file could not be read")?;
    ensure!(bytes.len() <= maximum, "dataset file exceeds byte budget");
    Ok(bytes)
}
pub fn safe_resolve(base: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
        "dataset paths must be relative without traversal"
    );
    let full = base
        .join(path)
        .canonicalize()
        .context("dataset file does not exist")?;
    ensure!(
        full.starts_with(base),
        "dataset symlink escapes manifest directory"
    );
    Ok(full)
}
fn normalized_value(value: &FieldValue) -> Result<String> {
    let value = match value {
        FieldValue::Text { value } => {
            ensure!(value.len() <= 4096, "text value exceeds budget");
            FieldValue::Text {
                value: value
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase(),
            }
        }
        FieldValue::Identifier { value } => {
            ensure!(
                !value.is_empty() && value.len() <= 128,
                "invalid identifier"
            );
            FieldValue::Identifier {
                value: value.trim().to_owned(),
            }
        }
        FieldValue::Date { value } => {
            let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .context("date must be calendar-valid ISO")?;
            ensure!(
                date.format("%Y-%m-%d").to_string() == *value,
                "date must be canonical ISO"
            );
            FieldValue::Date {
                value: value.clone(),
            }
        }
        FieldValue::Money {
            amount,
            currency,
            period,
        } => {
            ensure!(
                currency == "VND" && matches!(period.as_deref(), None | Some("month")),
                "unsupported money unit"
            );
            FieldValue::Money {
                amount: decimal(amount)?,
                currency: currency.clone(),
                period: period.clone(),
            }
        }
        FieldValue::Area { amount, unit } => {
            ensure!(unit == "m2", "unsupported area unit");
            FieldValue::Area {
                amount: decimal(amount)?,
                unit: unit.clone(),
            }
        }
    };
    Ok(serde_json::to_string(&value)?)
}
fn decimal(input: &str) -> Result<String> {
    ensure!(!input.is_empty() && input.len() <= 32, "invalid decimal");
    let parts: Vec<_> = input.split('.').collect();
    ensure!(
        parts.len() <= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "decimal must use canonical decimal notation"
    );
    let integer = parts[0].trim_start_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let fraction = parts
        .get(1)
        .copied()
        .unwrap_or_default()
        .trim_end_matches('0');
    Ok(if fraction.is_empty() {
        integer.into()
    } else {
        format!("{integer}.{fraction}")
    })
}

pub fn load_business(bytes: &[u8]) -> Result<BusinessResult> {
    ensure!(
        bytes.len() <= MAX_RESULT_BYTES,
        "result exceeds byte budget"
    );
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("result JSON is invalid"))?;
    let business =
        if value.get("schema_version").and_then(|v| v.as_str()) == Some("ocr-business/1.0.0") {
            serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("business result is malformed"))?
        } else {
            let extraction = value
                .get("result")
                .and_then(|r| r.get("extraction"))
                .or_else(|| value.get("extraction"))
                .unwrap_or(&value);
            let extraction: ExtractionResult =
                serde_json::from_value(extraction.clone()).map_err(|_| {
                    anyhow::anyhow!("expected extraction, OCR result, API job or business result")
                })?;
            ensure!(
                extraction.schema_version == ocr_extraction::SCHEMA_VERSION,
                "unsupported extraction schema"
            );
            BusinessResult::from_extraction(Uuid::nil(), extraction, ReviewSnapshot::default())
        };
    validate_business(&business)?;
    Ok(business)
}
fn validate_business(result: &BusinessResult) -> Result<()> {
    ensure!(
        result.machine_decision == "needs_review",
        "automatic machine acceptance is unsupported"
    );
    ensure!(
        result.fields.len() <= 512 && result.evidence.len() <= 10_000,
        "business result exceeds field/evidence budget"
    );
    for (id, evidence) in &result.evidence {
        ensure!(
            id == &format!("ev_{}", digest(&serde_json::to_vec(evidence)?)),
            "evidence reference hash mismatch"
        );
        ensure!(
            evidence.page_number > 0
                && evidence.byte_start < evidence.byte_end
                && evidence.byte_end - evidence.byte_start == evidence.quote.len()
                && !evidence.quote.is_empty(),
            "evidence span/quote is structurally invalid"
        );
        ensure!(
            evidence.confidence.is_none(),
            "uncalibrated numeric evidence confidence is unsupported"
        );
    }
    for field in result.fields.values() {
        ensure!(
            matches!(
                field.approval.as_str(),
                "requires_review" | "human_corrected_requires_review" | "human_approved"
            ),
            "unsupported field approval state"
        );
        if field.approval == "human_approved" {
            ensure!(
                result.review.status == "approved"
                    && result.review.revision > 0
                    && result.review.principal.is_some(),
                "field approval disagrees with review snapshot"
            );
        }
        ensure!(
            field.confidence.is_none(),
            "uncalibrated numeric field confidence is unsupported"
        );
        for reference in field
            .evidence_refs
            .iter()
            .chain(field.candidates.iter().flat_map(|c| &c.evidence_refs))
        {
            ensure!(
                result.evidence.contains_key(reference),
                "dangling evidence reference"
            );
        }
        if let Some(value) = &field.value {
            normalized_value(value)?;
        }
        for candidate in &field.candidates {
            normalized_value(&candidate.value)?;
        }
        if field.correction.is_none() {
            ensure!(
                if field.state == FieldState::Found {
                    field.value.is_some()
                } else {
                    field.value.is_none()
                },
                "machine field state/selection is inconsistent"
            );
        } else if let Some(correction) = &field.correction {
            ensure!(
                field.value.as_ref().map(normalized_value).transpose()?
                    == Some(normalized_value(&correction.value)?),
                "correction disagrees with reviewed field value"
            );
            ensure!(
                !correction.evidence_refs.is_empty()
                    && correction
                        .evidence_refs
                        .iter()
                        .all(|reference| field.evidence_refs.contains(reference)
                            && result.evidence.contains_key(reference)),
                "correction evidence is invalid"
            );
        }
    }
    ensure!(
        result
            .parties
            .iter()
            .flat_map(|party| &party.role_evidence_refs)
            .all(|reference| result.evidence.contains_key(reference)),
        "dangling role evidence reference"
    );
    Ok(())
}

#[derive(Debug, Default, Serialize)]
pub struct FieldMetrics {
    pub labels: u64,
    pub state_matches: u64,
    pub exact_matches: u64,
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub expected_conflicts: u64,
    pub conflict_state_detected: u64,
    pub conflict_exact_detected: u64,
    pub conflict_state_recall: Option<f64>,
    pub conflict_exact_recall: Option<f64>,
    pub unexpected_candidate_values: u64,
    pub absent_fields_with_candidates: u64,
    pub evidence_available: u64,
    pub manual_evidence_labels: u64,
    pub manual_evidence_exact_matches: u64,
    pub human_approved: u64,
    pub human_approved_evaluable: u64,
    pub human_approved_errors: u64,
    pub human_approval_coverage: Option<f64>,
    pub human_approved_error_rate: Option<f64>,
}
#[derive(Debug, Serialize)]
pub struct ScoreReport {
    pub schema_version: String,
    pub dataset_name: String,
    pub synthetic: bool,
    pub split: Split,
    pub documents: usize,
    pub labelled_fields: u64,
    pub fields: BTreeMap<String, FieldMetrics>,
    pub role_labels: u64,
    pub role_matches: u64,
    pub machine_auto_acceptance_enabled: bool,
    pub evidence_integrity_scope: String,
    pub source_result_linkage: String,
    pub limitations: Vec<String>,
}
pub fn score(dataset: &Dataset, split: Split) -> Result<ScoreReport> {
    let documents = dataset.selected(split)?;
    let mut report = ScoreReport {
        schema_version: "ocr-eval-report/1.0.0".into(), dataset_name: dataset.manifest.dataset_name.clone(),
        synthetic: dataset.manifest.synthetic, split, documents: documents.len(), labelled_fields: 0,
        fields: BTreeMap::new(), role_labels: 0, role_matches: 0, machine_auto_acceptance_enabled: false,
        evidence_integrity_scope: "reference_hash_span_and_presence_only".into(),
        source_result_linkage: "source_and_prediction_artifact_hashes_verified; embedded_source_hash_checked_when_present; otherwise_manifest_mapping_attested".into(),
        limitations: vec!["No pixel verification; source quote correctness requires independent manual gold labels.".into(), "No calibrated confidence or automatic acceptance policy is fitted.".into(), "Manually grouped near-duplicates must share a split; hashes detect exact duplicates only.".into()],
    };
    if report.synthetic {
        report.limitations.push("Synthetic fixtures verify evaluator mechanics; their scores do not measure real document accuracy.".into());
    }
    for document in documents {
        ensure!(
            !document.labels.is_empty(),
            "score requires at least one manual field label per selected document"
        );
        let business = dataset.result(document)?;
        for (path, label) in &document.labels {
            report.labelled_fields += 1;
            score_field(
                report.fields.entry(path.clone()).or_default(),
                label,
                business.fields.get(path),
                &business.evidence,
            )?;
        }
        for (id, role) in &document.roles {
            report.role_labels += 1;
            report.role_matches += u64::from(
                business
                    .parties
                    .iter()
                    .any(|p| p.id == *id && p.role == *role),
            );
        }
    }
    for field in report.fields.values_mut() {
        field.precision = ratio(
            field.true_positives,
            field.true_positives + field.false_positives,
        );
        field.recall = ratio(
            field.true_positives,
            field.true_positives + field.false_negatives,
        );
        field.conflict_state_recall =
            ratio(field.conflict_state_detected, field.expected_conflicts);
        field.conflict_exact_recall =
            ratio(field.conflict_exact_detected, field.expected_conflicts);
        field.human_approval_coverage = ratio(field.human_approved, field.labels);
        field.human_approved_error_rate =
            ratio(field.human_approved_errors, field.human_approved_evaluable);
    }
    Ok(report)
}
fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}
fn score_field(
    metrics: &mut FieldMetrics,
    label: &Label,
    field: Option<&BusinessField>,
    evidence: &BTreeMap<String, Evidence>,
) -> Result<()> {
    metrics.labels += 1;
    let gold: BTreeSet<_> = label
        .values
        .iter()
        .map(normalized_value)
        .collect::<Result<_>>()?;
    let state = field.map(|f| &f.state).unwrap_or(&FieldState::Missing);
    let candidates: BTreeSet<_> = field
        .into_iter()
        .flat_map(|f| &f.candidates)
        .map(|c| normalized_value(&c.value))
        .collect::<Result<_>>()?;
    // Corrections never inflate machine scores. Raw candidates/state are kept by
    // the business projection; a corrected found field recovers its machine value
    // only when the original normalized candidate set is unambiguous.
    let predicted: BTreeSet<_> = match field {
        Some(f) if matches!(f.state, FieldState::Conflict | FieldState::Ambiguous) => {
            candidates.clone()
        }
        Some(f) if f.correction.is_some() => {
            if f.state == FieldState::Found && candidates.len() == 1 {
                candidates.clone()
            } else {
                BTreeSet::new()
            }
        }
        Some(f) => f
            .value
            .iter()
            .map(normalized_value)
            .collect::<Result<_>>()?,
        None => BTreeSet::new(),
    };
    let exact = state == &label.state && predicted == gold;
    metrics.state_matches += u64::from(state == &label.state);
    metrics.exact_matches += u64::from(exact);
    metrics.true_positives += u64::from(exact && !gold.is_empty());
    metrics.false_positives += u64::from(!exact && !predicted.is_empty());
    metrics.false_negatives += u64::from(!exact && !gold.is_empty());
    metrics.unexpected_candidate_values += candidates.difference(&gold).count() as u64;
    metrics.absent_fields_with_candidates +=
        u64::from(label.state == FieldState::Missing && !candidates.is_empty());
    if label.state == FieldState::Conflict {
        metrics.expected_conflicts += 1;
        metrics.conflict_state_detected += u64::from(state == &FieldState::Conflict);
        metrics.conflict_exact_detected += u64::from(exact);
    }
    let refs: Vec<_> = field
        .into_iter()
        .flat_map(|f| &f.evidence_refs)
        .filter_map(|r| evidence.get(r))
        .collect();
    metrics.evidence_available += u64::from(!refs.is_empty());
    if let Some(gold) = &label.evidence_gold {
        metrics.manual_evidence_labels += 1;
        metrics.manual_evidence_exact_matches += u64::from(gold.iter().all(|expected| {
            refs.iter().any(|actual| {
                actual.page_number == expected.page_number
                    && actual.source == expected.source
                    && actual.quote == expected.quote
            })
        }));
    }
    if let Some(field) = field.filter(|f| f.approval == "human_approved") {
        metrics.human_approved += 1;
        // Conflicts need an adjudicated single truth before accepted-error scoring.
        if matches!(label.state, FieldState::Found | FieldState::Missing) {
            metrics.human_approved_evaluable += 1;
            let reviewed: BTreeSet<_> = field
                .value
                .iter()
                .map(normalized_value)
                .collect::<Result<_>>()?;
            metrics.human_approved_errors += u64::from(reviewed != gold);
        }
    }
    Ok(())
}
