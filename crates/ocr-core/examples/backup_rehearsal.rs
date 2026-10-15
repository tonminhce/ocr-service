//! Disposable synthetic backup fixture and SQL recovery rehearsal only.
//!
//! No schema creation, privilege changes, clock rewrites during recovery, Redis
//! publishing, model calls, or production activation. The source/target must be
//! explicitly provisioned disposable databases by the invoking operator.
use std::{collections::BTreeMap, process::ExitCode, sync::OnceLock, time::Duration};

use ocr_core::{
    backup,
    checkpoint::CheckpointWrite,
    domain::NewJob,
    postgres::safe_options,
    retention::{RetentionAction, RetentionHoldRequest},
    review::{ReviewAction, ReviewRequest},
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

type Result<T> = std::result::Result<T, &'static str>;
const MARKER: &str = "isolated-synthetic-only";
static INPUT_OVERRIDE: OnceLock<Vec<u8>> = OnceLock::new();
static PADDING_KIB: OnceLock<usize> = OnceLock::new();
const PREFIX: &str = "ocr-backup-isolated-rehearsal-";
const LABELS: [&str; 7] = [
    "completed",
    "ready",
    "delayed",
    "active",
    "expired",
    "final",
    "dispatch",
];
const HELP: &str = "backup_rehearsal seed|check-snapshot|recover\n\nRequires OCR_BACKUP_REHEARSAL=isolated-synthetic-only and DATABASE_URL_FILE\npointing to an explicitly disposable database's superuser URL. No DATABASE_URL\nenvironment value is accepted. Seed requires an empty migrated database.\nOptional OCR_REHEARSAL_INPUT_FILE supplies one regular PDF up to1MiB for all\njobs; set the same file for all subcommands. Otherwise invalid synthetic PDF\nbytes exercise storage only. Seed after the ops image is built, then backup/\nrestore and start recover within45 seconds. Recover waits real lease expiry\n(up to60 seconds), never changes fixture timestamps, and exercises Store SQL\nfencing/outbox only; no Redis/model. Output contains only aggregate counters/\nhashes, never job IDs/text/credentials.";

struct Harness {
    store: Store,
    pool: PgPool,
}
impl std::ops::Deref for Harness {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}
impl Harness {
    fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn id(index: usize) -> Uuid {
    Uuid::from_u128(0xa050_1500_0000_4000_8000_0000_0000_0000 + index as u128 + 1)
}
fn input(index: usize) -> Vec<u8> {
    if let Some(bytes) = INPUT_OVERRIDE.get() {
        return bytes.clone();
    }
    // Transport/storage fixture: deliberately not a valid inference PDF.
    format!("%PDF-synthetic-backup-rehearsal/1/{}", LABELS[index]).into_bytes()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn result(index: usize) -> Value {
    json!({"extraction":ocr_extraction::extract(&[]),
        "markdown":"synthetic storage rehearsal only", "layout":[],
        "source":{"sha256":hash(&input(index)),"original_retained":true}})
}
fn trace(phase: &str, checks: Value) {
    println!(
        "{}",
        json!({"phase":phase,"checks":checks,
        "synthetic":true,"redis_exercised":false,"model_exercised":false,
        "production_restore_certified":false})
    );
}
fn require(condition: bool, error: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(error) }
}
async fn complete(store: &Harness, index: usize, token: Uuid) -> Result<()> {
    let mut value = result(index);
    if index == 0 {
        let target = PADDING_KIB.get().copied().unwrap_or(0) * 1024;
        if target > 0 {
            let mut padding = String::with_capacity(target + 36);
            while padding.len() < target {
                padding.push_str(&Uuid::new_v4().to_string());
            }
            padding.truncate(target);
            value["rehearsal_padding"] = Value::String(padding);
        }
    }
    require(
        store
            .complete_retaining_input(id(index), token, value)
            .await
            .map_err(|_| "normal completion failed")?,
        "completion lost its lease",
    )
}

async fn seed(store: &Harness) -> Result<()> {
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM public.jobs")
        .fetch_one(store.pool())
        .await
        .map_err(|_| "empty fixture preflight failed")?;
    require(existing == 0, "seed refuses nonempty database")?;
    // No migrations/grants are performed by this fixture helper.
    store
        .check_schema()
        .await
        .map_err(|_| "fixture schema gate failed")?;
    for (index, label) in LABELS.iter().enumerate() {
        let bytes = input(index);
        let submitted = store
            .submit(NewJob {
                id: id(index),
                filename: format!("{PREFIX}{label}.pdf"),
                media_type: "application/pdf".into(),
                sha256: hash(&bytes),
                input: bytes,
                idempotency_key: Some(format!("{PREFIX}{label}")),
                extraction_profile: "vi_rental_contract_v1".into(),
            })
            .await
            .map_err(|_| "synthetic submission failed")?;
        require(submitted.created, "fixture unexpectedly deduplicated")?;
    }
    let completed = store
        .claim_by_id(id(0), 45, 3)
        .await
        .map_err(|_| "completed fixture claim failed")?
        .ok_or("completed fixture absent")?;
    store
        .change_retention_hold(RetentionHoldRequest {
            job_id: id(0),
            request_id: Uuid::new_v4(),
            expected_revision: 0,
            hold_id: Uuid::new_v4(),
            action: RetentionAction::Hold,
            reason: "isolated synthetic fixture hold".into(),
        })
        .await
        .map_err(|_| "fixture hold failed")?;
    complete(store, 0, completed.lease_token).await?;
    store
        .append_review(
            id(0),
            "isolated-synthetic-fixture",
            ReviewRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 0,
                action: ReviewAction::Reject,
                reason: "isolated synthetic fixture review".into(),
                corrections: BTreeMap::new(),
            },
        )
        .await
        .map_err(|_| "fixture review failed")?;

    // Make the delayed state using the same retry transition as a worker.
    let delayed = store
        .claim_by_id(id(2), 45, 3)
        .await
        .map_err(|_| "delayed fixture claim failed")?
        .ok_or("delayed fixture absent")?;
    require(
        store
            .fail(id(2), delayed.lease_token, "synthetic retry", true, 3, 45)
            .await
            .map_err(|_| "fixture retry transition failed")?,
        "fixture retry lease lost",
    )?;
    for index in [3, 4] {
        let lease = store
            .claim_by_id(id(index), if index == 3 { 45 } else { 1 }, 3)
            .await
            .map_err(|_| "checkpoint fixture claim failed")?
            .ok_or("checkpoint fixture absent")?;
        require(
            store
                .save_region_checkpoint(
                    id(index),
                    lease.lease_token,
                    &"a".repeat(64),
                    "synthetic checkpoint",
                )
                .await
                .map_err(|_| "fixture checkpoint failed")?
                == CheckpointWrite::Stored,
            "fixture checkpoint not stored",
        )?;
    }
    // Spend all attempts through normal claim/fail, then let the final lease
    // expire naturally. Recovery must terminalize, never grant a fourth attempt.
    for attempt in 1..=3 {
        let lease = store
            .claim_by_id(id(5), if attempt == 3 { 1 } else { 45 }, 3)
            .await
            .map_err(|_| "final-attempt fixture claim failed")?
            .ok_or("final fixture absent")?;
        require(
            lease.job.attempts == attempt,
            "fixture attempt accounting failed",
        )?;
        if attempt < 3 {
            require(
                store
                    .fail(id(5), lease.lease_token, "synthetic retry", true, 3, 0)
                    .await
                    .map_err(|_| "final fixture retry failed")?,
                "final retry lease lost",
            )?;
        }
    }
    // This targeted fixture-only update establishes a dispatcher already in
    // flight at snapshot time. Recovery never rewrites it or its expiry.
    sqlx::query("UPDATE public.job_outbox SET dispatch_token=$2,dispatch_lease_expires_at=clock_timestamp()+INTERVAL '45 seconds' WHERE job_id=$1")
        .bind(id(6)).bind(Uuid::new_v4()).execute(store.pool()).await
        .map_err(|_| "dispatch fixture setup failed")?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    snapshot(store).await?;
    trace(
        "seeded",
        json!({"jobs":7,"future_window_seconds":45,
        "source_is_inference_fixture":INPUT_OVERRIDE.get().is_some(),"initially_expired_worker_leases":2}),
    );
    Ok(())
}

async fn snapshot(store: &Harness) -> Result<()> {
    let rows=sqlx::query("SELECT id,filename,media_type,sha256,input,idempotency_key,extraction_profile,status,attempts,result,lease_token,lease_expires_at,retention_active_holds,retention_revision FROM public.jobs ORDER BY id")
        .fetch_all(store.pool()).await.map_err(|_| "fixture inventory failed")?;
    require(
        rows.len() == 7,
        "fixture must contain exactly seven synthetic jobs",
    )?;
    for (index, row) in rows.iter().enumerate() {
        let expected_status = match index {
            0 => "succeeded",
            1 | 2 | 6 => "queued",
            _ => "processing",
        };
        let expected_attempts = match index {
            1 | 6 => 0,
            5 => 3,
            _ => 1,
        };
        let expected_key = format!("{PREFIX}{}", LABELS[index]);
        require(
            row.get::<Uuid, _>("id") == id(index)
                && row.get::<String, _>("filename") == format!("{expected_key}.pdf")
                && row.get::<String, _>("media_type") == "application/pdf"
                && row.get::<String, _>("sha256") == hash(&input(index))
                && row.get::<Vec<u8>, _>("input") == input(index)
                && row.get::<Option<String>, _>("idempotency_key").as_deref()
                    == Some(expected_key.as_str())
                && row.get::<String, _>("extraction_profile") == "vi_rental_contract_v1"
                && row.get::<String, _>("status") == expected_status
                && row.get::<i32, _>("attempts") == expected_attempts,
            "synthetic fixture source/state/attempts differ",
        )?;
        let processing = (3..=5).contains(&index);
        require(
            row.get::<Option<Uuid>, _>("lease_token").is_some() == processing,
            "fixture worker token shape differs",
        )?;
        require(
            row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("lease_expires_at")
                .is_some()
                == processing,
            "fixture worker expiry shape differs",
        )?;
        require(
            row.get::<i32, _>("retention_active_holds") == if index == 0 { 1 } else { 0 }
                && row.get::<i64, _>("retention_revision") == if index == 0 { 1 } else { 0 },
            "fixture hold counters differ",
        )?;
        if index == 0 {
            let mut saved = row
                .get::<Option<Value>, _>("result")
                .ok_or("completed fixture result absent")?;
            let expected_padding = PADDING_KIB.get().copied().unwrap_or(0) * 1024;
            let padding = saved
                .as_object_mut()
                .ok_or("completed fixture result shape differs")?
                .remove("rehearsal_padding");
            require(
                match padding {
                    None => expected_padding == 0,
                    Some(Value::String(s)) => {
                        s.len() == expected_padding
                            && expected_padding > 0
                            && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                    }
                    _ => false,
                },
                "synthetic archive padding differs",
            )?;
            require(
                saved == result(0),
                "fixture original extraction/result differs",
            )?;
        } else {
            require(
                row.get::<Option<Value>, _>("result").is_none(),
                "nonterminal result exists",
            )?;
        }
    }
    let counts: (i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM public.job_outbox),(SELECT count(*) FROM public.job_review_events),(SELECT count(*) FROM public.region_checkpoints),(SELECT count(*) FROM public.job_retention_holds),(SELECT count(*) FROM public.job_retention_events)")
        .fetch_one(store.pool()).await.map_err(|_| "fixture auxiliary counts failed")?;
    require(counts == (6, 1, 2, 1, 1), "fixture auxiliary counts differ")?;
    let review:bool=sqlx::query_scalar("SELECT action='reject' AND revision=1 AND status='rejected' FROM public.job_review_events WHERE job_id=$1")
        .bind(id(0)).fetch_one(store.pool()).await.map_err(|_| "fixture review check failed")?;
    require(review, "fixture rejected review differs")?;
    let mut tx = store
        .pool()
        .begin()
        .await
        .map_err(|_| "snapshot transaction failed")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|_| "snapshot isolation failed")?;
    let content = backup::content_manifest(&mut tx)
        .await
        .map_err(|_| "snapshot content failed")?;
    tx.rollback()
        .await
        .map_err(|_| "snapshot rollback failed")?;
    trace(
        "snapshot_checked",
        json!({"jobs":7,"succeeded":1,"processing":3,"queued":3,
        "outbox":6,"review_events":1,"checkpoints":2,"holds":1,
        "retention_events":1,"raw_source_and_results_exact":true,"content":content}),
    );
    Ok(())
}

async fn recover(store: &Harness) -> Result<()> {
    snapshot(store).await?;
    let initial_tokens: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id,lease_token FROM public.jobs WHERE status='processing' ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(|_| "prior token lookup failed")?;
    let token = |index: usize| {
        initial_tokens
            .iter()
            .find(|(job, _)| *job == id(index))
            .map(|(_, t)| *t)
            .ok_or("prior worker token missing")
    };
    let old_dispatch: Uuid =
        sqlx::query_scalar("SELECT dispatch_token FROM public.job_outbox WHERE job_id=$1")
            .bind(id(6))
            .fetch_one(store.pool())
            .await
            .map_err(|_| "prior dispatch token missing")?;
    let window_open:bool=sqlx::query_scalar("SELECT (SELECT available_at>clock_timestamp() FROM public.jobs WHERE id=$1) AND (SELECT lease_expires_at>clock_timestamp() FROM public.jobs WHERE id=$2) AND (SELECT dispatch_lease_expires_at>clock_timestamp() FROM public.job_outbox WHERE job_id=$3)")
        .bind(id(2)).bind(id(3)).bind(id(6)).fetch_one(store.pool()).await
        .map_err(|_| "future-window check failed")?;
    require(
        window_open,
        "future fixture window elapsed; rebuild fresh rehearsal, never alter expiry",
    )?;
    for index in [2, 3] {
        require(
            store
                .claim_by_id(id(index), 10, 3)
                .await
                .map_err(|_| "suppression claim failed")?
                .is_none(),
            "delayed or live-lease job was claimable",
        )?;
    }
    let deliveries = store
        .claim_outbox(16, 10)
        .await
        .map_err(|_| "initial outbox claim failed")?;
    let mut delivered: Vec<_> = deliveries.iter().map(|d| d.job_id).collect();
    delivered.sort();
    let mut expected = vec![id(1), id(4), id(5)];
    expected.sort();
    require(
        delivered == expected,
        "initial outbox did not suppress future delay/worker/dispatcher leases",
    )?;
    for index in [4, 5] {
        require(
            !store
                .renew(id(index), token(index)?, 10)
                .await
                .map_err(|_| "expired renew check failed")?
                && !store
                    .complete_retaining_input(id(index), token(index)?, result(index))
                    .await
                    .map_err(|_| "expired completion check failed")?,
            "expired worker was accepted",
        )?;
    }
    let ready = store
        .claim_by_id(id(1), 10, 3)
        .await
        .map_err(|_| "ready claim failed")?
        .ok_or("ready claim absent")?;
    require(ready.job.attempts == 1, "ready attempt differs")?;
    complete(store, 1, ready.lease_token).await?;
    let expired = store
        .claim_by_id(id(4), 10, 3)
        .await
        .map_err(|_| "expired reclaim failed")?
        .ok_or("expired reclaim absent")?;
    require(
        expired.job.attempts == 2 && expired.lease_token != token(4)?,
        "expired attempts/token not preserved",
    )?;
    require(
        store
            .region_checkpoint(id(4), expired.lease_token, &"a".repeat(64))
            .await
            .map_err(|_| "reclaimed checkpoint check failed")?
            .as_deref()
            == Some("synthetic checkpoint"),
        "restored checkpoint unavailable to new lease",
    )?;
    complete(store, 4, expired.lease_token).await?;
    require(
        store
            .region_checkpoint(id(4), expired.lease_token, &"a".repeat(64))
            .await
            .map_err(|_| "terminal checkpoint fence check failed")?
            .is_none(),
        "terminal job exposed retained checkpoint",
    )?;
    require(
        store
            .claim_by_id(id(5), 10, 3)
            .await
            .map_err(|_| "final exhaustion check failed")?
            .is_none(),
        "final attempt was incorrectly retried",
    )?;
    let final_job = store
        .get(id(5))
        .await
        .map_err(|_| "final state lookup failed")?
        .ok_or("final state absent")?;
    require(
        final_job.status == "failed" && final_job.attempts == 3,
        "final exhaustion state differs",
    )?;
    trace(
        "initial_recovery",
        json!({"future_delayed_worker_dispatch_suppressed":3,
        "expired_stale_worker_writes_rejected":4,"ready_claim_attempts":1,
        "expired_reclaim_attempts":2,"final_attempts":3,"final_terminalized":true,
        "checkpoint_reused_by_new_lease":true,"outbox_notifications_claimed":3,
        "redis_acknowledgements":0}),
    );

    let remaining:f64=sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM GREATEST((SELECT available_at FROM public.jobs WHERE id=$1),(SELECT lease_expires_at FROM public.jobs WHERE id=$2),(SELECT dispatch_lease_expires_at FROM public.job_outbox WHERE job_id=$3))-clock_timestamp())::float8")
        .bind(id(2)).bind(id(3)).bind(id(6)).fetch_one(store.pool()).await
        .map_err(|_| "expiry wait calculation failed")?;
    require(
        remaining.is_finite() && remaining <= 60.0,
        "natural expiry wait exceeds rehearsal bound",
    )?;
    trace(
        "waiting_natural_expiry",
        json!({"maximum_wait_seconds":remaining.max(0.0).ceil(),
        "timestamp_updates":0}),
    );
    let wait_started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(remaining.max(0.0) + 0.1)).await;
    require(
        !store
            .renew(id(3), token(3)?, 10)
            .await
            .map_err(|_| "old live token renew failed")?
            && !store
                .complete_retaining_input(id(3), token(3)?, result(3))
                .await
                .map_err(|_| "old live token completion failed")?,
        "now-expired worker token was accepted",
    )?;
    require(
        store
            .region_checkpoint(id(3), token(3)?, &"a".repeat(64))
            .await
            .map_err(|_| "stale checkpoint check failed")?
            .is_none(),
        "stale worker read checkpoint",
    )?;
    let deliveries = store
        .claim_outbox(16, 10)
        .await
        .map_err(|_| "expired dispatcher outbox claim failed")?;
    let mut delivered: Vec<_> = deliveries.iter().map(|d| d.job_id).collect();
    delivered.sort();
    let mut expected = vec![id(2), id(3), id(6)];
    expected.sort();
    require(
        delivered == expected,
        "naturally expired outbox recovery differs",
    )?;
    require(
        !store
            .mark_dispatched(id(6), old_dispatch, 10)
            .await
            .map_err(|_| "stale dispatcher fence check failed")?,
        "old dispatcher token was accepted",
    )?;
    for index in [2, 3, 6] {
        let lease = store
            .claim_by_id(id(index), 10, 3)
            .await
            .map_err(|_| "natural expiry claim failed")?
            .ok_or("natural expiry claim absent")?;
        require(
            lease.job.attempts == if index == 6 { 1 } else { 2 },
            "recovered attempt differs",
        )?;
        if index == 3 {
            require(
                lease.lease_token != token(3)?
                    && store
                        .region_checkpoint(id(3), lease.lease_token, &"a".repeat(64))
                        .await
                        .map_err(|_| "active checkpoint reuse failed")?
                        .as_deref()
                        == Some("synthetic checkpoint"),
                "active checkpoint/new lease differs",
            )?;
        }
        complete(store, index, lease.lease_token).await?;
        if index == 3 {
            require(
                store
                    .region_checkpoint(id(3), lease.lease_token, &"a".repeat(64))
                    .await
                    .map_err(|_| "terminal checkpoint fence check failed")?
                    .is_none(),
                "terminal job exposed retained checkpoint",
            )?;
        }
    }
    let final_counts:(i64,i64,i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE status='succeeded'),count(*) FILTER(WHERE status='failed'),count(*) FILTER(WHERE octet_length(input)>0),(SELECT count(*) FROM public.job_outbox),(SELECT count(*) FROM public.region_checkpoints),(SELECT count(*) FROM public.job_review_events),(SELECT count(*) FROM public.job_retention_holds),(SELECT count(*) FROM public.job_retention_events) FROM public.jobs")
        .fetch_one(store.pool()).await.map_err(|_| "recovery totals failed")?;
    require(
        final_counts == (6, 1, 7, 0, 2, 1, 1, 1),
        "recovery final counters differ",
    )?;
    trace(
        "recovery_verified",
        json!({"succeeded":6,"failed":1,"sources_retained":7,
        "outbox":0,"retained_checkpoint_rows":2,"terminal_checkpoint_reads_blocked":true,
        "review_events":1,"holds":1,"retention_events":1,
        "old_worker_token_rejected_after_expiry":true,"old_dispatch_token_rejected_after_replacement":true,
        "natural_wait_seconds":wait_started.elapsed().as_secs_f64(),"attempts_reset":false,
        "timestamps_rewritten":false,"runtime_activated":false}),
    );
    Ok(())
}

async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    require(
        args.len() == 1 && matches!(args[0].as_str(), "seed" | "check-snapshot" | "recover"),
        "use --help",
    )?;
    require(
        std::env::var("OCR_BACKUP_REHEARSAL").as_deref() == Ok(MARKER),
        "explicit isolated-synthetic marker required",
    )?;
    let padding = std::env::var("OCR_REHEARSAL_PADDING_KIB")
        .unwrap_or_else(|_| "0".into())
        .parse::<usize>()
        .map_err(|_| "synthetic padding must be integer KiB")?;
    require(padding <= 1024, "synthetic padding exceeds1024KiB")?;
    PADDING_KIB
        .set(padding)
        .map_err(|_| "synthetic padding already configured")?;
    require(
        std::env::var_os("DATABASE_URL").is_none()
            && std::env::var_os("DATABASE_URL_FILE").is_some(),
        "only DATABASE_URL_FILE is accepted",
    )?;
    if let Some(path) = std::env::var_os("OCR_REHEARSAL_INPUT_FILE") {
        use std::io::Read;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options
            .open(path)
            .map_err(|_| "synthetic PDF fixture unavailable")?;
        require(
            file.metadata()
                .map_err(|_| "synthetic PDF metadata failed")?
                .is_file(),
            "synthetic PDF fixture must be a regular file",
        )?;
        let mut bytes = Vec::new();
        file.take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|_| "synthetic PDF fixture read failed")?;
        require(
            bytes.len() <= 1_048_576 && bytes.starts_with(b"%PDF-"),
            "synthetic PDF must be bounded and have PDF signature",
        )?;
        INPUT_OVERRIDE
            .set(bytes)
            .map_err(|_| "synthetic PDF already configured")?;
    }
    let url =
        ocr_core::secrets::required("DATABASE_URL").map_err(|_| "database secret unavailable")?;
    let options = safe_options(&url).map_err(|_| "database endpoint unsupported")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .map_err(|_| "disposable database connection failed")?;
    let privileged:bool=sqlx::query_scalar("SELECT r.rolsuper AND session_user=current_user AND NOT d.datistemplate FROM pg_roles r CROSS JOIN pg_database d WHERE r.rolname=session_user AND d.datname=current_database()")
        .fetch_one(&pool).await.map_err(|_| "fixture principal preflight failed")?;
    require(
        privileged,
        "fixture requires actual disposable database superuser",
    )?;
    sqlx::query("SET statement_timeout='5s'")
        .execute(&pool)
        .await
        .map_err(|_| "fixture statement bound failed")?;
    let store = Harness {
        store: Store::from_pool(pool.clone()),
        pool: pool.clone(),
    };
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        match args[0].as_str() {
            "seed" => seed(&store).await,
            "check-snapshot" => snapshot(&store).await,
            "recover" => recover(&store).await,
            _ => unreachable!(),
        }
    })
    .await
    .map_err(|_| "fixture total deadline exceeded")?;
    pool.close().await;
    outcome
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backup_rehearsal: {error}");
            ExitCode::FAILURE
        }
    }
}
