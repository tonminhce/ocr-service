//! Revisioned human decisions. A configured credential principal is the actor;
//! callers cannot claim an authenticated human identity in request JSON.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ocr_extraction::{FieldState, FieldValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::{projection::BusinessResult, store::Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub value: FieldValue,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub request_id: Uuid,
    pub expected_revision: i64,
    pub action: ReviewAction,
    pub reason: String,
    #[serde(default)]
    pub corrections: BTreeMap<String, Correction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Correct,
    Approve,
    Reject,
    Reopen,
}

impl ReviewAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Reopen => "reopen",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSnapshot {
    pub revision: i64,
    pub status: String,
    pub corrections: BTreeMap<String, Correction>,
    pub principal: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl Default for ReviewSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            status: "needs_review".into(),
            corrections: BTreeMap::new(),
            principal: None,
            decided_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ReviewEvent {
    pub revision: i64,
    pub request_id: Uuid,
    pub principal: String,
    pub action: String,
    pub reason: String,
    pub request: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ReviewHistory {
    pub snapshot: ReviewSnapshot,
    pub events: Vec<ReviewEvent>,
    /// Request after_revision=next_after_revision while has_more=true.
    pub next_after_revision: i64,
    pub has_more: bool,
}

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("job not found or expired")]
    NotFound,
    #[error("structured extraction is unavailable for this job")]
    Unavailable,
    #[error("review revision or request fingerprint conflict")]
    Conflict,
    #[error("review request invalid: {0}")]
    Invalid(&'static str),
    #[error("stored extraction or review is malformed")]
    InvalidStoredResult,
    #[error("stored result exceeds the response allocation limit")]
    ResponseTooLarge,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl ReviewRequest {
    pub fn validate(&self) -> Result<(), ReviewError> {
        if self.expected_revision < 0 || self.expected_revision >= 10_000 {
            return Err(ReviewError::Invalid("expected_revision must be 0..9999"));
        }
        if self.request_id.is_nil() {
            return Err(ReviewError::Invalid("request_id must be a non-nil UUID"));
        }
        if self.reason.trim().is_empty()
            || self.reason.len() > 2000
            || self
                .reason
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(ReviewError::Invalid(
                "reason must contain 1..2000 UTF-8 bytes without unsafe controls",
            ));
        }
        if self.action == ReviewAction::Correct {
            if self.corrections.is_empty() || self.corrections.len() > 64 {
                return Err(ReviewError::Invalid(
                    "correct action requires 1..64 field corrections",
                ));
            }
        } else if !self.corrections.is_empty() {
            return Err(ReviewError::Invalid(
                "corrections are allowed only for action=correct",
            ));
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| ReviewError::Invalid("request cannot be serialized"))?;
        if bytes.len() > 60 * 1024 {
            return Err(ReviewError::Invalid("review request exceeds 60 KiB"));
        }
        Ok(())
    }
}

fn validate_value(path: &str, value: &FieldValue) -> Result<(), ReviewError> {
    let name = path.rsplit('.').next().unwrap_or(path);
    let ok = match (name, value) {
        ("birth_date" | "issue_date", FieldValue::Date { value }) => {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == *value)
        }
        ("id_number", FieldValue::Identifier { value }) => {
            matches!(value.len(), 9 | 12) && value.bytes().all(|b| b.is_ascii_digit())
        }
        ("bank_account", FieldValue::Identifier { value }) => {
            (5..=32).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
        }
        ("phone", FieldValue::Identifier { value }) => {
            let digits = value.strip_prefix('+').unwrap_or(value);
            (5..=20).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        }
        (
            "monthly_rent" | "deposit_amount",
            FieldValue::Money {
                amount,
                currency,
                period,
            },
        ) => {
            valid_decimal(amount)
                && currency == "VND"
                && if name == "monthly_rent" {
                    period.as_deref() == Some("month")
                } else {
                    period.is_none()
                }
        }
        ("property_area", FieldValue::Area { amount, unit }) => {
            valid_decimal(amount) && unit == "m2"
        }
        (
            "birth_date" | "issue_date" | "id_number" | "bank_account" | "phone" | "monthly_rent"
            | "deposit_amount" | "property_area",
            _,
        ) => false,
        (_, FieldValue::Text { value }) => {
            !value.trim().is_empty() && value.len() <= 2048 && !value.chars().any(char::is_control)
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ReviewError::Invalid(
            "correction has an invalid type or value for its field",
        ))
    }
}

fn valid_decimal(value: &str) -> bool {
    if value.is_empty() || value.len() > 24 {
        return false;
    }
    let mut parts = value.split('.');
    let integer = parts.next().unwrap();
    if integer.is_empty()
        || integer.len() > 18
        || !integer.bytes().all(|b| b.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
    {
        return false;
    }
    match parts.next() {
        None => true,
        Some(fraction) => {
            !fraction.is_empty()
                && fraction.len() <= 4
                && fraction.bytes().all(|b| b.is_ascii_digit())
                && parts.next().is_none()
        }
    }
}

fn apply_request(
    mut business: BusinessResult,
    request: &ReviewRequest,
    principal: &str,
    now: DateTime<Utc>,
) -> Result<ReviewSnapshot, ReviewError> {
    let mut snapshot = business.review.clone();
    if request.expected_revision != snapshot.revision {
        return Err(ReviewError::Conflict);
    }
    match request.action {
        ReviewAction::Correct => {
            for (path, correction) in &request.corrections {
                let field = business
                    .fields
                    .get(path)
                    .ok_or(ReviewError::Invalid("unknown business field path"))?;
                validate_value(path, &correction.value)?;
                let refs: BTreeSet<_> = correction.evidence_refs.iter().collect();
                if refs.is_empty()
                    || refs.len() != correction.evidence_refs.len()
                    || refs.len() > 32
                    || refs.iter().any(|r| {
                        !field.evidence_refs.contains(r) || !business.evidence.contains_key(*r)
                    })
                {
                    return Err(ReviewError::Invalid(
                        "correction requires 1..32 unique existing evidence references belonging to this field",
                    ));
                }
                snapshot
                    .corrections
                    .insert(path.clone(), correction.clone());
            }
            snapshot.status = "needs_review".into();
        }
        ReviewAction::Approve => {
            if !business.supported_template {
                return Err(ReviewError::Invalid(
                    "unsupported or budget-exhausted extraction cannot be approved",
                ));
            }
            let roles: BTreeSet<_> = business
                .parties
                .iter()
                .map(|p| p.field_prefix.as_str())
                .collect();
            if business.parties.len() != 2
                || !roles.contains("landlord")
                || !roles.contains("tenant")
            {
                return Err(ReviewError::Invalid(
                    "approval requires exactly one explicit landlord and tenant",
                ));
            }
            for (path, field) in &mut business.fields {
                if let Some(correction) = snapshot.corrections.get(path) {
                    field.value = Some(correction.value.clone());
                } else if matches!(field.state, FieldState::Conflict | FieldState::Ambiguous) {
                    return Err(ReviewError::Invalid(
                        "all conflicting or ambiguous fields require explicit evidence-linked correction",
                    ));
                }
                if let Some(value) = &field.value {
                    validate_value(path, value)?;
                    if field.evidence_refs.is_empty() {
                        return Err(ReviewError::Invalid(
                            "selected business field has no source evidence",
                        ));
                    }
                }
            }
            for path in [
                "landlord.name",
                "landlord.id_number",
                "landlord.birth_date",
                "landlord.issue_date",
                "landlord.residence_address",
                "tenant.name",
                "tenant.id_number",
                "tenant.birth_date",
                "tenant.issue_date",
                "tenant.residence_address",
                "monthly_rent",
                "property_address",
            ] {
                let field = business
                    .fields
                    .get(path)
                    .ok_or(ReviewError::Invalid("required business field is missing"))?;
                let value = field.value.as_ref().ok_or(ReviewError::Invalid(
                    "required business field has no resolved value",
                ))?;
                validate_value(path, value)?;
                if field.evidence_refs.is_empty() {
                    return Err(ReviewError::Invalid(
                        "required business field has no source evidence",
                    ));
                }
            }
            snapshot.status = "approved".into();
        }
        ReviewAction::Reject => snapshot.status = "rejected".into(),
        ReviewAction::Reopen => snapshot.status = "needs_review".into(),
    }
    snapshot.revision += 1;
    snapshot.principal = Some(principal.to_owned());
    snapshot.decided_at = Some(now);
    Ok(snapshot)
}

impl Store {
    pub async fn review_history(
        &self,
        id: Uuid,
        after_revision: i64,
    ) -> Result<ReviewHistory, ReviewError> {
        if after_revision < 0 {
            return Err(ReviewError::Invalid("after_revision must be nonnegative"));
        }
        // Latest snapshot and page share one MVCC snapshot; no document result loaded.
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *tx)
            .await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(ReviewError::NotFound);
        }
        let latest: Option<Value> = sqlx::query_scalar(
            "SELECT snapshot FROM job_review_events WHERE job_id=$1 ORDER BY revision DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let snapshot = latest
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ReviewError::InvalidStoredResult)?
            .unwrap_or_default();
        let mut events:Vec<ReviewEvent>=sqlx::query_as("SELECT revision, request_id, principal, action, reason, request, status, created_at FROM job_review_events WHERE job_id=$1 AND revision>$2 ORDER BY revision LIMIT 101").bind(id).bind(after_revision).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let has_more = events.len() > 100;
        events.truncate(100);
        let next_after_revision = events.last().map_or(after_revision, |e| e.revision);
        Ok(ReviewHistory {
            snapshot,
            events,
            next_after_revision,
            has_more,
        })
    }

    pub async fn append_review(
        &self,
        id: Uuid,
        principal: &str,
        request: ReviewRequest,
    ) -> Result<ReviewSnapshot, ReviewError> {
        self.append_review_bounded(id, principal, request, usize::MAX)
            .await
    }

    /// Reject oversized extraction before SQLx allocates it. Existing review
    /// request_id/CAS semantics and immutable OCR evidence remain unchanged.
    pub async fn append_review_bounded(
        &self,
        id: Uuid,
        principal: &str,
        request: ReviewRequest,
        max_stored_bytes: usize,
    ) -> Result<ReviewSnapshot, ReviewError> {
        request.validate()?;
        if principal.trim().is_empty()
            || principal.len() > 128
            || principal.chars().any(char::is_control)
        {
            return Err(ReviewError::Invalid(
                "configured review principal is invalid",
            ));
        }
        let request_json = serde_json::to_value(&request)
            .map_err(|_| ReviewError::Invalid("request cannot be serialized"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *tx)
            .await?;
        // Serialize decisions with job deletion and other decisions, never with worker model execution.
        let row: Option<(String, Option<Value>, bool)> = sqlx::query_as("SELECT status, CASE WHEN COALESCE(octet_length((result->'extraction')::text),0)<=$2 THEN result->'extraction' END, COALESCE(octet_length((result->'extraction')::text),0)>$2 FROM jobs WHERE id=$1 FOR UPDATE")
            .bind(id).bind(i64::try_from(max_stored_bytes).unwrap_or(i64::MAX)).fetch_optional(&mut *tx).await?;
        let (status, extraction, too_large) = row.ok_or(ReviewError::NotFound)?;
        let previous:Option<(String,Value,Value)>=sqlx::query_as("SELECT principal, request, snapshot FROM job_review_events WHERE job_id=$1 AND request_id=$2").bind(id).bind(request.request_id).fetch_optional(&mut *tx).await?;
        if let Some((stored_principal, stored_request, snapshot)) = previous {
            if stored_principal != principal || stored_request != request_json {
                return Err(ReviewError::Conflict);
            }
            let snapshot =
                serde_json::from_value(snapshot).map_err(|_| ReviewError::InvalidStoredResult)?;
            tx.commit().await?;
            return Ok(snapshot);
        }
        if too_large {
            return Err(ReviewError::ResponseTooLarge);
        }
        if status != "succeeded" {
            return Err(ReviewError::Unavailable);
        }
        let extraction = extraction
            .filter(|e| !e.is_null())
            .ok_or(ReviewError::Unavailable)?;
        let latest: Option<Value> = sqlx::query_scalar(
            "SELECT snapshot FROM job_review_events WHERE job_id=$1 ORDER BY revision DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let review = latest
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ReviewError::InvalidStoredResult)?
            .unwrap_or_default();
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let request_for_apply = request.clone();
        let principal_for_apply = principal.to_owned();
        let (snapshot, snapshot_json) = crate::projection::bounded_projection(move || {
            let extraction =
                serde_json::from_value(extraction).map_err(|_| ReviewError::InvalidStoredResult)?;
            let snapshot = apply_request(
                BusinessResult::from_extraction(id, extraction, review),
                &request_for_apply,
                &principal_for_apply,
                now,
            )?;
            let snapshot_json =
                serde_json::to_value(&snapshot).map_err(|_| ReviewError::InvalidStoredResult)?;
            if serde_json::to_vec(&snapshot_json)
                .map_err(|_| ReviewError::InvalidStoredResult)?
                .len()
                > 900 * 1024
            {
                return Err(ReviewError::Invalid(
                    "cumulative corrections exceed the review snapshot limit",
                ));
            }
            Ok((snapshot, snapshot_json))
        })
        .await?;
        sqlx::query("INSERT INTO job_review_events (job_id, revision, request_id, principal, action, reason, request, snapshot, status, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(id).bind(snapshot.revision).bind(request.request_id).bind(principal).bind(request.action.as_str()).bind(&request.reason).bind(request_json).bind(snapshot_json).bind(&snapshot.status).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correction_values_require_real_dates_strings_and_exact_decimal_units() {
        assert!(
            validate_value(
                "tenant.birth_date",
                &FieldValue::Date {
                    value: "2024-02-29".into()
                }
            )
            .is_ok()
        );
        for bad in ["2023-02-29", "2024-2-01", "2024-13-01"] {
            assert!(
                validate_value("tenant.birth_date", &FieldValue::Date { value: bad.into() })
                    .is_err()
            );
        }
        assert!(
            validate_value(
                "tenant.id_number",
                &FieldValue::Identifier {
                    value: "001204001171".into()
                }
            )
            .is_ok()
        );
        assert!(
            validate_value(
                "tenant.id_number",
                &FieldValue::Text {
                    value: "001204001171".into()
                }
            )
            .is_err()
        );
        for bad in ["NaN", "-100", "1e6", "01", "1.", "1.23456"] {
            assert!(!valid_decimal(bad));
        }
        assert!(valid_decimal("10000000"));
    }
    fn complete_business() -> BusinessResult {
        use crate::projection::{BusinessCandidate, BusinessField, BusinessParty};
        let mut result = BusinessResult::from_extraction(
            Uuid::nil(),
            ocr_extraction::extract(&[]),
            ReviewSnapshot::default(),
        );
        result.supported_template = true;
        result.parties = ["landlord", "tenant"]
            .into_iter()
            .map(|role| BusinessParty {
                id: role.into(),
                role: role.into(),
                field_prefix: role.into(),
                role_evidence_refs: vec![],
            })
            .collect();
        result.fields.clear();
        for path in [
            "landlord.name",
            "landlord.id_number",
            "landlord.birth_date",
            "landlord.issue_date",
            "landlord.residence_address",
            "tenant.name",
            "tenant.id_number",
            "tenant.birth_date",
            "tenant.issue_date",
            "tenant.residence_address",
            "monthly_rent",
            "property_address",
        ] {
            let value = if path.ends_with("id_number") {
                FieldValue::Identifier {
                    value: "001204001171".into(),
                }
            } else if path.ends_with("date") {
                FieldValue::Date {
                    value: "2004-01-04".into(),
                }
            } else if path == "monthly_rent" {
                FieldValue::Money {
                    amount: "10000000".into(),
                    currency: "VND".into(),
                    period: Some("month".into()),
                }
            } else {
                FieldValue::Text {
                    value: "source text".into(),
                }
            };
            result.fields.insert(
                path.into(),
                BusinessField {
                    state: FieldState::Found,
                    value: Some(value.clone()),
                    confidence: None,
                    confidence_kind: "uncalibrated_extraction".into(),
                    reason_codes: vec![],
                    candidates: vec![BusinessCandidate {
                        value,
                        origins: vec![],
                        methods: vec![],
                        evidence_refs: vec!["ev_test".into()],
                    }],
                    evidence_refs: vec!["ev_test".into()],
                    approval: "requires_review".into(),
                    correction: None,
                },
            );
        }
        result
    }
    #[test]
    fn approval_requires_critical_values_and_every_conflict_resolution() {
        let request = ReviewRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 0,
            action: ReviewAction::Approve,
            reason: "checked source".into(),
            corrections: BTreeMap::new(),
        };
        let business = complete_business();
        assert_eq!(
            apply_request(business.clone(), &request, "principal", Utc::now())
                .unwrap()
                .status,
            "approved"
        );
        let mut missing = business.clone();
        missing.fields.get_mut("landlord.birth_date").unwrap().value = None;
        assert!(apply_request(missing, &request, "principal", Utc::now()).is_err());
        let mut conflict = business;
        conflict.fields.get_mut("tenant.birth_date").unwrap().state = FieldState::Conflict;
        assert!(apply_request(conflict, &request, "principal", Utc::now()).is_err());
    }
    #[test]
    fn corrections_cannot_invent_fields_or_claim_unrelated_evidence() {
        let business = complete_business();
        let mut request = ReviewRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 0,
            action: ReviewAction::Correct,
            reason: "checked source".into(),
            corrections: BTreeMap::new(),
        };
        request.corrections.insert(
            "tenant.birth_date".into(),
            Correction {
                value: FieldValue::Date {
                    value: "2004-01-04".into(),
                },
                evidence_refs: vec!["ev_unrelated".into()],
            },
        );
        assert!(apply_request(business.clone(), &request, "principal", Utc::now()).is_err());
        request.corrections.clear();
        request.corrections.insert(
            "invented.field".into(),
            Correction {
                value: FieldValue::Text {
                    value: "claim".into(),
                },
                evidence_refs: vec!["ev_test".into()],
            },
        );
        assert!(apply_request(business, &request, "principal", Utc::now()).is_err());
    }
    #[test]
    fn approval_never_accepts_empty_or_unsupported_extraction() {
        let request = ReviewRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 0,
            action: ReviewAction::Approve,
            reason: "checked".into(),
            corrections: BTreeMap::new(),
        };
        let business = BusinessResult::from_extraction(
            Uuid::nil(),
            ocr_extraction::extract(&[]),
            ReviewSnapshot::default(),
        );
        assert!(apply_request(business, &request, "review-service", Utc::now()).is_err());
    }
}
