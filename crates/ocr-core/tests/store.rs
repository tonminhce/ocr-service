//! Run against a disposable PostgreSQL database:
//! TEST_DATABASE_URL=postgres://... cargo test -p ocr-core --test store -- --ignored

use std::{collections::HashSet, str::FromStr, time::Duration};

use ocr_core::{
    domain::NewJob,
    store::{EnqueueTimeoutKind, Store, StoreError, SubmitDeadlines},
};
use serde_json::json;
use sqlx::{PgPool, postgres::PgConnectOptions, postgres::PgPoolOptions};
use tokio::task::JoinSet;
use uuid::Uuid;

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
}

impl TestDatabase {
    async fn new() -> Self {
        let database = Self::unmigrated().await;
        database.store.migrate().await.unwrap();
        database
    }

    async fn unmigrated() -> Self {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must name a disposable PostgreSQL database");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        // The schema identifier is generated locally, with no caller-provided SQL.
        let schema = format!("ocr_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .application_name(&schema)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await
            .unwrap();
        let store = Store::from_pool(pool.clone());
        Self {
            admin,
            pool,
            store,
            schema,
        }
    }

    async fn expire(&self, id: Uuid) {
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = clock_timestamp() - INTERVAL '1 second' WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn outbox_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM job_outbox")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn assert_pool_recovered(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut held = Vec::new();
            for _ in 0..10 {
                let mut connection = self.pool.acquire().await.unwrap();
                let healthy: i32 = sqlx::query_scalar("SELECT 1")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap();
                assert_eq!(healthy, 1);
                held.push(connection);
            }
            let leaked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND state LIKE 'idle in transaction%'",
            )
            .bind(&self.schema)
            .fetch_one(&self.admin)
            .await
            .unwrap();
            assert_eq!(leaked, 0);
        })
        .await
        .expect("all pool connections must recover within two seconds after cancellation");
    }

    async fn make_dispatch_due(&self, id: Uuid) {
        sqlx::query(
            "UPDATE job_outbox SET next_dispatch_at = clock_timestamp() - INTERVAL '1 second', \
                dispatch_lease_expires_at = CASE WHEN dispatch_token IS NOT NULL \
                    THEN clock_timestamp() - INTERVAL '1 second' ELSE NULL END \
             WHERE job_id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn document(key: Option<&str>) -> NewJob {
    NewJob {
        id: Uuid::new_v4(),
        filename: "invoice.png".into(),
        media_type: "image/png".into(),
        sha256: "a".repeat(64),
        input: vec![1, 2, 3, 4],
        idempotency_key: key.map(str::to_owned),
        extraction_profile: "none".into(),
    }
}

fn deadlines(total_ms: u64, lock_ms: u64, statement_ms: u64) -> SubmitDeadlines {
    SubmitDeadlines::new(
        Duration::from_millis(total_ms),
        Duration::from_millis(lock_ms),
        Duration::from_millis(statement_ms),
    )
    .unwrap()
}

#[test]
fn submit_deadlines_reject_disabled_fractional_or_misordered_timeouts() {
    assert!(SubmitDeadlines::new(Duration::ZERO, Duration::ZERO, Duration::ZERO).is_err());
    assert!(
        SubmitDeadlines::new(
            Duration::from_secs(1),
            Duration::from_nanos(1_000_001),
            Duration::from_millis(10)
        )
        .is_err()
    );
    assert!(
        SubmitDeadlines::new(
            Duration::from_secs(1),
            Duration::from_millis(20),
            Duration::from_millis(20)
        )
        .is_err()
    );
    assert!(
        SubmitDeadlines::new(
            Duration::from_secs(1),
            Duration::from_millis(20),
            Duration::from_secs(1)
        )
        .is_err()
    );
    assert!(
        SubmitDeadlines::new(
            Duration::from_secs(u64::MAX),
            Duration::from_millis(1),
            Duration::from_millis(2)
        )
        .is_err()
    );
    let valid = deadlines(250, 50, 100);
    assert_eq!(valid.total(), Duration::from_millis(250));
    assert_eq!(valid.lock(), Duration::from_millis(50));
    assert_eq!(valid.statement(), Duration::from_millis(100));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn stored_result_guard_preserves_boundaries_replays_and_conflict_precedence() {
    let db = TestDatabase::new().await;
    let submitted = db
        .store
        .submit(document(Some("stored-size")))
        .await
        .unwrap();
    assert_eq!(
        db.store.get_bounded(submitted.job.id, 0).await.unwrap(),
        Some(submitted.job.clone())
    );
    assert!(
        db.store
            .get_bounded(Uuid::new_v4(), 0)
            .await
            .unwrap()
            .is_none()
    );
    let lease = db.store.claim(60, 3).await.unwrap().unwrap();
    db.store
        .complete(
            submitted.job.id,
            lease.lease_token,
            json!({"text":"Tiếng Việt", "nested":{"raw":"x".repeat(512)}}),
        )
        .await
        .unwrap();
    let bytes: i32 = sqlx::query_scalar("SELECT octet_length(result::text) FROM jobs WHERE id=$1")
        .bind(submitted.job.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let bytes = usize::try_from(bytes).unwrap();
    let legacy = db.store.get(submitted.job.id).await.unwrap().unwrap();
    let outbox_before = db.outbox_count().await;
    assert_eq!(
        db.store.get_bounded(submitted.job.id, bytes).await.unwrap(),
        Some(legacy.clone())
    );
    assert!(matches!(
        db.store.get_bounded(submitted.job.id, bytes - 1).await,
        Err(StoreError::ResponseTooLarge)
    ));
    let replay = db
        .store
        .submit_bounded_with_deadlines_and_response_limit(
            document(Some("stored-size")),
            1,
            SubmitDeadlines::default(),
            bytes,
        )
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.job, legacy);
    assert!(matches!(
        db.store
            .submit_bounded_with_deadlines_and_response_limit(
                document(Some("stored-size")),
                1,
                SubmitDeadlines::default(),
                bytes - 1,
            )
            .await,
        Err(StoreError::ResponseTooLarge)
    ));
    let mut conflicting = document(Some("stored-size"));
    conflicting.sha256 = "b".repeat(64);
    assert!(matches!(
        db.store
            .submit_bounded_with_deadlines_and_response_limit(
                conflicting,
                1,
                SubmitDeadlines::default(),
                0,
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(db.outbox_count().await, outbox_before);
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn bounded_response_read_uses_local_sql_timeout_and_pool_recovers() {
    let db = TestDatabase::new().await;
    let job = db.store.submit(document(None)).await.unwrap().job;
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let outcome = db.store.get_bounded(job.id, 1000).await;
    assert!(matches!(
        outcome,
        Err(StoreError::Database(sqlx::Error::Database(ref error)))
            if error.code().as_deref() == Some("55P03")
    ));
    assert!(started.elapsed() < Duration::from_secs(4));
    blocker.rollback().await.unwrap();
    assert_eq!(db.store.get_bounded(job.id, 1000).await.unwrap(), Some(job));
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_admission_lock_times_out_without_starving_lease_renewal() {
    let db = TestDatabase::new().await;
    let existing = db.store.submit(document(None)).await.unwrap();
    let lease = db.store.claim(60, 3).await.unwrap().unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1,$2)")
        .bind(0x4f43_5253_i32)
        .bind(0x4a4f_4251_i32)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let (submission, renewed) = tokio::join!(
        db.store.submit_bounded_with_deadlines(
            document(Some("blocked-admission")),
            10,
            deadlines(600, 80, 250),
        ),
        db.store.renew(existing.job.id, lease.lease_token, 60)
    );
    assert!(matches!(
        submission,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Lock))
    ));
    assert!(renewed.unwrap());
    assert!(started.elapsed() < Duration::from_secs(2));
    blocker.rollback().await.unwrap();
    let retry = db
        .store
        .submit_bounded_with_deadlines(
            document(Some("blocked-admission")),
            10,
            deadlines(600, 80, 250),
        )
        .await
        .unwrap();
    assert!(retry.created);
    assert_eq!(db.outbox_count().await, 2);
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_idempotency_row_lock_is_bounded_and_replay_preserves_one_outbox() {
    let db = TestDatabase::new().await;
    let existing = db.store.submit(document(Some("row-lock"))).await.unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(existing.job.id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let outcome = db
        .store
        .submit_bounded_with_deadlines(document(Some("row-lock")), 1, deadlines(600, 80, 250))
        .await;
    assert!(matches!(
        outcome,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Lock))
    ));
    blocker.rollback().await.unwrap();
    let replay = db
        .store
        .submit_bounded_with_deadlines(document(Some("row-lock")), 1, deadlines(600, 80, 250))
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.job.id, existing.job.id);
    assert_eq!(db.outbox_count().await, 1);
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_total_deadline_includes_pool_wait_and_pool_recovers() {
    let db = TestDatabase::new().await;
    let mut held = Vec::new();
    for _ in 0..10 {
        held.push(db.pool.acquire().await.unwrap());
    }
    let started = std::time::Instant::now();
    let outcome = db
        .store
        .submit_bounded_with_deadlines(document(Some("pool-wait")), 1, deadlines(100, 20, 50))
        .await;
    assert!(matches!(
        outcome,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Deadline))
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), db.store.health())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(db.outbox_count().await, 0);
    let retry = db
        .store
        .submit_bounded_with_deadlines(document(Some("pool-wait")), 1, deadlines(600, 80, 250))
        .await
        .unwrap();
    assert!(retry.created);
    assert_eq!(db.outbox_count().await, 1);
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_pool_timeout_is_classified_before_total_deadline() {
    let db = TestDatabase::new().await;
    let small_pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(40))
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    let store = Store::from_pool(small_pool.clone());
    let held = small_pool.acquire().await.unwrap();
    let outcome = store
        .submit_bounded_with_deadlines(document(Some("pool-timeout")), 1, deadlines(600, 80, 250))
        .await;
    assert!(matches!(
        outcome,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Pool))
    ));
    drop(held);
    store.health().await.unwrap();
    small_pool.close().await;
    assert_eq!(db.outbox_count().await, 0);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_statement_timeout_rolls_back_job_and_outbox_without_session_leak() {
    let db = TestDatabase::new().await;
    sqlx::raw_sql(
        "CREATE FUNCTION slow_enqueue() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(0.2); RETURN NEW; END $$;
         CREATE TRIGGER slow_enqueue BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION slow_enqueue();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let outcome = db
        .store
        .submit_bounded_with_deadlines(document(Some("statement-wait")), 1, deadlines(600, 50, 100))
        .await;
    assert!(matches!(
        outcome,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Statement))
    ));
    assert_eq!(db.outbox_count().await, 0);
    // Acquire every connection, including the one whose transaction timed out.
    let mut connections = Vec::new();
    for _ in 0..10 {
        let mut connection = db.pool.acquire().await.unwrap();
        for setting in ["SHOW lock_timeout", "SHOW statement_timeout"] {
            let value: String = sqlx::query_scalar(setting)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            assert_eq!(value, "0");
        }
        connections.push(connection);
    }
    drop(connections);
    sqlx::query("DROP TRIGGER slow_enqueue ON jobs")
        .execute(&db.pool)
        .await
        .unwrap();
    let retry = db
        .store
        .submit_bounded_with_deadlines(document(Some("statement-wait")), 1, deadlines(600, 50, 100))
        .await
        .unwrap();
    assert!(retry.created);
    assert_eq!(db.outbox_count().await, 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn cancelled_enqueue_drops_transaction_and_same_key_reconciles_exactly_once() {
    let db = TestDatabase::new().await;
    sqlx::raw_sql(
        "CREATE FUNCTION slow_enqueue() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(0.2); RETURN NEW; END $$;
         CREATE TRIGGER slow_enqueue BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION slow_enqueue();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let store = db.store.clone();
    let caller = tokio::spawn(async move {
        store
            .submit_bounded_with_deadlines(
                document(Some("cancelled-enqueue")),
                1,
                deadlines(1500, 500, 1000),
            )
            .await
    });
    // Observe the server executing the insert rather than assuming a sleep gave
    // the caller enough time to acquire a connection and enter its transaction.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let inserting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND pid<>pg_backend_pid() \
                 AND state='active' AND wait_event='PgSleep' AND query LIKE 'INSERT INTO jobs%')",
            )
            .bind(&db.schema)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if inserting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    sqlx::query("DROP TRIGGER slow_enqueue ON jobs")
        .execute(&db.pool)
        .await
        .unwrap();
    let retry = db
        .store
        .submit(document(Some("cancelled-enqueue")))
        .await
        .unwrap();
    assert!(retry.created);
    assert_eq!(db.outbox_count().await, 1);
    let replay = db
        .store
        .submit(document(Some("cancelled-enqueue")))
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(retry.job.id, replay.job.id);
    db.assert_pool_recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn enqueue_deadline_during_commit_has_uncertain_outcome_and_key_reconciles() {
    let db = TestDatabase::new().await;
    sqlx::raw_sql(
        "CREATE FUNCTION slow_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(0.10); RETURN NEW; END $$;
         CREATE TRIGGER slow_insert BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION slow_insert();
         CREATE FUNCTION slow_commit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(0.15); RETURN NEW; END $$;
         CREATE CONSTRAINT TRIGGER slow_commit AFTER INSERT ON jobs DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW EXECUTE FUNCTION slow_commit();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (outcome, observed_commit) = tokio::join!(
        db.store.submit_bounded_with_deadlines(
            document(Some("commit-timeout")),
            1,
            deadlines(200, 50, 180),
        ),
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let committing: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 \
                     AND state='active' AND wait_event='PgSleep' AND query='COMMIT')",
                )
                .bind(&db.schema)
                .fetch_one(&db.pool)
                .await
                .unwrap();
                if committing {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    );
    observed_commit.expect("the total deadline must expire after COMMIT started on the server");
    assert!(matches!(
        outcome,
        Err(StoreError::EnqueueTimeout(EnqueueTimeoutKind::Deadline))
    ));
    // DDL must wait for the original transaction to settle. A timeout alone
    // cannot distinguish a committed transaction from a rolled-back one.
    tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::raw_sql("DROP TRIGGER slow_insert ON jobs; DROP TRIGGER slow_commit ON jobs")
            .execute(&db.pool),
    )
    .await
    .unwrap()
    .unwrap();
    let retry = db
        .store
        .submit(document(Some("commit-timeout")))
        .await
        .unwrap();
    let original_committed = !retry.created;
    let replay = db
        .store
        .submit(document(Some("commit-timeout")))
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(retry.job.id, replay.job.id);
    assert_eq!(db.outbox_count().await, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    db.assert_pool_recovered().await;
    eprintln!(
        "enqueue commit trace: observed_server_commit=true total_deadline=true original_committed={original_committed} jobs={jobs} outbox=1 pool_connections_recovered=10"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn retained_source_is_lease_fenced_and_legacy_prune_is_disabled() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let stale = db.store.claim(60, 3).await.unwrap().unwrap();
    db.expire(id).await;
    let current = db.store.claim(60, 3).await.unwrap().unwrap();
    assert!(
        !db.store
            .complete_retaining_input(id, stale.lease_token, json!({"stale":true}))
            .await
            .unwrap()
    );
    assert!(
        db.store
            .complete_retaining_input(id, current.lease_token, json!({"layout":[]}))
            .await
            .unwrap()
    );
    // A duplicate/stale completion cannot clear the source retained by its owner.
    assert!(
        !db.store
            .complete(id, stale.lease_token, json!({"stale":true}))
            .await
            .unwrap()
    );
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT input FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(bytes, [1, 2, 3, 4]);
    assert_eq!(db.store.prune(7).await.unwrap(), 0);
    sqlx::query("UPDATE jobs SET updated_at = clock_timestamp() - INTERVAL '8 days' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.store.prune(7).await.is_err());
    assert!(db.store.get(id).await.unwrap().is_some());
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT input FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(bytes, [1, 2, 3, 4]);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn concurrent_submissions_keep_one_job_and_reject_conflicting_fingerprints() {
    let db = TestDatabase::new().await;
    db.store.health().await.unwrap();
    let mut submissions = JoinSet::new();
    for _ in 0..16 {
        let store = db.store.clone();
        submissions
            .spawn(async move { store.submit(document(Some("same-request"))).await.unwrap() });
    }
    let mut ids = HashSet::new();
    let mut created = 0;
    while let Some(outcome) = submissions.join_next().await {
        let outcome = outcome.unwrap();
        ids.insert(outcome.job.id);
        created += usize::from(outcome.created);
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(created, 1);

    let mut conflicting = document(Some("same-request"));
    conflicting.sha256 = "b".repeat(64);
    assert!(matches!(
        db.store.submit(conflicting).await,
        Err(StoreError::IdempotencyConflict)
    ));
    let mut wrong_media = document(Some("same-request"));
    wrong_media.media_type = "application/pdf".into();
    assert!(matches!(
        db.store.submit(wrong_media).await,
        Err(StoreError::IdempotencyConflict)
    ));
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT input FROM jobs WHERE idempotency_key = 'same-request'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(bytes, [1, 2, 3, 4]);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn extraction_profile_is_persisted_leased_and_part_of_idempotency() {
    let db = TestDatabase::new().await;
    let mut input = document(Some("profile-request"));
    input.extraction_profile = "vi_rental_contract_v1".into();
    let first = db.store.submit(input).await.unwrap();
    assert!(first.created);
    assert_eq!(first.job.extraction_profile, "vi_rental_contract_v1");
    let stored = db.store.get(first.job.id).await.unwrap().unwrap();
    assert_eq!(stored.extraction_profile, "vi_rental_contract_v1");

    let mut replay = document(Some("profile-request"));
    replay.extraction_profile = "vi_rental_contract_v1".into();
    let replay = db.store.submit_bounded(replay, 1).await.unwrap();
    assert_eq!(replay.job.id, first.job.id);
    assert!(!replay.created);
    assert!(matches!(
        db.store
            .submit_bounded(document(Some("profile-request")), 1)
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    let claimed = db
        .store
        .claim_by_id(first.job.id, 60, 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.extraction_profile, "vi_rental_contract_v1");
    let mut invalid = document(None);
    invalid.extraction_profile = "unregistered-profile".into();
    assert!(matches!(
        db.store.submit(invalid).await,
        Err(StoreError::Database(_))
    ));
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn extraction_profile_migration_preserves_existing_jobs_as_ocr_only() {
    let db = TestDatabase::unmigrated().await;
    sqlx::raw_sql(include_str!("../migrations/0001_jobs.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_job_outbox.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, filename, media_type, sha256, input) \
        VALUES ($1, 'legacy.pdf', 'application/pdf', 'test-hash', '\\x010203'::bytea)",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0003_extraction_profile.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().extraction_profile,
        "none"
    );
    assert_eq!(
        db.store
            .claim(60, 3)
            .await
            .unwrap()
            .unwrap()
            .job
            .extraction_profile,
        "none"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn bounded_submission_serializes_admission_and_allows_replays_when_full() {
    let db = TestDatabase::new().await;
    let original = db
        .store
        .submit_bounded(document(Some("original")), 4)
        .await
        .unwrap();
    let mut submissions = JoinSet::new();
    for index in 0..15 {
        let store = db.store.clone();
        submissions.spawn(async move {
            let key = format!("request-{index}");
            store.submit_bounded(document(Some(&key)), 4).await
        });
    }
    let mut admitted = 0;
    let mut rejected = 0;
    while let Some(result) = submissions.join_next().await {
        match result.unwrap() {
            Ok(outcome) => {
                assert!(outcome.created);
                admitted += 1;
            }
            Err(StoreError::QueueFull) => rejected += 1,
            Err(error) => panic!("unexpected submission error: {error}"),
        }
    }
    assert_eq!(admitted, 3);
    assert_eq!(rejected, 12);
    let replay = db
        .store
        .submit_bounded(document(Some("original")), 4)
        .await
        .unwrap();
    assert_eq!(replay.job.id, original.job.id);
    assert!(!replay.created);
    let mut conflict = document(Some("original"));
    conflict.sha256 = "b".repeat(64);
    assert!(matches!(
        db.store.submit_bounded(conflict, 4).await,
        Err(StoreError::IdempotencyConflict)
    ));
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status IN ('queued', 'processing')")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pending, 4);
    let leased = db.store.claim(60, 3).await.unwrap().unwrap();
    db.store
        .complete(leased.job.id, leased.lease_token, json!({"text": "done"}))
        .await
        .unwrap();
    assert!(
        db.store
            .submit_bounded(document(None), 4)
            .await
            .unwrap()
            .created
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn simultaneous_consumers_claim_distinct_jobs() {
    let db = TestDatabase::new().await;
    for _ in 0..8 {
        db.store.submit(document(None)).await.unwrap();
    }
    let mut claims = JoinSet::new();
    for _ in 0..16 {
        let store = db.store.clone();
        claims.spawn(async move { store.claim(60, 3).await.unwrap() });
    }
    let mut ids = HashSet::new();
    let mut tokens = HashSet::new();
    while let Some(claim) = claims.join_next().await {
        if let Some(claim) = claim.unwrap() {
            assert!(ids.insert(claim.job.id), "one live job was claimed twice");
            assert!(tokens.insert(claim.lease_token));
            assert_eq!(claim.job.status, "processing");
            assert_eq!(claim.job.attempts, 1);
        }
    }
    assert_eq!(ids.len(), 8);
    assert!(db.store.claim(60, 3).await.unwrap().is_none());
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn expired_leases_are_fenced_before_and_after_reclaim() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let first = db.store.claim(60, 3).await.unwrap().unwrap();
    db.expire(id).await;

    assert!(!db.store.renew(id, first.lease_token, 60).await.unwrap());
    assert!(
        !db.store
            .complete(id, first.lease_token, json!({"old": true}))
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .fail(id, first.lease_token, "old error", true, 3, 0)
            .await
            .unwrap()
    );

    let second = db.store.claim(60, 3).await.unwrap().unwrap();
    assert_eq!(second.job.id, id);
    assert_eq!(second.job.attempts, 2);
    assert_ne!(second.lease_token, first.lease_token);
    assert_eq!(second.input, [1, 2, 3, 4]);
    assert!(!db.store.renew(id, first.lease_token, 60).await.unwrap());
    assert!(
        !db.store
            .complete(id, first.lease_token, json!({"old": true}))
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .fail(id, first.lease_token, "old error", false, 3, 0)
            .await
            .unwrap()
    );
    assert!(db.store.renew(id, second.lease_token, 120).await.unwrap());
    let seconds_remaining: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM lease_expires_at - clock_timestamp())::double precision FROM jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(seconds_remaining > 100.0);

    let result = json!({"pages": [{"text": "invoice"}]});
    assert!(
        db.store
            .complete(id, second.lease_token, result.clone())
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .complete(id, second.lease_token, json!({"duplicate": true}))
            .await
            .unwrap()
    );
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "succeeded");
    assert_eq!(job.result, Some(result));
    let input_length: i32 =
        sqlx::query_scalar("SELECT octet_length(input) FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(input_length, 0);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn retries_wait_until_due_and_stop_at_the_attempt_budget() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let first = db.store.claim(60, 2).await.unwrap().unwrap();
    assert!(
        db.store
            .fail(
                id,
                first.lease_token,
                "temporarily unavailable",
                true,
                2,
                60
            )
            .await
            .unwrap()
    );
    assert_eq!(db.store.get(id).await.unwrap().unwrap().status, "queued");
    assert!(db.store.claim(60, 2).await.unwrap().is_none());
    sqlx::query(
        "UPDATE jobs SET available_at = clock_timestamp() - INTERVAL '1 second' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let second = db.store.claim(60, 2).await.unwrap().unwrap();
    assert_eq!(second.job.attempts, 2);
    assert!(
        db.store
            .fail(id, second.lease_token, "still unavailable", true, 2, 60)
            .await
            .unwrap()
    );
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "failed");
    assert_eq!(job.error.as_deref(), Some("still unavailable"));
    assert!(db.store.claim(60, 2).await.unwrap().is_none());
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn a_crashed_final_attempt_becomes_a_terminal_failure() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let claimed = db.store.claim(60, 1).await.unwrap().unwrap();
    db.expire(id).await;
    assert!(db.store.claim(60, 1).await.unwrap().is_none());
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "failed");
    assert_eq!(job.attempts, 1);
    assert_eq!(
        job.error.as_deref(),
        Some("worker lease expired after maximum attempts")
    );
    assert!(
        !db.store
            .complete(id, claimed.lease_token, json!({"late": true}))
            .await
            .unwrap()
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn claim_orders_due_work_by_available_time() {
    let db = TestDatabase::new().await;
    let first_id = db.store.submit(document(None)).await.unwrap().job.id;
    let second_id = db.store.submit(document(None)).await.unwrap().job.id;
    sqlx::query(
        "UPDATE jobs SET available_at = clock_timestamp() - INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(second_id)
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        db.store.claim(60, 3).await.unwrap().unwrap().job.id,
        second_id
    );
    assert_eq!(
        db.store.claim(60, 3).await.unwrap().unwrap().job.id,
        first_id
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn disabled_retention_preserves_old_terminal_jobs_and_their_keys() {
    let db = TestDatabase::new().await;
    let succeeded = db
        .store
        .submit(document(Some("pruned-key")))
        .await
        .unwrap()
        .job
        .id;
    let lease = db.store.claim(60, 3).await.unwrap().unwrap();
    assert!(
        db.store
            .complete(succeeded, lease.lease_token, json!({"text": "done"}))
            .await
            .unwrap()
    );
    let failed = db.store.submit(document(None)).await.unwrap().job.id;
    let lease = db.store.claim(60, 3).await.unwrap().unwrap();
    assert!(
        db.store
            .fail(failed, lease.lease_token, "invalid document", false, 3, 0)
            .await
            .unwrap()
    );
    let active = db.store.submit(document(None)).await.unwrap().job.id;
    db.store.claim(60, 3).await.unwrap().unwrap();
    let queued = db.store.submit(document(None)).await.unwrap().job.id;
    sqlx::query("UPDATE jobs SET updated_at = clock_timestamp() - INTERVAL '90 days'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.store.prune(30).await.is_err());
    assert!(db.store.get(succeeded).await.unwrap().is_some());
    assert!(db.store.get(failed).await.unwrap().is_some());
    assert_eq!(
        db.store.get(active).await.unwrap().unwrap().status,
        "processing"
    );
    assert_eq!(
        db.store.get(queued).await.unwrap().unwrap().status,
        "queued"
    );
    let replacement = db.store.submit(document(Some("pruned-key"))).await.unwrap();
    assert!(!replacement.created);
    assert_eq!(replacement.job.id, succeeded);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn legacy_prune_skips_locked_rows_then_fails_closed() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let lease = db.store.claim(60, 3).await.unwrap().unwrap();
    db.store
        .complete(id, lease.lease_token, json!({"text": "done"}))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE jobs SET updated_at = clock_timestamp() - INTERVAL '90 days' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut reader = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id = $1 FOR SHARE")
        .bind(id)
        .execute(&mut *reader)
        .await
        .unwrap();
    assert_eq!(db.store.prune(30).await.unwrap(), 0);
    reader.commit().await.unwrap();
    assert!(db.store.prune(30).await.is_err());
    assert!(db.store.get(id).await.unwrap().is_some());
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn outbox_is_atomic_with_submission_and_idempotent_replay() {
    let db = TestDatabase::new().await;
    let first = db.store.submit(document(Some("outbox-key"))).await.unwrap();
    let replay = db.store.submit(document(Some("outbox-key"))).await.unwrap();
    assert_eq!(first.job.id, replay.job.id);
    assert!(!replay.created);
    assert_eq!(db.outbox_count().await, 1);

    // The trigger participates in the exact transaction that inserts the input.
    let rolled_back_id = Uuid::new_v4();
    let mut transaction = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, filename, media_type, sha256, input) \
         VALUES ($1, 'rollback.png', 'image/png', 'fingerprint', '\\x010203'::bytea)",
    )
    .bind(rolled_back_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_outbox WHERE job_id = $1)")
            .bind(rolled_back_id)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert!(exists);
    transaction.rollback().await.unwrap();
    assert!(db.store.get(rolled_back_id).await.unwrap().is_none());
    assert_eq!(db.outbox_count().await, 1);
    let deliveries = db.store.claim_outbox(10, 30).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].job_id, first.job.id);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn outbox_migration_backfills_queued_and_processing_work_only() {
    let db = TestDatabase::unmigrated().await;
    sqlx::raw_sql(include_str!("../migrations/0001_jobs.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    // This test isolates the outbox backfill, while current Store submissions
    // also require the independently tested extraction metadata column.
    sqlx::raw_sql(include_str!("../migrations/0003_extraction_profile.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    let queued_id = db.store.submit(document(None)).await.unwrap().job.id;
    let processing_id = db.store.submit(document(None)).await.unwrap().job.id;
    let terminal_id = db.store.submit(document(None)).await.unwrap().job.id;
    sqlx::query(
        "UPDATE jobs SET status = 'processing', lease_token = $2, \
         lease_expires_at = clock_timestamp() + INTERVAL '1 minute' WHERE id = $1",
    )
    .bind(processing_id)
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET status = 'failed' WHERE id = $1")
        .bind(terminal_id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_job_outbox.sql"))
        .execute(&db.pool)
        .await
        .unwrap();
    let ids: HashSet<Uuid> = sqlx::query_scalar("SELECT job_id FROM job_outbox")
        .fetch_all(&db.pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(ids, HashSet::from([queued_id, processing_id]));
    let schedule_matches: bool = sqlx::query_scalar(
        "SELECT bool_and(outbox.next_dispatch_at = CASE WHEN jobs.status = 'processing' \
            THEN jobs.lease_expires_at ELSE jobs.available_at END) \
         FROM job_outbox AS outbox JOIN jobs ON jobs.id = outbox.job_id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(schedule_matches);
    let deliveries = db.store.claim_outbox(10, 30).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].job_id, queued_id);
    // New submissions also receive the trigger after the backfill.
    db.store.submit(document(None)).await.unwrap();
    assert_eq!(db.outbox_count().await, 3);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn dispatchers_claim_bounded_distinct_batches_and_skip_locked_rows() {
    let db = TestDatabase::new().await;
    let locked_id = db.store.submit(document(None)).await.unwrap().job.id;
    for _ in 0..6 {
        db.store.submit(document(None)).await.unwrap();
    }
    let mut transaction = db.pool.begin().await.unwrap();
    sqlx::query("SELECT job_id FROM job_outbox WHERE job_id = $1 FOR UPDATE")
        .bind(locked_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    let mut publishers = JoinSet::new();
    for _ in 0..8 {
        let store = db.store.clone();
        publishers.spawn(async move { store.claim_outbox(2, 30).await.unwrap() });
    }
    let mut dispatched = HashSet::new();
    while let Some(deliveries) = publishers.join_next().await {
        let deliveries = deliveries.unwrap();
        assert!(deliveries.len() <= 2);
        for delivery in deliveries {
            assert_ne!(delivery.job_id, locked_id);
            assert!(dispatched.insert(delivery.job_id));
        }
    }
    assert_eq!(dispatched.len(), 6);
    transaction.commit().await.unwrap();
    let deliveries = db.store.claim_outbox(2, 30).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].job_id, locked_id);
    assert!(db.store.claim_outbox(2, 30).await.unwrap().is_empty());
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn durable_dispatch_replays_after_restart_and_failed_publish_is_fenced() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let first = db.store.claim_outbox(1, 30).await.unwrap().remove(0);
    assert!(db.store.mark_dispatched(id, first.token, 30).await.unwrap());
    assert_eq!(db.outbox_count().await, 1);
    let restarted = Store::from_pool(db.pool.clone());
    assert!(restarted.claim_outbox(10, 30).await.unwrap().is_empty());
    let recovery_delay: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_dispatch_at - clock_timestamp())::double precision \
         FROM job_outbox WHERE job_id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(recovery_delay > 20.0);

    // A Redis data loss needs no PostgreSQL mutation: the retained record becomes
    // due on its own. Move the database clock deadline forward for this test.
    db.make_dispatch_due(id).await;
    let lost_publish = restarted.claim_outbox(1, 30).await.unwrap().remove(0);
    assert_ne!(lost_publish.token, first.token);
    assert!(restarted.claim_outbox(1, 30).await.unwrap().is_empty());
    db.make_dispatch_due(id).await;
    // Even before a newer publisher claims it, an expired token cannot defer it.
    assert!(
        !db.store
            .mark_dispatched(id, lost_publish.token, 300)
            .await
            .unwrap()
    );
    let current = restarted.claim_outbox(1, 30).await.unwrap().remove(0);
    assert_ne!(current.token, lost_publish.token);
    assert!(
        !db.store
            .mark_dispatched(id, lost_publish.token, 300)
            .await
            .unwrap()
    );
    assert!(
        db.store
            .mark_dispatched(id, current.token, 30)
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .mark_dispatched(id, current.token, 30)
            .await
            .unwrap()
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn retries_reset_publication_tokens_and_respect_the_available_time() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let old_delivery = db.store.claim_outbox(1, 30).await.unwrap().remove(0);
    let first = db.store.claim_by_id(id, 60, 3).await.unwrap().unwrap();
    assert_eq!(db.outbox_count().await, 1);
    assert!(db.store.claim_outbox(1, 30).await.unwrap().is_empty());
    assert!(
        db.store
            .fail(id, first.lease_token, "retry", true, 3, 60)
            .await
            .unwrap()
    );
    let reset: bool = sqlx::query_scalar(
        "SELECT outbox.next_dispatch_at = jobs.available_at AND outbox.dispatch_token IS NULL \
            AND outbox.dispatch_lease_expires_at IS NULL \
         FROM job_outbox AS outbox JOIN jobs ON jobs.id = outbox.job_id WHERE jobs.id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(reset);
    assert!(
        !db.store
            .mark_dispatched(id, old_delivery.token, 300)
            .await
            .unwrap()
    );
    assert!(db.store.claim_outbox(1, 30).await.unwrap().is_empty());
    assert!(db.store.claim_by_id(id, 60, 3).await.unwrap().is_none());
    sqlx::query(
        "UPDATE jobs SET available_at = clock_timestamp() - INTERVAL '1 second' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let delivery = db.store.claim_outbox(1, 30).await.unwrap().remove(0);
    assert_eq!(delivery.job_id, id);
    let second = db.store.claim_by_id(id, 60, 3).await.unwrap().unwrap();
    assert_eq!(second.job.attempts, 2);
    assert!(
        db.store
            .fail(id, second.lease_token, "permanent", false, 3, 0)
            .await
            .unwrap()
    );
    assert_eq!(db.outbox_count().await, 0);
    assert!(
        !db.store
            .mark_dispatched(id, delivery.token, 30)
            .await
            .unwrap()
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn claim_by_id_fences_duplicates_and_expired_workers_and_retains_source() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let unrelated = db.store.submit(document(None)).await.unwrap().job.id;
    let mut consumers = JoinSet::new();
    for _ in 0..16 {
        let store = db.store.clone();
        consumers.spawn(async move { store.claim_by_id(id, 60, 3).await.unwrap() });
    }
    let mut claimed = Vec::new();
    while let Some(result) = consumers.join_next().await {
        if let Some(job) = result.unwrap() {
            claimed.push(job);
        }
    }
    assert_eq!(claimed.len(), 1);
    let first = claimed.remove(0);
    assert_eq!(first.job.attempts, 1);
    assert_eq!(db.store.get(unrelated).await.unwrap().unwrap().attempts, 0);
    db.expire(id).await;
    assert!(!db.store.renew(id, first.lease_token, 60).await.unwrap());
    assert!(
        !db.store
            .complete(id, first.lease_token, json!({"stale": true}))
            .await
            .unwrap()
    );
    let current = db.store.claim_by_id(id, 60, 3).await.unwrap().unwrap();
    assert_eq!(current.job.attempts, 2);
    assert_ne!(first.lease_token, current.lease_token);
    assert!(
        !db.store
            .fail(id, first.lease_token, "stale", false, 3, 0)
            .await
            .unwrap()
    );
    assert!(
        db.store
            .complete_retaining_input(id, current.lease_token, json!({"text": "complete"}))
            .await
            .unwrap()
    );
    assert!(db.store.claim_by_id(id, 60, 3).await.unwrap().is_none());
    let input: Vec<u8> = sqlx::query_scalar("SELECT input FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(input, [1, 2, 3, 4]);
    assert_eq!(db.outbox_count().await, 1); // Only the unrelated queued job remains.
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn expired_final_notification_terminalizes_job_and_stops_outbox_replay() {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let final_attempt = db.store.claim_by_id(id, 60, 1).await.unwrap().unwrap();
    assert!(db.store.claim_outbox(10, 30).await.unwrap().is_empty());
    db.expire(id).await;
    let delivery = db.store.claim_outbox(10, 30).await.unwrap().remove(0);
    assert_eq!(delivery.job_id, id);
    assert!(db.store.claim_by_id(id, 60, 1).await.unwrap().is_none());
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "failed");
    assert_eq!(job.attempts, 1);
    assert_eq!(
        job.error.as_deref(),
        Some("worker lease expired after maximum attempts")
    );
    assert_eq!(db.outbox_count().await, 0);
    assert!(
        !db.store
            .mark_dispatched(id, delivery.token, 30)
            .await
            .unwrap()
    );
    assert!(db.store.claim_by_id(id, 60, 1).await.unwrap().is_none());
    assert!(db.store.claim_outbox(10, 30).await.unwrap().is_empty());
    assert!(
        !db.store
            .complete(id, final_attempt.lease_token, json!({"late": true}))
            .await
            .unwrap()
    );
    db.cleanup().await;
}

#[derive(Clone, Copy)]
enum ExpiringWrite {
    Renew,
    Complete,
    CompleteRetainingInput,
    Fail,
}

type FencedJobSnapshot = (
    String,
    Option<serde_json::Value>,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    Vec<u8>,
);

/// The blocker only locks: it never changes the tuple. This specifically catches
/// UPDATE predicates evaluated before the lock wait without EvalPlanQual refresh.
async fn unchanged_locked_row_rechecks_expiry(write: ExpiringWrite) {
    let db = TestDatabase::new().await;
    let id = db.store.submit(document(None)).await.unwrap().job.id;
    let leased = db.store.claim_by_id(id, 60, 3).await.unwrap().unwrap();
    sqlx::query("UPDATE jobs SET lease_expires_at=clock_timestamp()+INTERVAL '900 milliseconds' WHERE id=$1")
        .bind(id).execute(&db.pool).await.unwrap();
    let before: FencedJobSnapshot = sqlx::query_as(
        "SELECT status,result,error,lease_expires_at,updated_at,input FROM jobs WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let store = db.store.clone();
    let token = leased.lease_token;
    let pending = tokio::spawn(async move {
        match write {
            ExpiringWrite::Renew => store.renew(id, token, 60).await,
            ExpiringWrite::Complete => store.complete(id, token, json!({"late":true})).await,
            ExpiringWrite::CompleteRetainingInput => {
                store
                    .complete_retaining_input(id, token, json!({"late":true}))
                    .await
            }
            ExpiringWrite::Fail => store.fail(id, token, "late failure", true, 3, 0).await,
        }
    });
    tokio::time::timeout(std::time::Duration::from_millis(700), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(blocker_pid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fenced writer must wait on the unchanged row while its lease is still live");
    let live: bool =
        sqlx::query_scalar("SELECT lease_expires_at>clock_timestamp() FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        live,
        "writer must have reached the unchanged lock before expiration"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let expired: bool = sqlx::query_scalar(
                "SELECT lease_expires_at<=clock_timestamp() FROM jobs WHERE id=$1",
            )
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if expired {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    blocker.rollback().await.unwrap();
    assert!(
        !pending.await.unwrap().unwrap(),
        "a lease that expired during an unchanged row-lock wait must never authorize a mutation"
    );
    let after: FencedJobSnapshot = sqlx::query_as(
        "SELECT status,result,error,lease_expires_at,updated_at,input FROM jobs WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        before, after,
        "rejected late write must leave all original state and source bytes unchanged"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn renew_rechecks_wall_clock_after_unchanged_row_lock_wait() {
    unchanged_locked_row_rechecks_expiry(ExpiringWrite::Renew).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn complete_rechecks_wall_clock_after_unchanged_row_lock_wait() {
    unchanged_locked_row_rechecks_expiry(ExpiringWrite::Complete).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn complete_retaining_input_rechecks_wall_clock_after_unchanged_row_lock_wait() {
    unchanged_locked_row_rechecks_expiry(ExpiringWrite::CompleteRetainingInput).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn fail_rechecks_wall_clock_after_unchanged_row_lock_wait() {
    unchanged_locked_row_rechecks_expiry(ExpiringWrite::Fail).await;
}
