//! Real PostgreSQL/Redis worker tests. Set TEST_DATABASE_URL and TEST_REDIS_URL,
//! then run cargo test -p ocr-worker --test worker_lifecycle -- --ignored.
//! Every test owns an isolated PostgreSQL schema and Redis stream/group.
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, routing::post};
use ocr_core::{domain::NewJob, store::Store};
use ocr_worker::{queue::RedisQueue, settings::Settings, worker};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Database {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
    redis_url: String,
    redis_stream: String,
    redis_group: String,
}
impl Database {
    async fn new() -> Self {
        let redis_url =
            std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL is required for ignored tests");
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for ignored tests");
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("worker_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let scope = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let query = format!("SET search_path TO {scope}");
                Box::pin(async move {
                    sqlx::query(&query).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        let store = Store::from_pool(pool.clone());
        store.migrate().await.unwrap();
        Self {
            admin,
            pool,
            store,
            redis_stream: format!("ocr:test:{}", Uuid::new_v4()),
            redis_group: format!("test-{}", Uuid::new_v4()),
            redis_url,
            schema,
        }
    }
    async fn redis(&self) -> redis::aio::MultiplexedConnection {
        redis::Client::open(self.redis_url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap()
    }
    async fn pending(&self) -> i64 {
        let mut connection = self.redis().await;
        let reply: redis::streams::StreamPendingReply = redis::cmd("XPENDING")
            .arg(&self.redis_stream)
            .arg(&self.redis_group)
            .query_async(&mut connection)
            .await
            .unwrap();
        reply.count() as i64
    }
    async fn stream_len(&self) -> i64 {
        redis::cmd("XLEN")
            .arg(&self.redis_stream)
            .query_async(&mut self.redis().await)
            .await
            .unwrap()
    }
    async fn cleanup(self) {
        let _: i64 = redis::cmd("DEL")
            .arg(&self.redis_stream)
            .query_async(&mut self.redis().await)
            .await
            .unwrap();
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
    async fn submit(&self) -> Uuid {
        self.submit_with_profile("none").await
    }
    async fn submit_with_profile(&self, profile: &str) -> Uuid {
        let mut encoded = Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8, 8, image::Rgb([255; 3]))
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let id = Uuid::new_v4();
        self.store
            .submit(NewJob {
                id,
                filename: "lease.png".into(),
                media_type: "image/png".into(),
                extraction_profile: profile.into(),
                sha256: "a".repeat(64),
                input: encoded.into_inner(),
                idempotency_key: None,
            })
            .await
            .unwrap();
        id
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn profile_runs_in_worker_persists_review_output_and_retains_original() {
    let db = Database::new().await;
    let server = SlowInference::new(0).await;
    let id = db.submit_with_profile("vi_rental_contract_v1").await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        shutdown.clone(),
    ));
    wait_terminal(&db, id).await;
    wait_empty(&db).await;
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "succeeded");
    assert_eq!(job.extraction_profile, "vi_rental_contract_v1");
    let result = job.result.unwrap();
    assert_eq!(result["extraction"]["decision"], "needs_review");
    assert_eq!(result["extraction"]["supported_template"], false);
    assert_eq!(result["source"]["original_retained"], true);
    let retained: Option<Vec<u8>> = sqlx::query_scalar("SELECT input FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!retained.unwrap().is_empty());
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    db.cleanup().await;
}

struct SlowInference {
    base: String,
    started: Arc<Notify>,
    handle: JoinHandle<()>,
    requests: Arc<AtomicUsize>,
}
impl Drop for SlowInference {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
impl SlowInference {
    async fn new(delay: u64) -> Self {
        let started = Arc::new(Notify::new());
        let notification = started.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_counter = requests.clone();
        let app = Router::new().route("/v1/chat/completions", post(move || {
            let notification = notification.clone();
            let request_counter = request_counter.clone();
            async move {
                request_counter.fetch_add(1, Ordering::SeqCst);
                notification.notify_one();
                tokio::time::sleep(Duration::from_secs(delay)).await;
                Json(json!({"choices":[{"message":{"content":"lease test"},"finish_reason":"stop"}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            started,
            handle,
            requests,
        }
    }
    async fn wait_started(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.started.notified())
            .await
            .unwrap();
    }
}

fn settings(base: &str, db: &Database) -> Settings {
    Settings {
        database_url: "postgres://unused".into(),
        redis_url: db.redis_url.clone(),
        redis_stream: db.redis_stream.clone(),
        redis_consumer_group: db.redis_group.clone(),
        redis_timeout_ms: 200,
        redis_redispatch_seconds: 1,
        vllm_base_url: base.into(),
        vllm_model: "test".into(),
        vllm_api_key: None,
        worker_concurrency: 1,
        lease_seconds: 3,
        max_attempts: 1,
        poll_interval_ms: 10,
        request_timeout_seconds: 30,
        job_timeout_seconds: 20,
        max_pages: 20,
        max_image_pixels: 40_000_000,
        max_image_edge: 2048,
        table_crop_padding_pixels: 0,
        pdf_render_dpi: 200,
        page_max_edge: 3500,
        strategy: ocr_worker::settings::OcrStrategy::FullPage,
        layout_binary: "unused".into(),
        layout_model_path: "unused".into(),
        layout_model_sha256: String::new(),
        layout_timeout_seconds: 60,
        layout_pool_size: 1,
        max_regions_per_page: 128,
        max_regions_per_document: 2048,
        region_concurrency: 4,
        inference_revision: String::new(),
        inference_deployment: None,
        checkpoint_enabled: false,
        recovery_max_regions: 0,
        recovery_expansion_pixels: 16,
        recognize_image_regions: false,
        pdf_render_timeout_seconds: 30,
        max_output_tokens: 8192,
        retention_days: 7,
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn heartbeat_keeps_slow_inference_leased_and_shutdown_drains_without_claiming_more() {
    let db = Database::new().await;
    let server = SlowInference::new(5).await;
    let first = db.submit().await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        shutdown.clone(),
    ));
    server.wait_started().await;
    tokio::time::sleep(Duration::from_millis(3300)).await;
    assert_eq!(
        db.pending().await,
        1,
        "must remain pending before SQL commit"
    );
    // The original three-second lease has passed. Another worker still cannot
    // claim it because the independent heartbeat renewed during inference.
    assert!(db.store.claim(3, 1).await.unwrap().is_none());
    assert_eq!(
        db.store.get(first).await.unwrap().unwrap().status,
        "processing"
    );
    shutdown.cancel();
    let second = db.submit().await;
    tokio::time::timeout(Duration::from_secs(4), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        db.store.get(first).await.unwrap().unwrap().status,
        "succeeded"
    );
    assert_eq!(
        db.store.get(second).await.unwrap().unwrap().status,
        "queued"
    );
    assert_eq!(db.pending().await, 0, "ack follows the durable commit");
    let consumers: redis::streams::StreamInfoConsumersReply = redis::cmd("XINFO")
        .arg("CONSUMERS")
        .arg(&db.redis_stream)
        .arg(&db.redis_group)
        .query_async(&mut db.redis().await)
        .await
        .unwrap();
    assert!(
        consumers.consumers.is_empty(),
        "graceful empty consumer is retired"
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn heartbeat_does_not_stop_polling_the_checkpoint_that_holds_its_job_row_lock() {
    let db = Database::new().await;
    let server = SlowInference::new(0).await;
    let id = db.submit().await;
    let lock_key = i64::from_le_bytes(Uuid::new_v4().as_bytes()[..8].try_into().unwrap());
    // The trigger blocks only this isolated schema. A real checkpoint first
    // locks jobs, then waits here while PostgreSQL executes its INSERT.
    sqlx::query(&format!(
        "CREATE FUNCTION wait_for_checkpoint_fixture() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN \
         PERFORM pg_advisory_xact_lock({lock_key}::bigint); RETURN NEW; END $$"
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER wait_for_checkpoint_fixture BEFORE INSERT ON region_checkpoints \
         FOR EACH ROW EXECUTE FUNCTION wait_for_checkpoint_fixture()",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let mut gate = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut *gate)
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    let mut config = settings(&server.base, &db);
    config.checkpoint_enabled = true;
    config.inference_revision = "heartbeat-checkpoint-fixture-v1".into();
    // Renewal starts after two seconds, before the checkpoint's three-second
    // deadline. Observing the actual lock graph avoids a scheduling sleep.
    config.lease_seconds = 6;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(db.store.clone(), config, shutdown.clone()));
    let checkpoint_pid =
        wait_for_blocked_query(&db, gate_pid, "INSERT INTO region_checkpoints%").await;
    // Renewal acquires its lock before checking the wall-clock lease in a
    // subsequent UPDATE, so the heartbeat blocks in this lock statement.
    wait_for_blocked_query(
        &db,
        checkpoint_pid,
        "SELECT id FROM jobs WHERE id=$1 FOR UPDATE",
    )
    .await;
    shutdown.cancel();
    // Releasing the server-side gate makes INSERT ready. The processing future
    // still must be polled to commit its transaction and unblock the renewal.
    gate.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), handle)
        .await
        .expect("checkpoint and heartbeat must make progress while draining")
        .unwrap()
        .unwrap();
    let job = db.store.get(id).await.unwrap().unwrap();
    let checkpoints: i64 =
        sqlx::query_scalar("SELECT count(*) FROM region_checkpoints WHERE job_id=$1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pending = db.pending().await;
    let requests = server.requests.load(Ordering::SeqCst);
    db.cleanup().await;
    assert_eq!(
        job.status, "succeeded",
        "renewal must not cancel its own checkpoint"
    );
    assert_eq!(job.attempts, 1);
    assert_eq!(
        checkpoints, 1,
        "the actual checkpoint transaction committed"
    );
    assert_eq!(
        pending, 0,
        "successful commit must acknowledge the notification"
    );
    assert_eq!(
        requests, 1,
        "lock contention must not cause repeated inference"
    );
}

async fn wait_for_blocked_query(db: &Database, blocker: i32, query_pattern: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) \
                 AND query LIKE $2 LIMIT 1",
            )
            .bind(blocker)
            .bind(query_pattern)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
            if let Some(pid) = blocked {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("expected database lock edge was not observed: {query_pattern}"))
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn lost_lease_cancels_local_inference_and_drains_without_waiting_for_the_server() {
    let db = Database::new().await;
    let server = SlowInference::new(20).await;
    let id = db.submit().await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        shutdown.clone(),
    ));
    server.wait_started().await;
    sqlx::query("UPDATE jobs SET lease_token = $2 WHERE id = $1")
        .bind(id)
        .bind(Uuid::new_v4())
        .execute(&db.pool)
        .await
        .unwrap();
    shutdown.cancel();
    // Draining still renews leases. The next heartbeat sees the changed token,
    // cancels its HTTP future, and returns long before the 20-second mock reply.
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "processing");
    assert!(job.result.is_none());
    assert_eq!(
        db.pending().await,
        1,
        "lost lease must remain pending after shutdown"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn total_job_timeout_records_failure_and_drops_pending_inference() {
    let db = Database::new().await;
    let server = SlowInference::new(20).await;
    let id = db.submit().await;
    let shutdown = CancellationToken::new();
    let mut config = settings(&server.base, &db);
    config.job_timeout_seconds = 1;
    let handle = tokio::spawn(worker::run(db.store.clone(), config, shutdown.clone()));
    server.wait_started().await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let job = db.store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status, "failed");
    assert!(job.error.unwrap().contains("deadline"));
    assert!(job.result.is_none());
    db.cleanup().await;
}

async fn wait_terminal(db: &Database, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if matches!(
                db.store.get(id).await.unwrap().unwrap().status.as_str(),
                "succeeded" | "failed"
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("job should reach a durable terminal state");
}

async fn wait_empty(db: &Database) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while db.stream_len().await != 0 || db.pending().await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("stream and pending entries should be cleaned");
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn outbox_recovers_dispatch_crashes_and_redis_stream_loss() {
    let db = Database::new().await;
    let server = SlowInference::new(0).await;
    let config = settings(&server.base, &db);
    let queue = RedisQueue::new(&config).unwrap();
    let unpublished = db.submit().await;
    let lease = db.store.claim_outbox(1, 30).await.unwrap().pop().unwrap();
    assert_eq!(lease.job_id, unpublished);
    // Simulate process death after claiming its outbox row, before XADD.
    sqlx::query("UPDATE job_outbox SET dispatch_lease_expires_at = now() - interval '1 second' WHERE job_id = $1")
        .bind(unpublished).execute(&db.pool).await.unwrap();
    let published = db.submit().await;
    let deliveries = db.store.claim_outbox(8, 30).await.unwrap();
    for delivery in deliveries {
        queue.publish(delivery.job_id).await.unwrap();
        assert!(
            db.store
                .mark_dispatched(delivery.job_id, delivery.token, 1)
                .await
                .unwrap()
        );
    }
    // Delete only this test's stream to simulate losing Redis persistence.
    let _: i64 = redis::cmd("DEL")
        .arg(&db.redis_stream)
        .query_async(&mut db.redis().await)
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(db.store.clone(), config, shutdown.clone()));
    wait_terminal(&db, unpublished).await;
    wait_terminal(&db, published).await;
    wait_empty(&db).await;
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    assert_eq!(
        db.store.get(unpublished).await.unwrap().unwrap().attempts,
        1
    );
    assert_eq!(db.store.get(published).await.unwrap().unwrap().attempts, 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn xadd_before_dispatch_commit_duplicate_and_terminal_redelivery_run_inference_once() {
    let db = Database::new().await;
    let server = SlowInference::new(1).await;
    let config = settings(&server.base, &db);
    let queue = RedisQueue::new(&config).unwrap();
    let id = db.submit().await;
    db.store.claim_outbox(1, 30).await.unwrap();
    queue.publish(id).await.unwrap();
    // The process published but died before recording dispatch. A future
    // dispatcher can publish the same ID again after this lease expires.
    sqlx::query("UPDATE job_outbox SET dispatch_lease_expires_at = now() - interval '1 second' WHERE job_id = $1")
        .bind(id).execute(&db.pool).await.unwrap();
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(db.store.clone(), config, shutdown.clone()));
    wait_terminal(&db, id).await;
    wait_empty(&db).await;
    // A delayed network retry arriving after completion is also harmless.
    queue.publish(id).await.unwrap();
    wait_empty(&db).await;
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert_eq!(db.store.get(id).await.unwrap().unwrap().attempts, 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn crashed_consumer_pending_entry_is_reclaimed_and_malformed_entries_are_deleted() {
    let db = Database::new().await;
    let server = SlowInference::new(0).await;
    let mut config = settings(&server.base, &db);
    config.max_attempts = 2;
    let crashed = RedisQueue::new(&config).unwrap();
    crashed.ensure_group().await.unwrap();
    let id = db.submit().await;
    crashed.publish(id).await.unwrap();
    assert_eq!(crashed.read(1).await.unwrap().len(), 1);
    db.store.claim_by_id(id, 3, 2).await.unwrap().unwrap();
    let _: String = redis::cmd("XADD")
        .arg(&db.redis_stream)
        .arg("*")
        .arg("job_id")
        .arg("invalid-private-payload")
        .query_async(&mut db.redis().await)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(3100)).await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(db.store.clone(), config, shutdown.clone()));
    wait_terminal(&db, id).await;
    wait_empty(&db).await;
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert_eq!(db.store.get(id).await.unwrap().unwrap().attempts, 2);
    db.cleanup().await;
}

/// Fault injection affects only sockets opened by this test. Real Redis remains
/// untouched, so running other test cases or local services stays safe.
struct RedisProxy {
    url: String,
    failed: CancellationToken,
    handle: JoinHandle<()>,
}
impl RedisProxy {
    async fn new(upstream_url: &str) -> Self {
        let mut url = reqwest::Url::parse(upstream_url).unwrap();
        let upstream_host = url.host_str().unwrap().to_owned();
        let upstream_port = url.port().unwrap_or(6379);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = listener.local_addr().unwrap().port();
        url.set_host(Some("127.0.0.1")).unwrap();
        url.set_port(Some(local_port)).unwrap();
        let failed = CancellationToken::new();
        let failure = failed.clone();
        let handle = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut downstream, _) = listener.accept().await.unwrap();
                let failure = failure.clone();
                let upstream_host = upstream_host.clone();
                connections.spawn(async move {
                    tokio::select! {
                        _ = failure.cancelled() => {
                            // Keep this socket open without a response, exercising
                            // command/connect timeouts rather than a quick reset.
                            std::future::pending::<()>().await;
                        },
                        _ = async {
                            let mut upstream = tokio::net::TcpStream::connect((upstream_host.as_str(), upstream_port)).await.unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                        } => {},
                    }
                });
                while connections.try_join_next().is_some() {}
            }
        });
        Self {
            url: url.to_string(),
            failed,
            handle,
        }
    }
}
impl Drop for RedisProxy {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn redis_failure_is_bounded_while_sql_heartbeats_and_durable_commit_continue() {
    let db = Database::new().await;
    let server = SlowInference::new(5).await;
    let proxy = RedisProxy::new(&db.redis_url).await;
    let mut config = settings(&server.base, &db);
    config.redis_url = proxy.url.clone();
    let id = db.submit().await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        config.clone(),
        shutdown.clone(),
    ));
    server.wait_started().await;
    proxy.failed.cancel();
    tokio::time::sleep(Duration::from_millis(3300)).await;
    assert!(db.store.claim_by_id(id, 3, 1).await.unwrap().is_none());
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().status,
        "processing"
    );
    let start = tokio::time::Instant::now();
    assert!(
        RedisQueue::new(&config)
            .unwrap()
            .ensure_group()
            .await
            .is_err()
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "Redis error must be bounded"
    );
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(4), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(db.store.get(id).await.unwrap().unwrap().status, "succeeded");
    assert_eq!(
        db.pending().await,
        1,
        "unacknowledged durable commit remains recoverable"
    );
    // Restart against live Redis. Cleanup of the committed pending delivery
    // must happen without sending a second inference request.
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        shutdown.clone(),
    ));
    wait_empty(&db).await;
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn dispatcher_publishes_queued_jobs_while_every_inference_slot_is_busy() {
    let db = Database::new().await;
    let server = SlowInference::new(3).await;
    let first = db.submit().await;
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        shutdown.clone(),
    ));
    server.wait_started().await;
    let second = db.submit().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let messages: redis::streams::StreamRangeReply = redis::cmd("XRANGE")
                .arg(&db.redis_stream)
                .arg("-")
                .arg("+")
                .query_async(&mut db.redis().await)
                .await
                .unwrap();
            if messages.ids.iter().any(|message| {
                message.get::<String>("job_id").as_deref() == Some(&second.to_string())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("dispatcher should publish while inference capacity is occupied");
    assert_eq!(
        db.store.get(second).await.unwrap().unwrap().status,
        "queued"
    );
    assert_eq!(
        db.store.get(first).await.unwrap().unwrap().status,
        "processing"
    );
    assert_eq!(
        db.pending().await,
        1,
        "busy worker must not prefetch a second job"
    );
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(db.store.get(second).await.unwrap().unwrap().attempts, 0);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a dedicated disposable TEST_REDIS_OOM_URL server in addition to database/Redis test URLs"]
async fn existing_group_can_read_reclaim_and_ack_when_redis_is_at_maxmemory() {
    let Ok(oom_url) = std::env::var("TEST_REDIS_OOM_URL") else {
        eprintln!("TEST_REDIS_OOM_URL unset: skipping dedicated Redis memory-limit regression");
        return;
    };
    let db = Database::new().await;
    let mut config = settings("http://localhost:8000/v1", &db);
    config.redis_url = oom_url.clone();
    let queue = RedisQueue::new(&config).unwrap();
    let mut admin = redis::Client::open(oom_url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let test_server: String = redis::cmd("INFO")
        .arg("server")
        .query_async(&mut admin)
        .await
        .unwrap();
    let live_server: String = redis::cmd("INFO")
        .arg("server")
        .query_async(&mut db.redis().await)
        .await
        .unwrap();
    let run_id = |info: &str| {
        info.lines()
            .find_map(|line| line.strip_prefix("run_id:"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(
        run_id(&test_server),
        run_id(&live_server),
        "OOM regression requires a separate disposable Redis instance"
    );
    let original: std::collections::HashMap<String, String> = redis::cmd("CONFIG")
        .arg("GET")
        .arg("maxmemory")
        .arg("maxmemory-policy")
        .query_async(&mut admin)
        .await
        .unwrap();
    queue.ensure_group().await.unwrap();
    queue.publish(Uuid::new_v4()).await.unwrap();
    queue.publish(Uuid::new_v4()).await.unwrap();
    assert_eq!(queue.read(1).await.unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    use futures_util::FutureExt;
    let outcome = std::panic::AssertUnwindSafe(async {
        let _: String = redis::cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory")
            .arg(1)
            .arg("maxmemory-policy")
            .arg("noeviction")
            .query_async(&mut admin)
            .await
            .unwrap();
        assert!(
            queue.ensure_group().await.is_err(),
            "XGROUP CREATE reproduces DENYOOM despite existing group"
        );
        let reclaimed = queue.reclaim("0-0", 1, 1).await.unwrap();
        assert_eq!(reclaimed.claimed.len(), 1);
        let fresh = queue.read(1).await.unwrap();
        assert_eq!(fresh.len(), 1);
        queue.acknowledge(&reclaimed.claimed[0].id).await.unwrap();
        queue.acknowledge(&fresh[0].id).await.unwrap();
        queue.retire_consumer().await.unwrap();
        let length: i64 = redis::cmd("XLEN")
            .arg(&config.redis_stream)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(length, 0);
    })
    .catch_unwind()
    .await;
    // Always restore this dedicated server, even if the assertions fail.
    let _: String = redis::cmd("CONFIG")
        .arg("SET")
        .arg("maxmemory")
        .arg(&original["maxmemory"])
        .arg("maxmemory-policy")
        .arg(&original["maxmemory-policy"])
        .query_async(&mut admin)
        .await
        .unwrap();
    let _: i64 = redis::cmd("DEL")
        .arg(&config.redis_stream)
        .query_async(&mut admin)
        .await
        .unwrap();
    db.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, TEST_REDIS_URL and CREATE SCHEMA privileges"]
async fn aborting_worker_also_stops_its_background_dispatcher() {
    let db = Database::new().await;
    let server = SlowInference::new(20).await;
    db.submit().await;
    let handle = tokio::spawn(worker::run(
        db.store.clone(),
        settings(&server.base, &db),
        CancellationToken::new(),
    ));
    server.wait_started().await;
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    tokio::task::yield_now().await;
    let queued = db.submit().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let published: Option<Uuid> =
        sqlx::query_scalar("SELECT dispatch_token FROM job_outbox WHERE job_id = $1")
            .bind(queued)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        published.is_none(),
        "dropped worker must not keep claiming outbox rows"
    );
    let messages: redis::streams::StreamRangeReply = redis::cmd("XRANGE")
        .arg(&db.redis_stream)
        .arg("-")
        .arg("+")
        .query_async(&mut db.redis().await)
        .await
        .unwrap();
    assert!(
        !messages
            .ids
            .iter()
            .any(|message| message.get::<String>("job_id").as_deref() == Some(&queued.to_string())),
        "dropped worker must not keep publishing"
    );
    assert_eq!(
        db.store.get(queued).await.unwrap().unwrap().status,
        "queued"
    );
    db.cleanup().await;
}
