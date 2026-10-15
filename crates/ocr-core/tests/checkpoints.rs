//! Real PostgreSQL retry-cache invariants. Run with TEST_DATABASE_URL and --ignored.
use std::str::FromStr;

use ocr_core::{checkpoint::CheckpointWrite, domain::NewJob, store::Store};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::task::JoinSet;
use uuid::Uuid;

struct Database {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
}
impl Database {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("checkpoint_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await
            .unwrap();
        let store = Store::from_pool(pool.clone());
        store.migrate().await.unwrap();
        Self {
            admin,
            pool,
            store,
            schema,
        }
    }
    async fn leased(&self) -> ocr_core::domain::LeasedJob {
        let id = Uuid::new_v4();
        self.store
            .submit(NewJob {
                id,
                filename: "fixture.png".into(),
                media_type: "image/png".into(),
                sha256: "a".repeat(64),
                input: vec![1, 2, 3],
                idempotency_key: None,
                extraction_profile: "none".into(),
            })
            .await
            .unwrap();
        self.store.claim_by_id(id, 60, 3).await.unwrap().unwrap()
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
fn key(index: u64) -> String {
    format!("{index:064x}")
}
fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn lease_is_rechecked_after_waiting_for_an_unchanged_row_lock() {
    let db = Database::new().await;
    let lease = db.leased().await;
    let id = lease.job.id;
    sqlx::query(
        "UPDATE jobs SET lease_expires_at=clock_timestamp()+INTERVAL '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let store = db.store.clone();
    let pending = tokio::spawn(async move {
        store
            .save_region_checkpoint(id, lease.lease_token, &key(1), "late")
            .await
            .unwrap()
    });
    // Confirm a waiter exists before letting the original valid lease expire.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() \
                 AND wait_event_type='Lock' AND query LIKE '%FROM jobs WHERE id=$1%')",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("SELECT pg_sleep(1.1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(pending.await.unwrap(), CheckpointWrite::LeaseLost);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM region_checkpoints WHERE job_id=$1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn expired_and_stale_leases_cannot_read_or_write_but_reclaimed_owner_can_resume() {
    let db = Database::new().await;
    let old = db.leased().await;
    let id = old.job.id;
    assert_eq!(
        db.store
            .save_region_checkpoint(id, old.lease_token, &key(1), "recognized")
            .await
            .unwrap(),
        CheckpointWrite::Stored
    );
    sqlx::query(
        "UPDATE jobs SET lease_expires_at=clock_timestamp()-INTERVAL '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(
        db.store
            .region_checkpoint(id, old.lease_token, &key(1))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.store
            .save_region_checkpoint(id, old.lease_token, &key(2), "stale")
            .await
            .unwrap(),
        CheckpointWrite::LeaseLost
    );
    let current = db.store.claim_by_id(id, 60, 3).await.unwrap().unwrap();
    assert_ne!(old.lease_token, current.lease_token);
    assert_eq!(current.job.attempts, 2);
    assert_eq!(
        db.store
            .region_checkpoint(id, current.lease_token, &key(1))
            .await
            .unwrap()
            .as_deref(),
        Some("recognized")
    );
    assert!(
        db.store
            .region_checkpoint(id, old.lease_token, &key(1))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.store
            .save_region_checkpoint(id, old.lease_token, &key(1), "overwrite")
            .await
            .unwrap(),
        CheckpointWrite::LeaseLost
    );
    assert!(
        db.store
            .complete(id, current.lease_token, serde_json::json!({"ok":true}))
            .await
            .unwrap()
    );
    assert!(
        db.store
            .region_checkpoint(id, current.lease_token, &key(1))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.store
            .save_region_checkpoint(id, current.lease_token, &key(3), "late")
            .await
            .unwrap(),
        CheckpointWrite::LeaseLost
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn checkpoint_values_are_immutable_job_scoped_and_hash_checked() {
    let db = Database::new().await;
    let a = db.leased().await;
    let b = db.leased().await;
    for (job, text) in [(&a, "first job"), (&b, "second job")] {
        assert_eq!(
            db.store
                .save_region_checkpoint(job.job.id, job.lease_token, &key(1), text)
                .await
                .unwrap(),
            CheckpointWrite::Stored
        );
    }
    assert_eq!(
        db.store
            .save_region_checkpoint(a.job.id, a.lease_token, &key(1), "replacement")
            .await
            .unwrap(),
        CheckpointWrite::Stored
    );
    assert_eq!(
        db.store
            .region_checkpoint(a.job.id, a.lease_token, &key(1))
            .await
            .unwrap()
            .as_deref(),
        Some("first job")
    );
    assert_eq!(
        db.store
            .region_checkpoint(b.job.id, b.lease_token, &key(1))
            .await
            .unwrap()
            .as_deref(),
        Some("second job")
    );
    assert!(
        db.store
            .region_checkpoint(a.job.id, b.lease_token, &key(1))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE region_checkpoints SET content='corrupted' WHERE job_id=$1")
        .bind(a.job.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        db.store
            .region_checkpoint(a.job.id, a.lease_token, &key(1))
            .await
            .unwrap()
            .is_none()
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn invalid_keys_and_entry_byte_budget_are_rejected_without_persisting() {
    let db = Database::new().await;
    let job = db.leased().await;
    for invalid in ["".into(), "A".repeat(64), "x".repeat(64), "1".repeat(65)] {
        assert_eq!(
            db.store
                .save_region_checkpoint(job.job.id, job.lease_token, &invalid, "x")
                .await
                .unwrap(),
            CheckpointWrite::BudgetExceeded
        );
        assert!(
            db.store
                .region_checkpoint(job.job.id, job.lease_token, &invalid)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        db.store
            .save_region_checkpoint(job.job.id, job.lease_token, &key(1), &"đ".repeat(65_537))
            .await
            .unwrap(),
        CheckpointWrite::BudgetExceeded
    );
    assert_eq!(
        db.store
            .save_region_checkpoint(job.job.id, job.lease_token, &key(2), &"đ".repeat(65_536))
            .await
            .unwrap(),
        CheckpointWrite::Stored
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM region_checkpoints")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn concurrent_writes_cannot_exceed_the_aggregate_byte_budget() {
    let db = Database::new().await;
    let job = db.leased().await;
    let text = "x".repeat(128 * 1024);
    sqlx::query("INSERT INTO region_checkpoints(job_id,cache_key,content,content_sha256) SELECT $1,lpad(to_hex(i),64,'0'),$2,$3 FROM generate_series(1,127) AS i")
        .bind(job.job.id).bind(&text).bind(digest(&text)).execute(&db.pool).await.unwrap();
    let mut tasks = JoinSet::new();
    for i in 128..136 {
        let store = db.store.clone();
        let id = job.job.id;
        let token = job.lease_token;
        tasks.spawn(async move {
            store
                .save_region_checkpoint(id, token, &key(i), &"y".repeat(64 * 1024))
                .await
                .unwrap()
        });
    }
    let mut stored = 0;
    let mut rejected = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            CheckpointWrite::Stored => stored += 1,
            CheckpointWrite::BudgetExceeded => rejected += 1,
            CheckpointWrite::LeaseLost => panic!("test lease unexpectedly lost"),
        }
    }
    assert_eq!((stored, rejected), (2, 6));
    let bytes: i64 = sqlx::query_scalar(
        "SELECT sum(octet_length(content))::bigint FROM region_checkpoints WHERE job_id=$1",
    )
    .bind(job.job.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(bytes, 16 * 1024 * 1024);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn entry_count_budget_and_disabled_retention_preserve_checkpoints() {
    let db = Database::new().await;
    let job = db.leased().await;
    sqlx::query("INSERT INTO region_checkpoints(job_id,cache_key,content,content_sha256) SELECT $1,lpad(to_hex(i),64,'0'),'x',$2 FROM generate_series(1,4096) AS i")
        .bind(job.job.id).bind(digest("x")).execute(&db.pool).await.unwrap();
    assert_eq!(
        db.store
            .save_region_checkpoint(job.job.id, job.lease_token, &key(4097), "x")
            .await
            .unwrap(),
        CheckpointWrite::BudgetExceeded
    );
    assert!(
        db.store
            .complete(job.job.id, job.lease_token, serde_json::json!({"ok":true}))
            .await
            .unwrap()
    );
    sqlx::query("UPDATE jobs SET updated_at=clock_timestamp()-INTERVAL '8 days' WHERE id=$1")
        .bind(job.job.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.store.prune(7).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM region_checkpoints")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 4096);
    db.cleanup().await;
}
