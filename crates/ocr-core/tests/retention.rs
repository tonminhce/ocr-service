//! Disposable-database tests. No policy or destructive apply is enabled.
//! TEST_DATABASE_URL=postgres://... cargo test -p ocr-core --test retention -- --ignored --nocapture
use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use ocr_core::{
    domain::NewJob,
    postgres::safe_options,
    retention::{
        RetentionAction, RetentionError, RetentionExclusion, RetentionHoldRequest,
        RetentionPlanRequest,
    },
    review::{ReviewAction, ReviewRequest},
    store::Store,
};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

const CHANGE: &str = "SELECT revision,active_holds,source_present,replayed,created_at FROM change_job_retention_hold($1,$2,$3,$4,$5,$6)";
const SOURCE: &[u8] = b"%PDF-original-private-source";

struct Database {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
}
impl Database {
    async fn new() -> Self {
        let uri =
            std::env::var("TEST_DATABASE_URL").expect("disposable TEST_DATABASE_URL required");
        let options = safe_options(&uri).unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let schema = format!("ocr_retention_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(
                options
                    .application_name(&schema)
                    .options([("search_path", schema.as_str())]),
            )
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
    async fn job(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.store
            .submit(NewJob {
                id,
                filename: "private-filename-sentinel.pdf".into(),
                media_type: "application/pdf".into(),
                sha256: "test".into(),
                input: SOURCE.to_vec(),
                idempotency_key: None,
                extraction_profile: "vi_rental_contract_v1".into(),
            })
            .await
            .unwrap();
        id
    }
    async fn completed(&self) -> Uuid {
        let id = self.job().await;
        let lease = self.store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
        assert!(self.store.complete_retaining_input(id,lease.lease_token,
            json!({"extraction":ocr_extraction::extract(&[]),"markdown":"raw-text-sentinel","layout":[]}))
            .await.unwrap());
        id
    }
    async fn state(&self, id: Uuid) -> (i64, i32, i64, i64, Vec<u8>) {
        sqlx::query_as("SELECT retention_revision,retention_active_holds,(SELECT count(*) FROM job_retention_holds WHERE job_id=j.id),(SELECT count(*) FROM job_retention_events WHERE job_id=j.id),input FROM jobs j WHERE id=$1")
            .bind(id).fetch_one(&self.pool).await.unwrap()
    }
    async fn blocked(&self, pid: i32) {
        tokio::time::timeout(Duration::from_secs(1),async {
            loop {
                let blocked:bool=sqlx::query_scalar("SELECT COALESCE(wait_event_type='Lock',false) FROM pg_stat_activity WHERE pid=$1")
                    .bind(pid).fetch_one(&self.admin).await.unwrap();
                if blocked {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("server must observe actual lock wait");
    }
    async fn recovered(&self) {
        tokio::time::timeout(Duration::from_secs(2),async {
            let mut connections=Vec::new();
            for _ in 0..6 {
                let mut c=self.pool.acquire().await.unwrap();
                assert_eq!(sqlx::query_scalar::<_,i32>("SELECT 1").fetch_one(&mut *c).await.unwrap(),1);
                connections.push(c);
            }
            let leaked:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND state LIKE 'idle in transaction%'")
                .bind(&self.schema).fetch_one(&self.admin).await.unwrap();
            assert_eq!(leaked,0);
        }).await.expect("cancelled transaction must release every pool connection");
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
fn hold(id: Uuid, expected_revision: i64) -> RetentionHoldRequest {
    RetentionHoldRequest {
        job_id: id,
        request_id: Uuid::new_v4(),
        expected_revision,
        hold_id: Uuid::new_v4(),
        action: RetentionAction::Hold,
        reason: "private-reason-sentinel".into(),
    }
}
fn release(request: &RetentionHoldRequest, revision: i64) -> RetentionHoldRequest {
    RetentionHoldRequest {
        request_id: Uuid::new_v4(),
        expected_revision: revision,
        action: RetentionAction::Release,
        ..request.clone()
    }
}
fn message(error: sqlx::Error) -> String {
    error.as_database_error().unwrap().message().to_owned()
}
async fn change_in_tx(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, r: &RetentionHoldRequest) {
    sqlx::query(CHANGE)
        .bind(r.job_id)
        .bind(r.request_id)
        .bind(r.expected_revision)
        .bind(r.hold_id)
        .bind(match r.action {
            RetentionAction::Hold => "hold",
            RetentionAction::Release => "release",
        })
        .bind(&r.reason)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn independent_holds_replay_original_snapshot_and_preserve_service_actor() {
    let db = Database::new().await;
    let id = db.job().await;
    let first = hold(id, 0);
    let a = db.store.change_retention_hold(first.clone()).await.unwrap();
    assert_eq!(
        (a.revision, a.active_holds, a.source_present, a.replayed),
        (1, 1, true, false)
    );
    let second = hold(id, 1);
    let b = db
        .store
        .change_retention_hold(second.clone())
        .await
        .unwrap();
    assert_eq!((b.revision, b.active_holds), (2, 2));
    let released = db
        .store
        .change_retention_hold(release(&first, 2))
        .await
        .unwrap();
    assert_eq!((released.revision, released.active_holds), (3, 1));
    let replay = db.store.change_retention_hold(first.clone()).await.unwrap();
    assert_eq!(
        (replay.revision, replay.active_holds, replay.created_at),
        (1, 1, a.created_at)
    );
    assert!(replay.replayed);
    for changed in [
        RetentionHoldRequest {
            reason: "different".into(),
            ..first.clone()
        },
        RetentionHoldRequest {
            hold_id: Uuid::new_v4(),
            ..first.clone()
        },
        RetentionHoldRequest {
            expected_revision: 3,
            ..first.clone()
        },
    ] {
        assert_eq!(
            db.store.change_retention_hold(changed).await.unwrap_err(),
            RetentionError::Conflict
        );
    }
    assert_eq!(
        db.store
            .change_retention_hold(hold(id, 0))
            .await
            .unwrap_err(),
        RetentionError::Conflict
    );
    assert_eq!(
        db.store
            .change_retention_hold(release(&first, 3))
            .await
            .unwrap_err(),
        RetentionError::HoldNotFound
    );
    let principal: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let actors: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT principal FROM job_retention_events WHERE job_id=$1")
            .bind(id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(actors, vec![principal]);
    assert_eq!(db.state(id).await, (3, 1, 1, 3, SOURCE.to_vec()));
    eprintln!(
        "retention trace independent_holds: hold1=1 hold2=2 release1=3 remaining=1 replay_revision=1 replay_count=1 actor=session_user source_preserved=true"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn database_guards_block_legacy_deletion_event_mutation_and_held_source_loss() {
    let db = Database::new().await;
    let id = db.job().await;
    db.store.change_retention_hold(hold(id, 0)).await.unwrap();
    let lease = db.store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    assert!(
        db.store
            .complete(
                id,
                lease.lease_token,
                json!({"immutable":"raw-text-sentinel","source":{"original_retained":false,"strategy":"full_page"}})
            )
            .await
            .unwrap()
    );
    assert_eq!(db.state(id).await.4, SOURCE);
    for sql in [
        "UPDATE jobs SET input=''::bytea WHERE id=$1",
        "UPDATE jobs SET input='replacement'::bytea WHERE id=$1",
    ] {
        assert_eq!(
            message(
                sqlx::query(sql)
                    .bind(id)
                    .execute(&db.pool)
                    .await
                    .unwrap_err()
            ),
            "retention_held_source_immutable"
        );
    }
    for sql in [
        "DELETE FROM jobs WHERE id=$1",
        "UPDATE job_retention_events SET reason='changed' WHERE job_id=$1",
        "DELETE FROM job_retention_events WHERE job_id=$1",
    ] {
        let expected = if sql.starts_with("DELETE FROM jobs") {
            "retention_apply_disabled"
        } else {
            "retention_events_append_only"
        };
        assert_eq!(
            message(
                sqlx::query(sql)
                    .bind(id)
                    .execute(&db.pool)
                    .await
                    .unwrap_err()
            ),
            expected
        );
    }
    for (sql, expected) in [
        ("TRUNCATE jobs CASCADE", "retention_apply_disabled"),
        (
            "TRUNCATE job_retention_events",
            "retention_events_append_only",
        ),
    ] {
        assert_eq!(
            message(sqlx::query(sql).execute(&db.pool).await.unwrap_err()),
            expected
        );
    }
    sqlx::query("UPDATE jobs SET updated_at=clock_timestamp()-INTERVAL '90 days' WHERE id=$1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.store.prune(7).await.is_err());
    assert_eq!(
        db.store.retention_apply().await.unwrap_err(),
        RetentionError::ApplyDisabled
    );
    assert_eq!(db.state(id).await, (1, 1, 1, 1, SOURCE.to_vec()));
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().result.unwrap(),
        json!({"immutable":"raw-text-sentinel","source":{"original_retained":true,"strategy":"full_page"}})
    );
    eprintln!(
        "retention trace guards: legacy_prune=blocked jobs_delete=blocked jobs_truncate=blocked event_update_delete_truncate=blocked held_source_clear_replace=blocked modern_completion=preserved apply=disabled"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn missing_original_is_reported_truthfully_and_release_does_not_invent_recovery() {
    let db = Database::new().await;
    let id = db.job().await;
    let lease = db.store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    assert!(
        db.store
            .complete(id, lease.lease_token, json!({"ok":true}))
            .await
            .unwrap()
    );
    let request = hold(id, 0);
    let event = db
        .store
        .change_retention_hold(request.clone())
        .await
        .unwrap();
    assert!(!event.source_present);
    assert!(
        !db.store
            .change_retention_hold(release(&request, 1))
            .await
            .unwrap()
            .source_present
    );
    assert_eq!(db.state(id).await, (2, 0, 0, 2, vec![]));
    let plan = db
        .store
        .retention_plan(RetentionPlanRequest::default())
        .await
        .unwrap();
    assert!(!plan.items[0].source_present);
    assert_eq!(plan.items[0].source_payload_bytes, 0);
    eprintln!(
        "retention trace source_truth: completed_before_hold=true original_already_missing=true hold_source_present=false release_source_present=false no_recovery_claim=true"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn sql_boundary_rejects_null_invalid_and_inconsistent_requests_without_partial_writes() {
    let db = Database::new().await;
    let id = db.job().await;
    let request_id = Uuid::new_v4();
    let hold_id = Uuid::new_v4();
    for omitted in 0..6 {
        let result = sqlx::query(CHANGE)
            .bind((omitted != 0).then_some(id))
            .bind((omitted != 1).then_some(request_id))
            .bind((omitted != 2).then_some(0_i64))
            .bind((omitted != 3).then_some(hold_id))
            .bind((omitted != 4).then_some("hold"))
            .bind((omitted != 5).then_some("reason"))
            .fetch_one(&db.pool)
            .await;
        assert_eq!(message(result.unwrap_err()), "retention_invalid_request");
    }
    for reason in [
        "".to_owned(),
        " \t\n".to_owned(),
        "bad\r".to_owned(),
        "é".repeat(1001),
    ] {
        assert_eq!(
            message(
                sqlx::query(CHANGE)
                    .bind(id)
                    .bind(request_id)
                    .bind(0_i64)
                    .bind(hold_id)
                    .bind("hold")
                    .bind(reason)
                    .fetch_one(&db.pool)
                    .await
                    .unwrap_err()
            ),
            "retention_invalid_request"
        );
    }
    assert_eq!(
        message(
            sqlx::query(CHANGE)
                .bind(id)
                .bind(request_id)
                .bind(-1_i64)
                .bind(hold_id)
                .bind("hold")
                .bind("reason")
                .fetch_one(&db.pool)
                .await
                .unwrap_err()
        ),
        "retention_invalid_request"
    );
    assert_eq!(
        message(
            sqlx::query(CHANGE)
                .bind(id)
                .bind(request_id)
                .bind(0_i64)
                .bind(hold_id)
                .bind("anything")
                .bind("reason")
                .fetch_one(&db.pool)
                .await
                .unwrap_err()
        ),
        "retention_invalid_request"
    );
    assert_eq!(db.state(id).await, (0, 0, 0, 0, SOURCE.to_vec()));
    assert_eq!(
        db.store
            .change_retention_hold(hold(Uuid::new_v4(), 0))
            .await
            .unwrap_err(),
        RetentionError::NotFound
    );
    // Deliberately corrupt only this disposable fixture with owner authority.
    sqlx::query("UPDATE jobs SET retention_active_holds=1 WHERE id=$1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        db.store
            .change_retention_hold(hold(id, 0))
            .await
            .unwrap_err(),
        RetentionError::InconsistentState
    );
    assert_eq!(db.state(id).await, (0, 1, 0, 0, SOURCE.to_vec()));
    eprintln!(
        "retention trace SQL_validation: null_arguments=6 invalid_reason_bytes_controls=true invalid_action_revision=true no_partial_state=true inconsistent_state=fail_closed"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn active_hold_cap_and_event_reservations_never_strand_an_admitted_hold() {
    let db = Database::new().await;
    let id = db.job().await;
    let mut first = None;
    for revision in 0..128 {
        let request = hold(id, revision);
        if first.is_none() {
            first = Some(request.clone());
        }
        assert_eq!(
            db.store
                .change_retention_hold(request)
                .await
                .unwrap()
                .active_holds,
            revision as u32 + 1
        );
    }
    assert_eq!(
        db.store
            .change_retention_hold(hold(id, 128))
            .await
            .unwrap_err(),
        RetentionError::HoldLimit
    );
    assert_eq!(
        db.store
            .change_retention_hold(release(&first.unwrap(), 128))
            .await
            .unwrap()
            .active_holds,
        127
    );
    let boundary = db.job().await;
    let a = hold(boundary, 0);
    db.store.change_retention_hold(a.clone()).await.unwrap();
    // Owner fixture advances the counter; no history fabrication or apply bypass.
    sqlx::query("UPDATE jobs SET retention_revision=9997 WHERE id=$1")
        .bind(boundary)
        .execute(&db.pool)
        .await
        .unwrap();
    let b = hold(boundary, 9997);
    assert_eq!(
        db.store
            .change_retention_hold(b.clone())
            .await
            .unwrap()
            .revision,
        9998
    );
    assert_eq!(
        db.store
            .change_retention_hold(hold(boundary, 9998))
            .await
            .unwrap_err(),
        RetentionError::EventLimit
    );
    assert_eq!(
        db.store
            .change_retention_hold(release(&a, 9998))
            .await
            .unwrap()
            .revision,
        9999
    );
    assert_eq!(
        db.store
            .change_retention_hold(hold(boundary, 9999))
            .await
            .unwrap_err(),
        RetentionError::EventLimit
    );
    let final_release = db
        .store
        .change_retention_hold(release(&b, 9999))
        .await
        .unwrap();
    assert_eq!(
        (final_release.revision, final_release.active_holds),
        (10000, 0)
    );
    assert_eq!(
        db.store
            .change_retention_hold(hold(boundary, 10000))
            .await
            .unwrap_err(),
        RetentionError::EventLimit
    );
    assert!(db.store.change_retention_hold(b).await.unwrap().replayed);
    assert!(
        sqlx::query("UPDATE jobs SET retention_active_holds=1 WHERE id=$1")
            .bind(boundary)
            .execute(&db.pool)
            .await
            .is_err()
    );
    eprintln!(
        "retention trace budgets: active_cap=128 over_cap=blocked revision9998_holds=2 reserved_releases=2 revision10000_holds=0 new_event=blocked original_replay=allowed"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn uncommitted_hold_blocks_old_worker_source_clear_using_locked_old_row() {
    let db = Database::new().await;
    let id = db.job().await;
    let r = hold(id, 0);
    let mut tx = db.pool.begin().await.unwrap();
    change_in_tx(&mut tx, &r).await;
    let mut old_worker = db.pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *old_worker)
        .await
        .unwrap();
    let writer = tokio::spawn(async move {
        sqlx::query("UPDATE jobs SET input=''::bytea WHERE id=$1")
            .bind(id)
            .execute(&mut *old_worker)
            .await
    });
    db.blocked(pid).await;
    tx.commit().await.unwrap();
    assert_eq!(
        message(writer.await.unwrap().unwrap_err()),
        "retention_held_source_immutable"
    );
    assert_eq!(db.state(id).await, (1, 1, 1, 1, SOURCE.to_vec()));
    db.recovered().await;
    eprintln!(
        "retention trace hold_then_legacy_completion: server_lock_wait_observed=true hold_commit_first=true stale_statement_snapshot_cannot_clear_source=true pool_recovered=6"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn completion_and_hold_serialize_in_both_orders_without_source_recovery_fiction() {
    let db = Database::new().await;
    let id = db.job().await;
    let lease = db.store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    let r = hold(id, 0);
    let mut holder = db.pool.begin().await.unwrap();
    change_in_tx(&mut holder, &r).await;
    let store = db.store.clone();
    let completion = tokio::spawn(async move {
        store
            .complete(id, lease.lease_token, json!({"first":"hold"}))
            .await
    });
    // Explicitly prove Store's FOR UPDATE is waiting on the holder transaction.
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query='SELECT id FROM jobs WHERE id=$1 FOR UPDATE')")
                .bind(&db.schema).fetch_one(&db.admin).await.unwrap();
            if waiting {break;}tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    holder.commit().await.unwrap();
    assert!(completion.await.unwrap().unwrap());
    assert_eq!(db.state(id).await.4, SOURCE);
    let id2 = db.job().await;
    let lease2 = db.store.claim_by_id(id2, 120, 3).await.unwrap().unwrap();
    // Simulate an old completion that acquires the parent lock first.
    let mut completing = db.pool.begin().await.unwrap();
    sqlx::query("UPDATE jobs SET input=''::bytea,status='succeeded',result='{}'::jsonb,lease_token=NULL,lease_expires_at=NULL WHERE id=$1 AND lease_token=$2")
        .bind(id2).bind(lease2.lease_token).execute(&mut *completing).await.unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    let second = hold(id2, 0);
    let waiting_hold = tokio::spawn(async move {
        sqlx::query(CHANGE)
            .bind(id2)
            .bind(second.request_id)
            .bind(0_i64)
            .bind(second.hold_id)
            .bind("hold")
            .bind("reason")
            .fetch_one(&mut *connection)
            .await
            .unwrap()
            .get::<bool, _>("source_present")
    });
    db.blocked(pid).await;
    completing.commit().await.unwrap();
    assert!(!waiting_hold.await.unwrap());
    assert_eq!(db.state(id2).await.4, Vec::<u8>::new());
    eprintln!(
        "retention trace completion_races: hold_first_wait_observed=true modern_completion_source=preserved old_completion_first_wait_observed=true later_hold_source_present=false"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn concurrent_cas_replay_and_review_share_parent_lock_without_lost_events() {
    let db = Database::new().await;
    let id = db.completed().await;
    let a = hold(id, 0);
    let b = hold(id, 0);
    let (one, two) = tokio::join!(
        db.store.change_retention_hold(a.clone()),
        db.store.change_retention_hold(b)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert_eq!(
        one.as_ref().err().or(two.as_ref().err()),
        Some(&RetentionError::Conflict)
    );
    let replay_request = hold(id, 1);
    let (one, two) = tokio::join!(
        db.store.change_retention_hold(replay_request.clone()),
        db.store.change_retention_hold(replay_request)
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_ne!(one.replayed, two.replayed);
    assert_eq!((one.revision, two.revision), (2, 2));
    assert_eq!(db.state(id).await, (2, 2, 2, 2, SOURCE.to_vec()));
    let original = db.store.get(id).await.unwrap().unwrap().result.unwrap();
    let mut holder = db.pool.begin().await.unwrap();
    change_in_tx(&mut holder, &hold(id, 2)).await;
    let store = db.store.clone();
    let review = tokio::spawn(async move {
        store
            .append_review(
                id,
                "configured-review-principal",
                ReviewRequest {
                    request_id: Uuid::new_v4(),
                    expected_revision: 0,
                    action: ReviewAction::Reject,
                    reason: "source checked".into(),
                    corrections: BTreeMap::new(),
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE 'SELECT status, CASE WHEN COALESCE(octet_length((result%')")
                .bind(&db.schema).fetch_one(&db.admin).await.unwrap();
            if waiting {break;}tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    holder.commit().await.unwrap();
    assert_eq!(review.await.unwrap().unwrap().revision, 1);
    assert_eq!(db.state(id).await, (3, 3, 3, 3, SOURCE.to_vec()));
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().result.unwrap(),
        original
    );
    assert_eq!(
        db.store.review_history(id, 0).await.unwrap().events.len(),
        1
    );
    eprintln!(
        "retention trace concurrency: same_revision_winners=1 competing_request=conflict same_request_events=1 replay_original=true review_wait_on_parent=observed hold_revision=3 review_revision=1 raw_result_unchanged=true"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn review_commit_holds_parent_lock_before_retention_hold_and_release() {
    let db = Database::new().await;
    let barrier_key = Uuid::new_v4().as_u128() as i64;
    // This isolated-schema fixture pauses the actual append_review transaction
    // at COMMIT, after its event exists and while its jobs row lock is held.
    // An advisory transaction lock supplies an observable barrier without
    // sleeping inside production code or fabricating review events.
    sqlx::query(&format!(
        "CREATE FUNCTION retention_test_review_commit_barrier() RETURNS trigger \
         LANGUAGE plpgsql SECURITY INVOKER SET search_path=pg_catalog,pg_temp AS $$ \
         BEGIN PERFORM pg_advisory_xact_lock({barrier_key}); RETURN NEW; END; $$"
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE CONSTRAINT TRIGGER retention_test_review_commit_pause \
         AFTER INSERT ON job_review_events DEFERRABLE INITIALLY DEFERRED \
         FOR EACH ROW EXECUTE FUNCTION retention_test_review_commit_barrier()",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    for action in [RetentionAction::Hold, RetentionAction::Release] {
        let id = db.completed().await;
        let original = db.store.get(id).await.unwrap().unwrap().result.unwrap();
        let request = hold(id, 0);
        let request = if action == RetentionAction::Release {
            db.store
                .change_retention_hold(request.clone())
                .await
                .unwrap();
            release(&request, 1)
        } else {
            request
        };
        let mut barrier = db.pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(barrier_key)
            .execute(&mut *barrier)
            .await
            .unwrap();
        let store = db.store.clone();
        let review = tokio::spawn(async move {
            store
                .append_review(
                    id,
                    "configured-review-principal",
                    ReviewRequest {
                        request_id: Uuid::new_v4(),
                        expected_revision: 0,
                        action: ReviewAction::Reject,
                        reason: "Source checked before retention mutation".into(),
                        corrections: BTreeMap::new(),
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let committing: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND query='COMMIT' AND wait_event_type='Lock' AND wait_event='advisory')")
                    .bind(&db.schema).fetch_one(&db.admin).await.unwrap();
                if committing {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("actual review COMMIT must wait on the test barrier");
        let store = db.store.clone();
        let retention = tokio::spawn(async move { store.change_retention_hold(request).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE 'SELECT revision,active_holds,source_present,replayed,created_at FROM change_job_retention_hold%')")
                    .bind(&db.schema).fetch_one(&db.admin).await.unwrap();
                if waiting {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("hold/release must wait behind the actual review transaction");
        barrier.rollback().await.unwrap();
        assert_eq!(review.await.unwrap().unwrap().revision, 1);
        let outcome = retention.await.unwrap().unwrap();
        let expected = if action == RetentionAction::Hold {
            (1, 1)
        } else {
            (2, 0)
        };
        assert_eq!((outcome.revision, outcome.active_holds), expected);
        assert!(outcome.source_present);
        assert_eq!(db.state(id).await.4, SOURCE);
        assert_eq!(
            db.store.get(id).await.unwrap().unwrap().result.unwrap(),
            original
        );
        assert_eq!(
            db.store.review_history(id, 0).await.unwrap().events.len(),
            1
        );
        eprintln!(
            "retention trace review_first: action={action:?} actual_review_commit_barrier=observed retention_parent_lock_wait=observed review_revision=1 retention_revision={} active_holds={} source_preserved=true raw_result_unchanged=true",
            outcome.revision, outcome.active_holds
        );
    }
    db.recovered().await;
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn cancellation_rolls_back_hold_and_lock_wait_without_pool_or_event_leaks() {
    let db = Database::new().await;
    let id = db.job().await;
    let r = hold(id, 0);
    let (ready, received) = tokio::sync::oneshot::channel();
    let pool = db.pool.clone();
    let r2 = r.clone();
    let pending = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        change_in_tx(&mut tx, &r2).await;
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
        tx.commit().await.unwrap();
    });
    received.await.unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    db.recovered().await;
    assert_eq!(db.state(id).await, (0, 0, 0, 0, SOURCE.to_vec()));
    let mut parent_lock = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *parent_lock)
        .await
        .unwrap();
    let store = db.store.clone();
    let waiter = tokio::spawn(async move { store.change_retention_hold(r).await });
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE 'SELECT revision,active_holds,source_present,replayed,created_at FROM change_job_retention_hold%')")
                .bind(&db.schema).fetch_one(&db.admin).await.unwrap();
            if waiting {break;}tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    parent_lock.rollback().await.unwrap();
    db.recovered().await;
    assert_eq!(db.state(id).await, (0, 0, 0, 0, SOURCE.to_vec()));
    assert_eq!(
        db.store
            .change_retention_hold(hold(id, 0))
            .await
            .unwrap()
            .revision,
        1
    );
    eprintln!(
        "retention trace cancellation: post_function_precommit_abort=rolled_back lock_wait_abort=rolled_back active_state_events_leaked=0 pool_recovered=6 retry_after_cancel=revision1"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn plan_is_bounded_keyset_metadata_only_and_review_revision_invalidates_due_state() {
    let db = Database::new().await;
    let before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let empty = db
        .store
        .retention_plan(RetentionPlanRequest::default())
        .await
        .unwrap();
    let after: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(empty.page_rows, 0);
    assert!(empty.snapshot_at >= before && empty.snapshot_at <= after);
    assert!(!empty.has_more);
    assert!(empty.next_cursor.is_none());
    let queued = db.job().await;
    let due = db.completed().await;
    let future = db.completed().await;
    let held = db.completed().await;
    for id in [due, future, held] {
        // Explicit disposable fixture, not a production policy assignment API.
        sqlx::query("UPDATE jobs SET retention_policy_id='fixture_policy',retention_class='fixture_class',retain_until=clock_timestamp()-INTERVAL '1 second',retention_review_revision=0 WHERE id=$1")
            .bind(id).execute(&db.pool).await.unwrap();
    }
    sqlx::query("UPDATE jobs SET retain_until=clock_timestamp()+INTERVAL '1 day' WHERE id=$1")
        .bind(future)
        .execute(&db.pool)
        .await
        .unwrap();
    db.store.change_retention_hold(hold(held, 0)).await.unwrap();
    // Equal timestamps exercise the UUID tie-breaker in both SQL and cursor.
    sqlx::query("UPDATE jobs SET created_at='2026-01-01T00:00:00Z'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let page = db
            .store
            .retention_plan(RetentionPlanRequest { limit: 2, cursor })
            .await
            .unwrap();
        assert_eq!(page.page_rows, 2);
        assert_eq!(page.items.len(), 2);
        assert!(!page.apply_enabled);
        assert_eq!(
            page.page_reason_counts[&RetentionExclusion::ApplyDisabled],
            2
        );
        let encoded = serde_json::to_string(&page).unwrap();
        for secret in [
            "private-filename-sentinel",
            "raw-text-sentinel",
            "private-reason-sentinel",
            "%PDF-original-private-source",
        ] {
            assert!(!encoded.contains(secret));
        }
        assert!(encoded.len() < 16 * 1024);
        cursor = page.next_cursor.clone();
        let more = page.has_more;
        all.extend(page.items);
        if !more {
            assert!(cursor.is_none());
            break;
        }
        assert!(cursor.is_some());
    }
    assert_eq!(all.len(), 4);
    let ids: Vec<_> = all.iter().map(|i| i.job_id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(ids, sorted);
    let item = |id| all.iter().find(|i| i.job_id == id).unwrap();
    assert!(
        item(queued)
            .exclusion_reasons
            .contains(&RetentionExclusion::Nonterminal)
    );
    assert!(
        item(queued)
            .exclusion_reasons
            .contains(&RetentionExclusion::UnassignedPolicy)
    );
    assert!(item(due).metadata_due);
    assert_eq!(
        item(due).exclusion_reasons,
        [RetentionExclusion::ApplyDisabled]
    );
    assert_eq!(item(due).source_payload_bytes, SOURCE.len() as u64);
    assert!(item(due).result_stored_datum_bytes > 0);
    assert!(
        item(future)
            .exclusion_reasons
            .contains(&RetentionExclusion::NotDue)
    );
    assert!(
        item(held)
            .exclusion_reasons
            .contains(&RetentionExclusion::Held)
    );
    db.store
        .append_review(
            due,
            "configured-review-principal",
            ReviewRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 0,
                action: ReviewAction::Reject,
                reason: "Checked source".into(),
                corrections: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    let changed = db
        .store
        .retention_plan(RetentionPlanRequest::default())
        .await
        .unwrap();
    let item = changed.items.iter().find(|i| i.job_id == due).unwrap();
    assert!(!item.metadata_due);
    assert_eq!(
        (item.captured_review_revision, item.latest_review_revision),
        (Some(0), 1)
    );
    assert!(
        item.exclusion_reasons
            .contains(&RetentionExclusion::ReviewRevisionChanged)
    );
    for limit in [0, 101, usize::MAX] {
        assert!(matches!(
            db.store
                .retention_plan(RetentionPlanRequest {
                    limit,
                    cursor: None
                })
                .await,
            Err(RetentionError::InvalidRequest(_))
        ));
    }
    let index:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE schemaname=$1 AND indexname='jobs_retention_scan_idx' AND indexdef LIKE '%(created_at, id)%')")
        .bind(&db.schema).fetch_one(&db.pool).await.unwrap();
    assert!(index);
    eprintln!(
        "retention trace plan: empty_clock=database rows=4 page_limit=2 pages=2 tie_breaker=UUID duplicate_or_missing_rows=0 text_filename_reason_leaks=0 due_metadata_only=1 review_revision_change=excluded apply_enabled=false scan_index=present"
    );
    db.cleanup().await;
}
