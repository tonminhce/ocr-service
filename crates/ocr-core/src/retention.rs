//! Hold controls and bounded dry-run snapshots. There is no destructive apply.
use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::store::Store;

pub const SCHEMA_VERSION: &str = "ocr_retention_v1";
pub const MAX_PLAN_ROWS: usize = 100;
pub const MAX_ACTIVE_HOLDS: u32 = 128;
pub const MAX_RETENTION_EVENTS: i64 = 10_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetentionAction {
    Hold,
    Release,
}

impl RetentionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionHoldRequest {
    pub job_id: Uuid,
    pub request_id: Uuid,
    pub expected_revision: i64,
    pub hold_id: Uuid,
    pub action: RetentionAction,
    pub reason: String,
}

impl RetentionHoldRequest {
    pub fn validate(&self) -> Result<(), RetentionError> {
        if self.job_id.is_nil() || self.request_id.is_nil() || self.hold_id.is_nil() {
            return Err(RetentionError::InvalidRequest(
                "identifiers must be non-nil UUIDs",
            ));
        }
        if !(0..=MAX_RETENTION_EVENTS).contains(&self.expected_revision) {
            return Err(RetentionError::InvalidRequest(
                "expected_revision must be 0..=10000",
            ));
        }
        if self.reason.trim().is_empty()
            || self.reason.len() > 2000
            || self
                .reason
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(RetentionError::InvalidRequest(
                "reason must contain 1..2000 UTF-8 bytes without unsafe controls",
            ));
        }
        Ok(())
    }
}

/// Replayed outcomes are the original event snapshot, not current hold state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionHoldOutcome {
    pub schema_version: String,
    pub job_id: Uuid,
    pub request_id: Uuid,
    pub hold_id: Uuid,
    pub action: RetentionAction,
    pub revision: i64,
    pub active_holds: u32,
    pub source_present: bool,
    pub replayed: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetentionCursor {
    pub created_at: DateTime<Utc>,
    pub job_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPlanRequest {
    pub limit: usize,
    pub cursor: Option<RetentionCursor>,
}

impl Default for RetentionPlanRequest {
    fn default() -> Self {
        Self {
            limit: 50,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RetentionExclusion {
    ApplyDisabled,
    Nonterminal,
    UnassignedPolicy,
    Held,
    NotDue,
    ReviewRevisionChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPlanItem {
    pub job_id: Uuid,
    pub status: String,
    pub retention_revision: i64,
    pub active_holds: u32,
    pub source_present: bool,
    pub source_payload_bytes: u64,
    /// PostgreSQL datum size, with its compression/TOAST semantics. This is not
    /// uncompressed JSON size, total disk usage, or reclaimable disk space.
    pub result_stored_datum_bytes: u64,
    pub policy_id: Option<String>,
    pub retention_class: Option<String>,
    pub retain_until: Option<DateTime<Utc>>,
    pub captured_review_revision: Option<i64>,
    pub latest_review_revision: i64,
    /// Only metadata predicates; deletion remains disabled even when true.
    pub metadata_due: bool,
    pub exclusion_reasons: Vec<RetentionExclusion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPlan {
    pub schema_version: String,
    pub apply_enabled: bool,
    pub snapshot_at: DateTime<Utc>,
    pub page_rows: usize,
    /// Counts refer only to returned items. Reasons overlap, so their sum may
    /// exceed page_rows; these are not totals across the database/cursor pages.
    pub page_reason_counts: BTreeMap<RetentionExclusion, usize>,
    pub items: Vec<RetentionPlanItem>,
    pub has_more: bool,
    pub next_cursor: Option<RetentionCursor>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RetentionError {
    #[error("retention request invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("retention job not found")]
    NotFound,
    #[error("retention revision or request fingerprint conflict")]
    Conflict,
    #[error("retention hold not found")]
    HoldNotFound,
    #[error("retention active hold budget reached")]
    HoldLimit,
    #[error("retention event budget reached")]
    EventLimit,
    #[error("retention state is inconsistent")]
    InconsistentState,
    #[error("retention apply is disabled pending explicit policy and restore gates")]
    ApplyDisabled,
    #[error("retention operation deadline elapsed")]
    Deadline,
    #[error("retention database operation failed (details redacted)")]
    Database,
}

impl RetentionError {
    fn database(error: sqlx::Error) -> Self {
        match error.as_database_error().map(|e| e.message()) {
            Some("retention_invalid_request") => {
                Self::InvalidRequest("database rejected request fields")
            }
            Some("retention_job_not_found") => Self::NotFound,
            Some("retention_conflict") => Self::Conflict,
            Some("retention_hold_not_found") => Self::HoldNotFound,
            Some("retention_hold_limit") => Self::HoldLimit,
            Some("retention_event_limit") => Self::EventLimit,
            Some("retention_inconsistent_state") => Self::InconsistentState,
            _ => Self::Database,
        }
    }
}

#[derive(FromRow)]
struct PlanRow {
    id: Uuid,
    status: String,
    created_at: DateTime<Utc>,
    retention_revision: i64,
    retention_active_holds: i32,
    source_payload_bytes: i64,
    result_stored_datum_bytes: i64,
    retention_policy_id: Option<String>,
    retention_class: Option<String>,
    retain_until: Option<DateTime<Utc>>,
    retention_review_revision: Option<i64>,
    latest_review_revision: i64,
}

impl Store {
    pub async fn change_retention_hold(
        &self,
        request: RetentionHoldRequest,
    ) -> Result<RetentionHoldOutcome, RetentionError> {
        request.validate()?;
        tokio::time::timeout(Duration::from_secs(10),async {
            let mut tx=self.pool.begin().await.map_err(RetentionError::database)?;
            sqlx::query("SET LOCAL lock_timeout='2s'").execute(&mut *tx).await.map_err(RetentionError::database)?;
            sqlx::query("SET LOCAL statement_timeout='5s'").execute(&mut *tx).await.map_err(RetentionError::database)?;
            let row=sqlx::query("SELECT revision,active_holds,source_present,replayed,created_at FROM change_job_retention_hold($1,$2,$3,$4,$5,$6)")
                .bind(request.job_id).bind(request.request_id).bind(request.expected_revision)
                .bind(request.hold_id).bind(request.action.as_str()).bind(&request.reason)
                .fetch_one(&mut *tx).await.map_err(RetentionError::database)?;
            let count:i32=row.try_get("active_holds").map_err(|_|RetentionError::Database)?;
            let outcome=RetentionHoldOutcome{schema_version:SCHEMA_VERSION.into(),job_id:request.job_id,
                request_id:request.request_id,hold_id:request.hold_id,action:request.action,
                revision:row.try_get("revision").map_err(|_|RetentionError::Database)?,
                active_holds:u32::try_from(count).map_err(|_|RetentionError::InconsistentState)?,
                source_present:row.try_get("source_present").map_err(|_|RetentionError::Database)?,
                replayed:row.try_get("replayed").map_err(|_|RetentionError::Database)?,
                created_at:row.try_get("created_at").map_err(|_|RetentionError::Database)?};
            tx.commit().await.map_err(RetentionError::database)?;
            Ok(outcome)
        }).await.map_err(|_|RetentionError::Deadline)?
    }

    /// Independent pages are independent bounded snapshots, not a consistent
    /// transaction spanning every cursor page or an authorization to delete.
    pub async fn retention_plan(
        &self,
        request: RetentionPlanRequest,
    ) -> Result<RetentionPlan, RetentionError> {
        if !(1..=MAX_PLAN_ROWS).contains(&request.limit)
            || request.cursor.as_ref().is_some_and(|c| c.job_id.is_nil())
        {
            return Err(RetentionError::InvalidRequest(
                "plan limit must be 1..=100 and cursor ID must be non-nil",
            ));
        }
        tokio::time::timeout(Duration::from_secs(10),async {
            let mut tx=self.pool.begin().await.map_err(RetentionError::database)?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.map_err(RetentionError::database)?;
            sqlx::query("SET LOCAL lock_timeout='2s'").execute(&mut *tx).await.map_err(RetentionError::database)?;
            sqlx::query("SET LOCAL statement_timeout='5s'").execute(&mut *tx).await.map_err(RetentionError::database)?;
            // Establish the same snapshot used by the SELECT below. Database
            // time is a reporting reference, never a deletion token.
            let snapshot_at:DateTime<Utc>=sqlx::query_scalar("SELECT statement_timestamp()")
                .fetch_one(&mut *tx).await.map_err(RetentionError::database)?;
            // Two fixed predicates keep the continuation tuple directly
            // indexable even after PostgreSQL selects a generic prepared plan.
            let predicate=if request.cursor.is_some() {"(j.created_at,j.id)>($1::timestamptz,$2::uuid)"}
                else {"$1::timestamptz IS NULL AND $2::uuid IS NULL"};
            let query=format!("SELECT j.id,j.status,j.created_at,j.retention_revision,j.retention_active_holds,octet_length(j.input)::bigint AS source_payload_bytes,COALESCE(pg_column_size(j.result),0)::bigint AS result_stored_datum_bytes,j.retention_policy_id,j.retention_class,j.retain_until,j.retention_review_revision,COALESCE((SELECT e.revision FROM job_review_events e WHERE e.job_id=j.id ORDER BY e.revision DESC LIMIT 1),0)::bigint AS latest_review_revision FROM jobs j WHERE {predicate} ORDER BY j.created_at,j.id LIMIT $3");
            let rows=sqlx::query_as::<_,PlanRow>(&query)
                .bind(request.cursor.as_ref().map(|c|c.created_at)).bind(request.cursor.as_ref().map(|c|c.job_id))
                .bind((request.limit+1) as i64).fetch_all(&mut *tx).await.map_err(RetentionError::database)?;
            tx.commit().await.map_err(RetentionError::database)?;
            let has_more=rows.len()>request.limit;
            let mut items=Vec::with_capacity(request.limit);
            let mut counts=BTreeMap::new();
            let mut last_cursor=None;
            for row in rows.into_iter().take(request.limit) {
                let mut reasons=Vec::with_capacity(6);
                reasons.push(RetentionExclusion::ApplyDisabled);
                if !matches!(row.status.as_str(),"succeeded"|"failed") {reasons.push(RetentionExclusion::Nonterminal);}
                if row.retention_policy_id.is_none() || row.retention_class.is_none() || row.retain_until.is_none() || row.retention_review_revision.is_none() {reasons.push(RetentionExclusion::UnassignedPolicy);}
                if row.retention_active_holds>0 {reasons.push(RetentionExclusion::Held);}
                if row.retain_until.is_some_and(|until|until>snapshot_at) {reasons.push(RetentionExclusion::NotDue);}
                if row.retention_review_revision.is_some_and(|revision|revision!=row.latest_review_revision) {reasons.push(RetentionExclusion::ReviewRevisionChanged);}
                for reason in &reasons {*counts.entry(*reason).or_insert(0)+=1;}
                last_cursor=Some(RetentionCursor{created_at:row.created_at,job_id:row.id});
                items.push(RetentionPlanItem{job_id:row.id,status:row.status,retention_revision:row.retention_revision,
                    active_holds:u32::try_from(row.retention_active_holds).map_err(|_|RetentionError::InconsistentState)?,
                    source_present:row.source_payload_bytes>0,
                    source_payload_bytes:u64::try_from(row.source_payload_bytes).map_err(|_|RetentionError::InconsistentState)?,
                    result_stored_datum_bytes:u64::try_from(row.result_stored_datum_bytes).map_err(|_|RetentionError::InconsistentState)?,
                    policy_id:row.retention_policy_id,retention_class:row.retention_class,retain_until:row.retain_until,
                    captured_review_revision:row.retention_review_revision,latest_review_revision:row.latest_review_revision,
                    metadata_due:reasons.len()==1,exclusion_reasons:reasons});
            }
            Ok(RetentionPlan{schema_version:SCHEMA_VERSION.into(),apply_enabled:false,snapshot_at,
                page_rows:items.len(),page_reason_counts:counts,items,has_more,
                next_cursor:if has_more {last_cursor}else{None}})
        }).await.map_err(|_|RetentionError::Deadline)?
    }

    pub async fn retention_apply(&self) -> Result<(), RetentionError> {
        Err(RetentionError::ApplyDisabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_bounds_and_service_actor_contract() {
        let mut r = RetentionHoldRequest {
            job_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            expected_revision: 0,
            hold_id: Uuid::new_v4(),
            action: RetentionAction::Hold,
            reason: "Operational hold".into(),
        };
        assert!(r.validate().is_ok());
        for reason in [
            "".to_string(),
            " ".to_string(),
            "x\r".to_string(),
            "é".repeat(1001),
        ] {
            r.reason = reason;
            assert!(r.validate().is_err());
        }
        let mut value = serde_json::to_value(&r).unwrap();
        value["principal"] = "claimed-human".into();
        assert!(serde_json::from_value::<RetentionHoldRequest>(value).is_err());
    }
}
