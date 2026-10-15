use std::time::Duration;

use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{Job, LeasedJob, NewJob, OutboxDelivery, SubmitOutcome};
use crate::metrics::OperationalSnapshot;

const JOB_COLUMNS: &str = "id, filename, media_type, sha256, extraction_profile, status, attempts, \
    created_at, updated_at, result, error";
// The server guards the JSON value before SQLx can allocate/deserialise it.
// This conservative stored-result cap is independent of the final wire cap.
const BOUNDED_JOB_COLUMNS: &str = "id, filename, media_type, sha256, extraction_profile, status, attempts, \
    created_at, updated_at, CASE WHEN $2::bigint IS NULL OR octet_length(result::text) <= $2 \
    THEN result ELSE NULL END AS result, error, \
    ($2::bigint IS NOT NULL AND COALESCE(octet_length(result::text) > $2, false)) AS response_too_large";

/// Independent request and PostgreSQL budgets. The total includes waiting for a
/// pool connection, every statement, and COMMIT. PostgreSQL timeouts are local to
/// the transaction and cannot leak into a later borrower of the connection.
#[derive(Clone, Copy, Debug)]
pub struct SubmitDeadlines {
    total: Duration,
    lock: Duration,
    statement: Duration,
}

#[derive(Debug, Error)]
#[error("enqueue deadlines must be integral positive milliseconds with lock < statement < total")]
pub struct InvalidSubmitDeadlines;

impl SubmitDeadlines {
    pub fn new(
        total: Duration,
        lock: Duration,
        statement: Duration,
    ) -> Result<Self, InvalidSubmitDeadlines> {
        let valid = [total, lock, statement].into_iter().all(|duration| {
            duration.as_millis() > 0
                && duration.as_millis() <= i32::MAX as u128
                && duration.subsec_nanos().is_multiple_of(1_000_000)
        });
        if !valid || lock >= statement || statement >= total {
            return Err(InvalidSubmitDeadlines);
        }
        Ok(Self {
            total,
            lock,
            statement,
        })
    }

    pub fn total(self) -> Duration {
        self.total
    }

    pub fn lock(self) -> Duration {
        self.lock
    }

    pub fn statement(self) -> Duration {
        self.statement
    }
}

impl Default for SubmitDeadlines {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(10),
            lock: Duration::from_secs(2),
            statement: Duration::from_secs(5),
        }
    }
}

/// A timeout is transient, but does not attest that the transaction did not
/// commit. Retry the same idempotency key to reconcile an uncertain outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EnqueueTimeoutKind {
    #[error("total deadline elapsed")]
    Deadline,
    #[error("connection pool acquire timed out")]
    Pool,
    #[error("database lock wait timed out")]
    Lock,
    #[error("database statement timed out")]
    Statement,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(
        "idempotency key was already used for different document content, media type, or extraction profile"
    )]
    IdempotencyConflict,
    #[error("pending OCR job capacity has been reached")]
    QueueFull,
    #[error("stored OCR result exceeds the response resource budget")]
    ResponseTooLarge,
    #[error("enqueue temporarily unavailable: {0}; retry using the same idempotency key")]
    EnqueueTimeout(EnqueueTimeoutKind),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),
    #[error("database connection setup failed (details redacted)")]
    ConnectionSetup,
    #[error("operational metrics query timed out")]
    OperationalSnapshotTimeout,
}

#[derive(Clone)]
pub struct Store {
    pub(crate) pool: PgPool,
}

#[derive(FromRow)]
struct OperationalSnapshotRow {
    queued_jobs: i64,
    processing_jobs: i64,
    failed_jobs_24h: i64,
    eligible_jobs: i64,
    delayed_jobs: i64,
    oldest_queued_seconds: i64,
    seconds_since_last_progress: i64,
    due_outbox_jobs: i64,
    oldest_due_outbox_seconds: i64,
    expired_leases: i64,
}

impl Store {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let options =
            crate::postgres::safe_options(url).map_err(|_| StoreError::ConnectionSetup)?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(|_| StoreError::ConnectionSetup)?;
        Ok(Self::from_pool(pool))
    }

    /// Allows callers to configure pool limits or an isolated test schema.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        crate::schema::MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Runtime startup only validates; it never runs DDL or repairs history.
    pub async fn check_schema(&self) -> Result<(), StoreError> {
        crate::schema::check(&self.pool).await?;
        Ok(())
    }

    pub async fn health(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// One bounded aggregate query for the background operational observer.
    /// Scrapes and request handlers never call this method.
    pub async fn operational_snapshot(&self) -> Result<OperationalSnapshot, StoreError> {
        let row = tokio::time::timeout(
            Duration::from_secs(3),
            sqlx::query_as::<_, OperationalSnapshotRow>(
                "SELECT \
                    (SELECT count(*) FROM jobs WHERE status = 'queued')::bigint AS queued_jobs, \
                    (SELECT count(*) FROM jobs WHERE status = 'processing')::bigint AS processing_jobs, \
                    (SELECT count(*) FROM jobs WHERE status = 'failed' AND updated_at >= clock_timestamp() - INTERVAL '24 hours')::bigint AS failed_jobs_24h, \
                    (SELECT count(*) FROM jobs WHERE status = 'queued' AND available_at <= clock_timestamp())::bigint AS eligible_jobs, \
                    (SELECT count(*) FROM jobs WHERE status = 'queued' AND available_at > clock_timestamp())::bigint AS delayed_jobs, \
                    COALESCE((SELECT GREATEST(0, EXTRACT(EPOCH FROM clock_timestamp() - min(created_at))::bigint) FROM jobs WHERE status = 'queued'), 0)::bigint AS oldest_queued_seconds, \
                    COALESCE((SELECT GREATEST(0, EXTRACT(EPOCH FROM clock_timestamp() - max(updated_at))::bigint) FROM jobs WHERE status = 'processing'), 0)::bigint AS seconds_since_last_progress, \
                    (SELECT count(*) FROM job_outbox AS outbox JOIN jobs ON jobs.id = outbox.job_id WHERE outbox.next_dispatch_at <= clock_timestamp() AND (outbox.dispatch_lease_expires_at IS NULL OR outbox.dispatch_lease_expires_at <= clock_timestamp()) AND ((jobs.status = 'queued' AND jobs.available_at <= clock_timestamp()) OR (jobs.status = 'processing' AND jobs.lease_expires_at <= clock_timestamp())))::bigint AS due_outbox_jobs, \
                    COALESCE((SELECT GREATEST(0, EXTRACT(EPOCH FROM clock_timestamp() - min(outbox.next_dispatch_at))::bigint) FROM job_outbox AS outbox JOIN jobs ON jobs.id = outbox.job_id WHERE outbox.next_dispatch_at <= clock_timestamp() AND (outbox.dispatch_lease_expires_at IS NULL OR outbox.dispatch_lease_expires_at <= clock_timestamp()) AND ((jobs.status = 'queued' AND jobs.available_at <= clock_timestamp()) OR (jobs.status = 'processing' AND jobs.lease_expires_at <= clock_timestamp()))), 0)::bigint AS oldest_due_outbox_seconds, \
                    (SELECT count(*) FROM jobs WHERE status = 'processing' AND lease_expires_at <= clock_timestamp())::bigint AS expired_leases",
            )
            .fetch_one(&self.pool),
        )
        .await
        .map_err(|_| StoreError::OperationalSnapshotTimeout)??;
        Ok(OperationalSnapshot {
            queued_jobs: row.queued_jobs.max(0) as u64,
            processing_jobs: row.processing_jobs.max(0) as u64,
            failed_jobs_24h: row.failed_jobs_24h.max(0) as u64,
            eligible_jobs: row.eligible_jobs.max(0) as u64,
            delayed_jobs: row.delayed_jobs.max(0) as u64,
            oldest_queued_seconds: row.oldest_queued_seconds.max(0) as u64,
            seconds_since_last_progress: row.seconds_since_last_progress.max(0) as u64,
            due_outbox_jobs: row.due_outbox_jobs.max(0) as u64,
            oldest_due_outbox_seconds: row.oldest_due_outbox_seconds.max(0) as u64,
            expired_leases: row.expired_leases.max(0) as u64,
            ..OperationalSnapshot::default()
        })
    }

    /// The unique key and document bytes are stored in one database transaction.
    /// Matching retries return the original job, including its original filename.
    pub async fn submit(&self, new: NewJob) -> Result<SubmitOutcome, StoreError> {
        self.submit_inner(new, None, SubmitDeadlines::default(), None)
            .await
    }

    /// Serialize brief enqueue transactions so simultaneous API requests cannot
    /// exceed the pending document budget. Existing idempotent retries still work.
    pub async fn submit_bounded(
        &self,
        new: NewJob,
        max_pending: i64,
    ) -> Result<SubmitOutcome, StoreError> {
        self.submit_bounded_with_deadlines(new, max_pending, SubmitDeadlines::default())
            .await
    }

    pub async fn submit_bounded_with_deadlines(
        &self,
        new: NewJob,
        max_pending: i64,
        deadlines: SubmitDeadlines,
    ) -> Result<SubmitOutcome, StoreError> {
        self.submit_inner(new, Some(max_pending), deadlines, None)
            .await
    }

    /// Limits stored result bytes before deserialising an idempotent replay.
    /// The HTTP serializer must enforce its own complete wire response cap.
    pub async fn submit_bounded_with_deadlines_and_response_limit(
        &self,
        new: NewJob,
        max_pending: i64,
        deadlines: SubmitDeadlines,
        max_stored_result_bytes: usize,
    ) -> Result<SubmitOutcome, StoreError> {
        self.submit_inner(
            new,
            Some(max_pending),
            deadlines,
            Some(max_stored_result_bytes),
        )
        .await
    }

    async fn submit_inner(
        &self,
        new: NewJob,
        max_pending: Option<i64>,
        deadlines: SubmitDeadlines,
        max_stored_result_bytes: Option<usize>,
    ) -> Result<SubmitOutcome, StoreError> {
        tokio::time::timeout(
            deadlines.total,
            self.submit_transaction(new, max_pending, deadlines, max_stored_result_bytes),
        )
        .await
        .map_err(|_| StoreError::EnqueueTimeout(EnqueueTimeoutKind::Deadline))?
        .map_err(|error| match error {
            StoreError::Database(sqlx::Error::PoolTimedOut) => {
                StoreError::EnqueueTimeout(EnqueueTimeoutKind::Pool)
            }
            StoreError::Database(sqlx::Error::Database(ref database)) => {
                match database.code().as_deref() {
                    Some("55P03") => StoreError::EnqueueTimeout(EnqueueTimeoutKind::Lock),
                    Some("57014") => StoreError::EnqueueTimeout(EnqueueTimeoutKind::Statement),
                    _ => error,
                }
            }
            _ => error,
        })
    }

    async fn submit_transaction(
        &self,
        new: NewJob,
        max_pending: Option<i64>,
        deadlines: SubmitDeadlines,
        max_stored_result_bytes: Option<usize>,
    ) -> Result<SubmitOutcome, StoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "SELECT set_config('lock_timeout',$1,true), set_config('statement_timeout',$2,true)",
        )
        .bind(format!("{}ms", deadlines.lock.as_millis()))
        .bind(format!("{}ms", deadlines.statement.as_millis()))
        .execute(&mut *transaction)
        .await?;
        let insert = format!(
            "INSERT INTO jobs (id, filename, media_type, sha256, input, idempotency_key, extraction_profile) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (idempotency_key) DO NOTHING RETURNING {JOB_COLUMNS}"
        );
        let existing =
            format!("SELECT {BOUNDED_JOB_COLUMNS} FROM jobs WHERE idempotency_key = $1 FOR SHARE");
        let result_limit =
            max_stored_result_bytes.map(|limit| i64::try_from(limit).unwrap_or(i64::MAX));
        let mut admission_locked = false;

        loop {
            if let Some(row) = sqlx::query(&existing)
                .bind(&new.idempotency_key)
                .bind(result_limit)
                .fetch_optional(&mut *transaction)
                .await?
            {
                let job = Job::from_row(&row)?;
                if job.sha256 != new.sha256
                    || job.media_type != new.media_type
                    || job.extraction_profile != new.extraction_profile
                {
                    return Err(StoreError::IdempotencyConflict);
                }
                if row.try_get::<bool, _>("response_too_large")? {
                    return Err(StoreError::ResponseTooLarge);
                }
                transaction.commit().await?;
                return Ok(SubmitOutcome {
                    job,
                    created: false,
                });
            }

            if let Some(max_pending) = max_pending {
                if !admission_locked {
                    // Two fixed positive int32 keys identify this application's
                    // enqueue admission lock. PostgreSQL releases it on commit/drop.
                    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
                        .bind(0x4f43_5253_i32)
                        .bind(0x4a4f_4251_i32)
                        .execute(&mut *transaction)
                        .await?;
                    admission_locked = true;
                    // Another request may have committed the same key while waiting.
                    continue;
                }
                let pending: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM jobs WHERE status IN ('queued', 'processing')",
                )
                .fetch_one(&mut *transaction)
                .await?;
                if pending >= max_pending {
                    return Err(StoreError::QueueFull);
                }
            }

            if let Some(job) = sqlx::query_as::<_, Job>(&insert)
                .bind(new.id)
                .bind(&new.filename)
                .bind(&new.media_type)
                .bind(&new.sha256)
                .bind(&new.input)
                .bind(&new.idempotency_key)
                .bind(&new.extraction_profile)
                .fetch_optional(&mut *transaction)
                .await?
            {
                transaction.commit().await?;
                return Ok(SubmitOutcome { job, created: true });
            }

            // A concurrent submission can create the key after the initial SELECT.
            // READ COMMITTED gives the next iteration a fresh snapshot. If retention
            // deleted the conflicting row, creation can be attempted again.
        }
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Job>, StoreError> {
        let query = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = $1");
        Ok(sqlx::query_as(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn get_bounded(
        &self,
        id: Uuid,
        max_stored_result_bytes: usize,
    ) -> Result<Option<Job>, StoreError> {
        let mut transaction = self.begin_response_read().await?;
        let query = format!("SELECT {BOUNDED_JOB_COLUMNS} FROM jobs WHERE id = $1");
        let row = sqlx::query(&query)
            .bind(id)
            .bind(i64::try_from(max_stored_result_bytes).unwrap_or(i64::MAX))
            .fetch_optional(&mut *transaction)
            .await?;
        let job = match row {
            Some(row) if row.try_get::<bool, _>("response_too_large")? => {
                return Err(StoreError::ResponseTooLarge);
            }
            Some(row) => Some(Job::from_row(&row)?),
            None => None,
        };
        transaction.commit().await?;
        Ok(job)
    }

    /// API-facing reads share finite local SQL budgets. The HTTP caller also
    /// applies a total deadline covering pool acquisition and this transaction.
    pub(crate) async fn begin_response_read(
        &self,
    ) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "SELECT set_config('lock_timeout','2s',true), set_config('statement_timeout','5s',true)",
        )
        .execute(&mut *transaction)
        .await?;
        Ok(transaction)
    }

    /// Lease a bounded batch of notifications, skipping other dispatchers' rows.
    /// Records are retained after delivery so Redis loss is repaired by replay.
    /// `batch_size` and `lease_seconds` must be positive validated settings.
    pub async fn claim_outbox(
        &self,
        batch_size: i64,
        lease_seconds: i64,
    ) -> Result<Vec<OutboxDelivery>, StoreError> {
        let token = Uuid::new_v4();
        Ok(sqlx::query_as(
            "WITH candidates AS ( \
                SELECT outbox.job_id FROM job_outbox AS outbox \
                JOIN jobs ON jobs.id = outbox.job_id \
                WHERE outbox.next_dispatch_at <= clock_timestamp() \
                    AND (outbox.dispatch_lease_expires_at IS NULL \
                        OR outbox.dispatch_lease_expires_at <= clock_timestamp()) \
                    AND ((jobs.status = 'queued' AND jobs.available_at <= clock_timestamp()) \
                        OR (jobs.status = 'processing' \
                            AND jobs.lease_expires_at <= clock_timestamp())) \
                ORDER BY outbox.next_dispatch_at, outbox.job_id LIMIT $1 \
                FOR UPDATE OF outbox SKIP LOCKED \
             ) UPDATE job_outbox SET dispatch_token = $2, \
                dispatch_lease_expires_at = clock_timestamp() + ($3::bigint * INTERVAL '1 second') \
             WHERE job_id IN (SELECT job_id FROM candidates) \
             RETURNING job_id, dispatch_token AS token",
        )
        .bind(batch_size)
        .bind(token)
        .bind(lease_seconds)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Call only after Redis confirms XADD. An expired or replaced publisher
    /// cannot postpone a newer dispatch, retry, or terminal transition.
    pub async fn mark_dispatched(
        &self,
        job_id: Uuid,
        token: Uuid,
        redispatch_seconds: i64,
    ) -> Result<bool, StoreError> {
        let changed = sqlx::query(
            "UPDATE job_outbox SET \
                next_dispatch_at = clock_timestamp() + ($3::bigint * INTERVAL '1 second'), \
                dispatch_token = NULL, dispatch_lease_expires_at = NULL \
             WHERE job_id = $1 AND dispatch_token = $2 \
                AND dispatch_lease_expires_at > clock_timestamp()",
        )
        .bind(job_id)
        .bind(token)
        .bind(redispatch_seconds)
        .execute(&self.pool)
        .await?;
        Ok(changed.rows_affected() == 1)
    }

    /// Claim a Redis notification against PostgreSQL's authoritative state.
    /// Duplicate, delayed, or already-owned notifications cannot spend attempts.
    /// An expired final attempt is terminalized, removing its outbox record.
    pub async fn claim_by_id(
        &self,
        id: Uuid,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> Result<Option<LeasedJob>, StoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE jobs SET status = 'failed', lease_token = NULL, lease_expires_at = NULL, \
                error = CASE WHEN status = 'processing' \
                    THEN 'worker lease expired after maximum attempts' \
                    ELSE COALESCE(error, 'maximum attempts exhausted') END, \
                updated_at = clock_timestamp() \
             WHERE id = $1 AND attempts >= $2 AND ( \
                (status = 'queued' AND available_at <= clock_timestamp()) OR \
                (status = 'processing' AND lease_expires_at <= clock_timestamp()) \
             )",
        )
        .bind(id)
        .bind(max_attempts)
        .execute(&mut *transaction)
        .await?;

        let claim = format!(
            "UPDATE jobs SET status = 'processing', attempts = attempts + 1, \
                lease_token = $2, \
                lease_expires_at = clock_timestamp() + ($3::bigint * INTERVAL '1 second'), \
                updated_at = clock_timestamp() \
             WHERE id = $1 AND attempts < $4 AND ( \
                (status = 'queued' AND available_at <= clock_timestamp()) OR \
                (status = 'processing' AND lease_expires_at <= clock_timestamp()) \
             ) RETURNING {JOB_COLUMNS}, input, lease_token"
        );
        let leased = sqlx::query(&claim)
            .bind(id)
            .bind(Uuid::new_v4())
            .bind(lease_seconds)
            .bind(max_attempts)
            .fetch_optional(&mut *transaction)
            .await?
            .map(|row| {
                Ok::<_, sqlx::Error>(LeasedJob {
                    job: Job::from_row(&row)?,
                    input: row.try_get("input")?,
                    lease_token: row.try_get("lease_token")?,
                })
            })
            .transpose()?;
        transaction.commit().await?;
        Ok(leased)
    }

    /// Claiming commits before OCR begins. Every claim gets a fresh fencing token.
    /// `lease_seconds` and `max_attempts` must be positive validated settings.
    pub async fn claim(
        &self,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> Result<Option<LeasedJob>, StoreError> {
        let mut transaction = self.pool.begin().await?;

        // Bound housekeeping work and skip rows owned by concurrent transactions.
        sqlx::query(
            "WITH exhausted AS ( \
                SELECT id FROM jobs \
                WHERE attempts >= $1 AND ( \
                    (status = 'processing' AND lease_expires_at <= clock_timestamp()) OR \
                    (status = 'queued' AND available_at <= clock_timestamp()) \
                ) ORDER BY created_at, id LIMIT 100 FOR UPDATE SKIP LOCKED \
             ) UPDATE jobs SET status = 'failed', lease_token = NULL, lease_expires_at = NULL, \
                error = CASE WHEN status = 'processing' \
                    THEN 'worker lease expired after maximum attempts' \
                    ELSE COALESCE(error, 'maximum attempts exhausted') END, \
                updated_at = clock_timestamp() \
             WHERE id IN (SELECT id FROM exhausted)",
        )
        .bind(max_attempts)
        .execute(&mut *transaction)
        .await?;

        let token = Uuid::new_v4();
        let claim = format!(
            "WITH candidate AS ( \
                SELECT id FROM jobs WHERE attempts < $1 AND ( \
                    (status = 'queued' AND available_at <= clock_timestamp()) OR \
                    (status = 'processing' AND lease_expires_at <= clock_timestamp()) \
                ) ORDER BY available_at, created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED \
             ) UPDATE jobs SET status = 'processing', attempts = attempts + 1, \
                lease_token = $2, \
                lease_expires_at = clock_timestamp() + ($3::bigint * INTERVAL '1 second'), \
                updated_at = clock_timestamp() \
             WHERE id IN (SELECT id FROM candidate) \
             RETURNING {JOB_COLUMNS}, input, lease_token"
        );
        let leased = sqlx::query(&claim)
            .bind(max_attempts)
            .bind(token)
            .bind(lease_seconds)
            .fetch_optional(&mut *transaction)
            .await?
            .map(|row| {
                Ok::<_, sqlx::Error>(LeasedJob {
                    job: Job::from_row(&row)?,
                    input: row.try_get("input")?,
                    lease_token: row.try_get("lease_token")?,
                })
            })
            .transpose()?;
        transaction.commit().await?;
        Ok(leased)
    }

    /// Acquire the row before evaluating wall-clock lease predicates. PostgreSQL
    /// may qualify an UPDATE before waiting on an unchanged locked row; that
    /// qualification is not refreshed just because the clock advanced. Every
    /// fenced job mutation must run its fresh predicate in a subsequent statement.
    async fn lock_job_write(
        &self,
        id: Uuid,
    ) -> Result<Option<Transaction<'static, Postgres>>, StoreError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT set_config('lock_timeout','2s',true), set_config('statement_timeout','5s',true)")
            .execute(&mut *transaction).await?;
        let locked: Option<Uuid> = sqlx::query_scalar("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await?;
        if locked.is_none() {
            return Ok(None);
        }
        Ok(Some(transaction))
    }

    /// A lost lease is never revived, even before another worker reclaims the job.
    pub async fn renew(
        &self,
        id: Uuid,
        token: Uuid,
        lease_seconds: i64,
    ) -> Result<bool, StoreError> {
        let Some(mut transaction) = self.lock_job_write(id).await? else {
            return Ok(false);
        };
        let changed = sqlx::query(
            "UPDATE jobs SET \
                lease_expires_at = clock_timestamp() + ($3::bigint * INTERVAL '1 second'), \
                updated_at = clock_timestamp() \
             WHERE id = $1 AND lease_token = $2 AND status = 'processing' \
                AND lease_expires_at > clock_timestamp()",
        )
        .bind(id)
        .bind(token)
        .bind(lease_seconds)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(changed.rows_affected() == 1)
    }

    pub async fn complete(&self, id: Uuid, token: Uuid, result: Value) -> Result<bool, StoreError> {
        self.complete_with_input_policy(id, token, result, false)
            .await
    }

    /// Retain the original through terminal retention for provenance, review,
    /// and re-reading. The same unexpired lease fences this write.
    pub async fn complete_retaining_input(
        &self,
        id: Uuid,
        token: Uuid,
        result: Value,
    ) -> Result<bool, StoreError> {
        self.complete_with_input_policy(id, token, result, true)
            .await
    }

    async fn complete_with_input_policy(
        &self,
        id: Uuid,
        token: Uuid,
        result: Value,
        retain: bool,
    ) -> Result<bool, StoreError> {
        let Some(mut transaction) = self.lock_job_write(id).await? else {
            return Ok(false);
        };
        let changed = sqlx::query(
            "UPDATE jobs SET status = 'succeeded', \
                result = CASE WHEN jsonb_typeof($3::jsonb->'source') = 'object' \
                    AND ($3::jsonb->'source') ? 'original_retained' \
                    THEN jsonb_set($3::jsonb, '{source,original_retained}', \
                        to_jsonb(($4 OR retention_active_holds > 0) AND octet_length(input) > 0)) \
                    ELSE $3::jsonb END, error = NULL, \
                input = CASE WHEN $4 OR retention_active_holds > 0 THEN input ELSE ''::bytea END, \
                lease_token = NULL, lease_expires_at = NULL, updated_at = clock_timestamp() \
             WHERE id = $1 AND lease_token = $2 AND status = 'processing' \
                AND lease_expires_at > clock_timestamp()",
        )
        .bind(id)
        .bind(token)
        .bind(result)
        .bind(retain)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(changed.rows_affected() == 1)
    }

    pub async fn fail(
        &self,
        id: Uuid,
        token: Uuid,
        error: &str,
        retryable: bool,
        max_attempts: i32,
        retry_delay_seconds: i64,
    ) -> Result<bool, StoreError> {
        let Some(mut transaction) = self.lock_job_write(id).await? else {
            return Ok(false);
        };
        let changed = sqlx::query(
            "UPDATE jobs SET \
                status = CASE WHEN $4 AND attempts < $5 THEN 'queued' ELSE 'failed' END, \
                available_at = clock_timestamp() + ($6::bigint * INTERVAL '1 second'), \
                error = $3, lease_token = NULL, lease_expires_at = NULL, \
                updated_at = clock_timestamp() \
             WHERE id = $1 AND lease_token = $2 AND status = 'processing' \
                AND lease_expires_at > clock_timestamp()",
        )
        .bind(id)
        .bind(token)
        .bind(error)
        .bind(retryable)
        .bind(max_attempts)
        .bind(retry_delay_seconds)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(changed.rows_affected() == 1)
    }

    /// Legacy pruning entry point. Migration 0006 blocks every matching DELETE;
    /// an empty or entirely locked candidate batch can still report zero.
    /// No worker calls this method. Use the bounded retention dry-run instead.
    pub async fn prune(&self, retention_days: i64) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "WITH expired AS ( \
                SELECT id FROM jobs WHERE status IN ('succeeded', 'failed') \
                AND updated_at < clock_timestamp() - ($1::bigint * INTERVAL '1 day') \
                ORDER BY updated_at, id LIMIT 1000 FOR UPDATE SKIP LOCKED \
             ) DELETE FROM jobs WHERE id IN (SELECT id FROM expired)",
        )
        .bind(retention_days)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}
