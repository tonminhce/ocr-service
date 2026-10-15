//! Job-scoped, lease-fenced retry cache. A checkpoint is not a published result.
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::store::{Store, StoreError};

const MAX_ENTRY_BYTES: usize = 128 * 1024;
const MAX_JOB_BYTES: i64 = 16 * 1024 * 1024;
const MAX_JOB_ENTRIES: i64 = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum CheckpointWrite {
    Stored,
    LeaseLost,
    BudgetExceeded,
}

impl Store {
    /// Expired/stale readers cannot use checkpoints to publish a result. The
    /// final job commit independently fences the same lease token.
    pub async fn region_checkpoint(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        if !valid_key(cache_key) {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT c.content, c.content_sha256 FROM region_checkpoints c \
             JOIN jobs j ON j.id=c.job_id WHERE c.job_id=$1 AND c.cache_key=$2 \
             AND j.status='processing' AND j.lease_token=$3 \
             AND j.lease_expires_at>clock_timestamp()",
        )
        .bind(job_id)
        .bind(cache_key)
        .bind(lease_token)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let text: String = row.try_get("content")?;
        let expected: String = row.try_get("content_sha256")?;
        if text.len() > MAX_ENTRY_BYTES
            || format!("{:x}", Sha256::digest(text.as_bytes())) != expected
        {
            // A corrupt cache is never substituted for model inference.
            return Ok(None);
        }
        Ok(Some(text))
    }

    /// Serialize brief checkpoint writes on the job row, bounding the aggregate
    /// cache size even when many regional HTTP requests complete together.
    pub async fn save_region_checkpoint(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        cache_key: &str,
        content: &str,
    ) -> Result<CheckpointWrite, StoreError> {
        if !valid_key(cache_key) || content.len() > MAX_ENTRY_BYTES {
            return Ok(CheckpointWrite::BudgetExceeded);
        }
        let mut tx = self.pool.begin().await?;
        // Acquire first, then evaluate the wall-clock lease. A predicate checked
        // before waiting on an unchanged locked row may have expired by the
        // time PostgreSQL grants the lock.
        let locked: Option<Uuid> = sqlx::query_scalar("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
            .bind(job_id)
            .fetch_optional(&mut *tx)
            .await?;
        if locked.is_none() {
            return Ok(CheckpointWrite::LeaseLost);
        }
        let valid: bool = sqlx::query_scalar(
            "SELECT status='processing' AND lease_token=$2 \
             AND lease_expires_at>clock_timestamp() FROM jobs WHERE id=$1",
        )
        .bind(job_id)
        .bind(lease_token)
        .fetch_one(&mut *tx)
        .await?;
        if !valid {
            return Ok(CheckpointWrite::LeaseLost);
        }
        // Existing entries are immutable; identical cache keys must describe
        // exactly the same crop + inference/pipeline configuration.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM region_checkpoints WHERE job_id=$1 AND cache_key=$2)",
        )
        .bind(job_id)
        .bind(cache_key)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            tx.commit().await?;
            return Ok(CheckpointWrite::Stored);
        }
        let totals = sqlx::query(
            "SELECT count(*) AS entries, COALESCE(sum(octet_length(content)),0)::bigint AS bytes \
             FROM region_checkpoints WHERE job_id=$1",
        )
        .bind(job_id)
        .fetch_one(&mut *tx)
        .await?;
        if totals.try_get::<i64, _>("entries")? >= MAX_JOB_ENTRIES
            || totals.try_get::<i64, _>("bytes")? + content.len() as i64 > MAX_JOB_BYTES
        {
            return Ok(CheckpointWrite::BudgetExceeded);
        }
        let inserted = sqlx::query(
            "INSERT INTO region_checkpoints(job_id,cache_key,content,content_sha256) \
             SELECT $1,$3,$4,$5 FROM jobs WHERE id=$1 AND status='processing' \
             AND lease_token=$2 AND lease_expires_at>clock_timestamp() \
             ON CONFLICT DO NOTHING",
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(cache_key)
        .bind(content)
        .bind(format!("{:x}", Sha256::digest(content.as_bytes())))
        .execute(&mut *tx)
        .await?;
        let outcome = if inserted.rows_affected() == 1 {
            CheckpointWrite::Stored
        } else {
            CheckpointWrite::LeaseLost
        };
        tx.commit().await?;
        Ok(outcome)
    }
}

fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprints_are_fixed_lowercase_hex() {
        assert!(valid_key(&"a".repeat(64)));
        assert!(!valid_key(&"A".repeat(64)));
        assert!(!valid_key(&"x".repeat(64)));
        assert!(!valid_key(""));
    }
}
