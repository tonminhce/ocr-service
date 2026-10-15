use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Public job metadata. Document bytes and lease ownership are never API fields.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub filename: String,
    pub media_type: String,
    pub sha256: String,
    /// Frozen when submitted so retries and worker restarts use the same policy.
    #[serde(default = "default_extraction_profile")]
    pub extraction_profile: String,
    pub status: String,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct NewJob {
    pub id: Uuid,
    pub filename: String,
    pub media_type: String,
    pub sha256: String,
    pub input: Vec<u8>,
    pub idempotency_key: Option<String>,
    pub extraction_profile: String,
}

fn default_extraction_profile() -> String {
    "none".into()
}

#[derive(Debug)]
pub struct SubmitOutcome {
    pub job: Job,
    pub created: bool,
}

#[derive(Debug)]
pub struct LeasedJob {
    pub job: Job,
    pub input: Vec<u8>,
    pub lease_token: Uuid,
}

/// A temporary right to publish a job notification. The job remains durable in
/// PostgreSQL until it reaches a terminal state.
#[derive(Debug, sqlx::FromRow)]
pub struct OutboxDelivery {
    pub job_id: Uuid,
    pub token: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_job_metadata_defaults_to_no_extraction() {
        let job: Job = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(), "filename": "test.pdf", "media_type": "application/pdf",
            "sha256": "test-digest", "status": "queued", "attempts": 0,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "result": null, "error": null
        }))
        .unwrap();
        assert_eq!(job.extraction_profile, "none");
    }
}
