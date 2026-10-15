//! Compact, evidence-linked business views. Raw OCR artifacts are never rewritten.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, LazyLock},
};

use chrono::{DateTime, Utc};
use ocr_extraction::{Evidence, ExtractionResult, FieldResult, FieldState, FieldValue, PartyRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    review::ReviewSnapshot,
    store::{Store, StoreError},
};

static PROJECTION_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(8)));

/// Own the CPU permit inside the blocking closure so dropping its async waiter
/// cannot admit more work while the original closure is still running.
pub(crate) async fn bounded_projection<T, F>(work: F) -> Result<T, crate::review::ReviewError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, crate::review::ReviewError> + Send + 'static,
{
    let permit = PROJECTION_SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| crate::review::ReviewError::InvalidStoredResult)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| crate::review::ReviewError::InvalidStoredResult)?
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobStatus {
    pub id: Uuid,
    pub status: String,
    pub attempts: i32,
    pub extraction_profile: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub error: Option<String>,
    pub result_available: bool,
    pub review_revision: i64,
    pub review_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessResult {
    pub schema_version: String,
    pub job_id: Uuid,
    /// Immutable upload digest when loaded from the store; extraction-only replay has none.
    pub source_sha256: Option<String>,
    pub extraction_profile: String,
    pub supported_template: bool,
    pub machine_decision: String,
    pub review: ReviewSnapshot,
    pub parties: Vec<BusinessParty>,
    /// Stable semantic keys when roles are unique; otherwise party:<id>.<field>.
    pub fields: BTreeMap<String, BusinessField>,
    pub evidence: BTreeMap<String, Evidence>,
    pub issue_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessParty {
    pub id: String,
    pub role: String,
    pub field_prefix: String,
    pub role_evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessField {
    pub state: FieldState,
    pub value: Option<FieldValue>,
    pub confidence: Option<f64>,
    pub confidence_kind: String,
    pub reason_codes: Vec<String>,
    pub candidates: Vec<BusinessCandidate>,
    pub evidence_refs: Vec<String>,
    /// A machine `found` state never grants approval.
    pub approval: String,
    pub correction: Option<crate::review::Correction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessCandidate {
    pub value: FieldValue,
    pub origins: Vec<String>,
    pub methods: Vec<String>,
    pub evidence_refs: Vec<String>,
}

impl BusinessResult {
    pub fn from_extraction(id: Uuid, extraction: ExtractionResult, review: ReviewSnapshot) -> Self {
        let mut evidence = BTreeMap::new();
        let mut fields = BTreeMap::new();
        for (name, field) in &extraction.fields {
            fields.insert(
                name.clone(),
                project_field(
                    field,
                    &mut evidence,
                    &extraction.issues,
                    &[format!("fields.{name}")],
                ),
            );
        }
        let mut parties = Vec::new();
        for party in &extraction.parties {
            let role = match party.role {
                PartyRole::Landlord => "landlord",
                PartyRole::Tenant => "tenant",
                PartyRole::Unknown => "unknown",
            };
            let count = extraction
                .parties
                .iter()
                .filter(|p| p.role == party.role)
                .count();
            let prefix = if role != "unknown" && count == 1 {
                role.to_owned()
            } else {
                format!("party:{}", party.id)
            };
            parties.push(BusinessParty {
                id: party.id.clone(),
                role: role.to_owned(),
                field_prefix: prefix.clone(),
                role_evidence_refs: party
                    .role_evidence
                    .iter()
                    .map(|ev| intern(ev, &mut evidence))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            });
            for (name, field) in &party.fields {
                fields.insert(
                    format!("{prefix}.{name}"),
                    project_field(
                        field,
                        &mut evidence,
                        &extraction.issues,
                        &[
                            format!("parties.{}.fields.{name}", party.id),
                            format!("parties.{}.contract_fields.{name}", party.id),
                            format!("parties.{}.identity_fields.{name}", party.id),
                        ],
                    ),
                );
            }
        }
        for (path, correction) in &review.corrections {
            if let Some(field) = fields.get_mut(path) {
                field.correction = Some(correction.clone());
                field.value = Some(correction.value.clone());
                field.approval = if review.status == "approved" {
                    "human_approved"
                } else {
                    "human_corrected_requires_review"
                }
                .into();
            }
        }
        if review.status == "approved" {
            for field in fields.values_mut().filter(|f| f.value.is_some()) {
                field.approval = "human_approved".into();
            }
        }
        Self {
            schema_version: "ocr-business/1.0.0".into(),
            job_id: id,
            source_sha256: None,
            extraction_profile: extraction.profile_version,
            supported_template: extraction.supported_template,
            machine_decision: "needs_review".into(),
            review,
            parties,
            fields,
            evidence,
            issue_codes: extraction
                .issues
                .into_iter()
                .map(|i| i.code)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}

fn intern(evidence: &Evidence, registry: &mut BTreeMap<String, Evidence>) -> String {
    // A full evidence object, including source, span, quote and geometry, defines identity.
    let bytes = serde_json::to_vec(evidence).expect("evidence serializes");
    let key = format!("ev_{:x}", Sha256::digest(bytes));
    registry
        .entry(key.clone())
        .or_insert_with(|| evidence.clone());
    key
}

fn project_field(
    field: &FieldResult,
    registry: &mut BTreeMap<String, Evidence>,
    issues: &[ocr_extraction::Issue],
    issue_paths: &[String],
) -> BusinessField {
    let mut candidates: BTreeMap<String, BusinessCandidate> = BTreeMap::new();
    let mut refs = BTreeSet::new();
    for candidate in &field.candidates {
        let key = serde_json::to_string(&candidate.value).expect("value serializes");
        let projected = candidates.entry(key).or_insert_with(|| BusinessCandidate {
            value: candidate.value.clone(),
            origins: Vec::new(),
            methods: Vec::new(),
            evidence_refs: Vec::new(),
        });
        projected.origins.push(candidate.origin.clone());
        projected.methods.push(candidate.method.clone());
        for ev in &candidate.evidence {
            let key = intern(ev, registry);
            projected.evidence_refs.push(key.clone());
            refs.insert(key);
        }
    }
    for ev in &field.observations {
        refs.insert(intern(ev, registry));
    }
    for candidate in candidates.values_mut() {
        candidate.origins.sort();
        candidate.origins.dedup();
        candidate.methods.sort();
        candidate.methods.dedup();
        candidate.evidence_refs.sort();
        candidate.evidence_refs.dedup();
    }
    let mut reasons = vec![
        match field.state {
            FieldState::Found => "machine_value_requires_review",
            FieldState::Missing => "field_not_observed",
            FieldState::Unreadable => "anchored_value_unresolved",
            FieldState::Ambiguous => "value_selection_unresolved",
            FieldState::Conflict => "cross_source_conflict",
        }
        .to_owned(),
    ];
    reasons.push("confidence_uncalibrated".into());
    if field.state == FieldState::Conflict {
        reasons.push("source_conflict".into());
    }
    for issue in issues.iter().filter(|issue| {
        issue
            .field_path
            .as_ref()
            .is_some_and(|path| issue_paths.contains(path))
    }) {
        if issue.code == "recovery_only_observation" {
            reasons.push("recovery_only_observation".into());
        }
        if issue.code == "invalid_or_unsupported_value" {
            reasons.push("invalid_or_unsupported_format".into());
        }
        if issue.code == "anchored_value_unreadable" {
            for ev in &issue.evidence {
                if let Some((_, tail)) = ev.quote.rsplit_once(':') {
                    let tail = tail.trim();
                    if tail.is_empty() {
                        reasons.push("observed_empty_value".into());
                    } else if tail
                        .chars()
                        .all(|c| c.is_whitespace() || matches!(c, '#' | '_' | '.'))
                    {
                        reasons.push("observed_placeholder".into());
                    }
                }
            }
        }
        if issue.code == "invalid_or_unsupported_value"
            && issue.evidence.iter().any(|ev| {
                ev.quote.contains('#')
                    || (!ev.quote.trim().is_empty()
                        && ev
                            .quote
                            .chars()
                            .all(|c| c.is_whitespace() || matches!(c, '_' | '.')))
            })
        {
            reasons.push("observed_placeholder".into());
        }
    }
    reasons.sort();
    reasons.dedup();
    BusinessField {
        state: field.state.clone(),
        value: field.selected.clone(),
        confidence: field.confidence,
        confidence_kind: "uncalibrated_extraction".into(),
        reason_codes: reasons,
        candidates: candidates.into_values().collect(),
        evidence_refs: refs.into_iter().collect(),
        approval: "requires_review".into(),
        correction: None,
    }
}

pub enum Artifact {
    Source { media_type: String, bytes: Vec<u8> },
    Json(Value),
    Markdown(String),
}

impl Store {
    /// This query does not select, decompress, deserialize, or transfer result JSON.
    pub async fn get_status(&self, id: Uuid) -> Result<Option<JobStatus>, StoreError> {
        let mut tx = self.begin_response_read().await?;
        let value = sqlx::query_as("SELECT id, status, attempts, extraction_profile, created_at, updated_at, error, status = 'succeeded' AS result_available, COALESCE((SELECT revision FROM job_review_events WHERE job_id=jobs.id ORDER BY revision DESC LIMIT 1),0) AS review_revision, COALESCE((SELECT status FROM job_review_events WHERE job_id=jobs.id ORDER BY revision DESC LIMIT 1),'needs_review') AS review_status FROM jobs WHERE id=$1")
            .bind(id).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        Ok(value)
    }

    pub async fn get_business(
        &self,
        id: Uuid,
    ) -> Result<Option<BusinessResult>, crate::review::ReviewError> {
        self.get_business_bounded(id, usize::MAX).await
    }

    /// Guard the uncompressed extraction and review JSON in the same statement
    /// snapshot, before sqlx transfers/deserializes them. This storage budget is
    /// independent of the API's final serde wire limit.
    pub async fn get_business_bounded(
        &self,
        id: Uuid,
        max_stored_bytes: usize,
    ) -> Result<Option<BusinessResult>, crate::review::ReviewError> {
        let max = i64::try_from(max_stored_bytes).unwrap_or(i64::MAX);
        let mut tx = self.begin_response_read().await?;
        let row: Option<(String, Option<Value>, Option<Value>, bool)> = sqlx::query_as("WITH candidate AS MATERIALIZED (SELECT sha256, result->'extraction' AS extraction, (SELECT snapshot FROM job_review_events WHERE job_id=jobs.id ORDER BY revision DESC LIMIT 1) AS review FROM jobs WHERE id=$1 AND status='succeeded') SELECT sha256, CASE WHEN COALESCE(octet_length(extraction::text),0)<=$2 THEN extraction END, CASE WHEN COALESCE(octet_length(review::text),0)<=$2 THEN review END, COALESCE(octet_length(extraction::text),0)>$2 OR COALESCE(octet_length(review::text),0)>$2 FROM candidate")
            .bind(id).bind(max).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        let row = match row {
            Some((_, _, _, true)) => return Err(crate::review::ReviewError::ResponseTooLarge),
            Some((sha256, extraction, review, false)) => Some((sha256, extraction, review)),
            None => None,
        };
        match row {
            None => Ok(None),
            Some((source_sha256, extraction, review)) => {
                bounded_projection(move || {
                    let extraction = extraction.ok_or(crate::review::ReviewError::Unavailable)?;
                    let extraction: ExtractionResult = serde_json::from_value(extraction)
                        .map_err(|_| crate::review::ReviewError::InvalidStoredResult)?;
                    let review: ReviewSnapshot = review
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| crate::review::ReviewError::InvalidStoredResult)?
                        .unwrap_or_default();
                    let mut business = BusinessResult::from_extraction(id, extraction, review);
                    business.source_sha256 = Some(source_sha256);
                    Ok(Some(business))
                })
                .await
            }
        }
    }

    pub async fn get_artifact(&self, id: Uuid, kind: &str) -> Result<Option<Artifact>, StoreError> {
        self.get_artifact_bounded(id, kind, usize::MAX).await
    }

    /// The SQL CASE suppresses overlimit bytes before client allocation. Source
    /// uses exact byte length; JSON uses uncompressed PostgreSQL text size.
    pub async fn get_artifact_bounded(
        &self,
        id: Uuid,
        kind: &str,
        max_stored_bytes: usize,
    ) -> Result<Option<Artifact>, StoreError> {
        let max = i64::try_from(max_stored_bytes).unwrap_or(i64::MAX);
        if kind == "source" {
            let mut tx = self.begin_response_read().await?;
            let row: Option<(String, Option<Vec<u8>>, bool)> = sqlx::query_as(
                "SELECT media_type, CASE WHEN octet_length(input)<=$2 THEN input END, octet_length(input)>$2 FROM jobs WHERE id=$1 AND octet_length(input)>0",
            ).bind(id).bind(max).fetch_optional(&mut *tx).await?;
            tx.commit().await?;
            return match row {
                Some((_, _, true)) => Err(StoreError::ResponseTooLarge),
                Some((media_type, Some(bytes), false)) => {
                    Ok(Some(Artifact::Source { media_type, bytes }))
                }
                _ => Ok(None),
            };
        }
        let expression = match kind {
            "markdown" => "result->'markdown'",
            "layout" => "result->'layout'",
            "evidence" => "result->'extraction'",
            "diagnostics" => "result->'post_processing'",
            "result" => "result",
            _ => return Ok(None),
        };
        let sql = format!(
            "WITH candidate AS MATERIALIZED (SELECT {expression} AS artifact FROM jobs WHERE id=$1 AND status='succeeded') SELECT CASE WHEN COALESCE(octet_length(artifact::text),0)<=$2 THEN artifact END, COALESCE(octet_length(artifact::text),0)>$2 FROM candidate"
        );
        let mut tx = self.begin_response_read().await?;
        let row: Option<(Option<Value>, bool)> = sqlx::query_as(&sql)
            .bind(id)
            .bind(max)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        let value = match row {
            Some((_, true)) => return Err(StoreError::ResponseTooLarge),
            Some((value, false)) => value,
            None => None,
        };
        Ok(value.filter(|v| !v.is_null()).map(|v| {
            if kind == "markdown" {
                Artifact::Markdown(v.as_str().unwrap_or("").to_owned())
            } else {
                Artifact::Json(v)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn business_deduplicates_evidence_but_preserves_conflicts_and_no_approval() {
        let evidence: Evidence = serde_json::from_value(json!({"block_id":"b1","page_number":1,"region_index":0,"source":"ocr","byte_start":0,"byte_end":3,"quote":"abc","bbox_2d":null,"geometry_scope":"block","detector_score":0.5,"detector_score_kind":"layout_detection","confidence":null})).unwrap();
        let mut result = ocr_extraction::extract(&[]);
        let mut field = FieldResult {
            state: FieldState::Conflict,
            ..FieldResult::default()
        };
        for value in ["abc", "abc", "def"] {
            field.candidates.push(ocr_extraction::Candidate {
                value: FieldValue::Text {
                    value: value.into(),
                },
                raw_value: value.into(),
                origin: "ocr".into(),
                method: "rule".into(),
                confidence: None,
                evidence: vec![evidence.clone()],
            });
        }
        result.fields.insert("test".into(), field);
        let business =
            BusinessResult::from_extraction(Uuid::nil(), result, ReviewSnapshot::default());
        assert_eq!(business.evidence.len(), 1);
        assert_eq!(business.fields["test"].candidates.len(), 2);
        assert!(business.fields["test"].value.is_none());
        assert_eq!(business.fields["test"].approval, "requires_review");
    }
    #[tokio::test]
    async fn cancelled_projection_waiter_retains_cpu_capacity_until_work_exits() {
        let before = PROJECTION_SLOTS.available_permits();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(bounded_projection(move || {
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        assert_eq!(PROJECTION_SLOTS.available_permits(), before - 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(PROJECTION_SLOTS.available_permits(), before - 1);
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while PROJECTION_SLOTS.available_permits() < before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
