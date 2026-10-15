//! Isolated cluster only: TEST_ADMIN_DATABASE_URL=... cargo test -p ocr-core
//! --test administration -- --ignored --nocapture --test-threads=1
//! Fixed cluster roles are intentional; never point this test at a live DB.
use std::{borrow::Cow, collections::BTreeMap, str::FromStr};

use ocr_core::{
    administration::{self, AdminError, LOGIN_ROLES},
    checkpoint::CheckpointWrite,
    domain::NewJob,
    review::{ReviewAction, ReviewRequest},
    schema::{MIGRATOR, SchemaError},
    store::{Store, StoreError},
};
use serde_json::json;
use sqlx::{
    Connection, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

fn passwords() -> [String; 5] {
    // Synthetic disposable credentials include SQL literal edge characters.
    std::array::from_fn(|i| format!("isolated-role-{i}-quote'back\\slash-safe"))
}

async fn pool(options: &PgConnectOptions, role: &str, password: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone().username(role).password(password))
        .await
        .unwrap()
}

async fn denied(pool: &PgPool, sql: &str) {
    let error = sqlx::query(sql)
        .execute(pool)
        .await
        .expect_err("restricted mutation must fail");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{sql}"
    );
}

async fn change_hold(
    pool: &PgPool,
    job_id: Uuid,
    request_id: Uuid,
    expected_revision: i64,
    hold_id: Uuid,
    action: &str,
) -> (i64, i32, bool, bool) {
    sqlx::query_as("SELECT revision,active_holds,source_present,replayed FROM public.change_job_retention_hold($1,$2,$3,$4,$5,$6)")
        .bind(job_id)
        .bind(request_id)
        .bind(expected_revision)
        .bind(hold_id)
        .bind(action)
        .bind("Synthetic retention hold")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires a dedicated disposable PostgreSQL >=16 cluster and fixed OCR roles"]
async fn restricted_credentials_preserve_pipeline_audit_and_fail_closed() {
    let url = std::env::var("TEST_ADMIN_DATABASE_URL")
        .expect("TEST_ADMIN_DATABASE_URL required; never use live DB");
    let mut cluster = PgConnection::connect(&url).await.unwrap();
    let name = format!("ocr_roles_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut cluster)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url).unwrap().database(&name);
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let mut owner_admin = PgConnection::connect_with(&options).await.unwrap();
    let passwords = passwords();

    // Additive bootstrap must not silently adopt unrelated/live objects.
    sqlx::query("CREATE TABLE public.unrelated(value integer)")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::Ownership)
    ));
    sqlx::query("DROP TABLE public.unrelated")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE SCHEMA other_application; CREATE TABLE other_application.unrelated(value integer);",
    )
    .execute(&admin)
    .await
    .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::Ownership)
    ));
    sqlx::query("DROP SCHEMA other_application CASCADE")
        .execute(&admin)
        .await
        .unwrap();
    // Deterministic CREATE ROLE failure after the bound password is supplied:
    // uncommitted catalog uniqueness is invisible to the existence SELECT, but
    // blocks the actual dynamic DDL until the bootstrap lock deadline elapses.
    sqlx::query("DROP ROLE IF EXISTS ocr_backup")
        .execute(&admin)
        .await
        .unwrap();
    let mut blocker = PgConnection::connect_with(&options).await.unwrap();
    let mut blocked = blocker.begin().await.unwrap();
    sqlx::query("CREATE ROLE ocr_backup NOLOGIN")
        .execute(&mut *blocked)
        .await
        .unwrap();
    sqlx::raw_sql("SET log_statement='all'; SET log_min_error_statement='error'; SET log_min_duration_statement=0; SET log_parameter_max_length=-1; SET log_parameter_max_length_on_error=-1;")
        .execute(&mut owner_admin).await.unwrap();
    let mut sentinel_passwords = passwords.clone();
    sentinel_passwords[4] = "a03-log-sentinel-injected-failure-SECRET-1234".into();
    let failure = administration::bootstrap(&mut owner_admin, &sentinel_passwords)
        .await
        .unwrap_err();
    assert!(matches!(failure, AdminError::Database));
    assert!(!format!("{failure:?} {failure}").contains(&sentinel_passwords[4]));
    blocked.rollback().await.unwrap();
    sqlx::query("RESET ALL")
        .execute(&mut owner_admin)
        .await
        .unwrap();
    println!(
        "TRACE bootstrap failure: dynamic CREATE ROLE catalog lock timeout redacted; server-log sentinel verification requires external disposable PG log capture"
    );
    administration::bootstrap(&mut owner_admin, &passwords)
        .await
        .unwrap();
    for (create, remove) in [
        (
            "CREATE TABLE public.unrelated_owner(value integer); ALTER TABLE public.unrelated_owner OWNER TO ocr_owner",
            "DROP TABLE public.unrelated_owner",
        ),
        (
            "CREATE TYPE public.unrelated_type AS ENUM ('synthetic'); ALTER TYPE public.unrelated_type OWNER TO ocr_owner",
            "DROP TYPE public.unrelated_type",
        ),
        (
            "CREATE FUNCTION public.unrelated_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'; ALTER FUNCTION public.unrelated_function() OWNER TO ocr_owner",
            "DROP FUNCTION public.unrelated_function()",
        ),
    ] {
        sqlx::raw_sql(create).execute(&admin).await.unwrap();
        assert!(matches!(
            administration::bootstrap(&mut owner_admin, &passwords).await,
            Err(AdminError::Ownership)
        ));
        assert!(matches!(
            administration::grant(&mut owner_admin).await,
            Err(AdminError::Ownership)
        ));
        sqlx::query(remove).execute(&admin).await.unwrap();
    }
    sqlx::query("ALTER ROLE ocr_api SET statement_timeout='90s'")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::RoleCollision)
    ));
    sqlx::query("ALTER ROLE ocr_api RESET statement_timeout")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!(
        "ALTER ROLE ocr_api IN DATABASE {name} SET session_replication_role='replica'"
    ))
    .execute(&admin)
    .await
    .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::RoleCollision)
    ));
    sqlx::query(&format!("ALTER ROLE ocr_api IN DATABASE {name} RESET ALL"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!(
        "ALTER DATABASE {name} SET session_replication_role='replica'"
    ))
    .execute(&admin)
    .await
    .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::RoleCollision)
    ));
    sqlx::query(&format!("ALTER DATABASE {name} RESET ALL"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("ALTER ROLE ocr_api CREATEDB")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(
        administration::bootstrap(&mut owner_admin, &passwords).await,
        Err(AdminError::RoleCollision)
    ));
    sqlx::query("ALTER ROLE ocr_api NOCREATEDB")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("GRANT CREATE,TEMP ON DATABASE {name} TO ocr_api"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("GRANT CREATE ON SCHEMA public TO ocr_api")
        .execute(&admin)
        .await
        .unwrap();
    administration::bootstrap(&mut owner_admin, &passwords)
        .await
        .unwrap();
    println!(
        "TRACE bootstrap: unrelated schema rejected; roles created additively; replay preserved credentials"
    );

    let migrator_options = options
        .clone()
        .username(LOGIN_ROLES[0])
        .password(&passwords[0]);
    let mut migrator = PgConnection::connect_with(&migrator_options).await.unwrap();
    // Real upgrade path: the same dedicated owner/role inventory must accept
    // a migrated A03 database before migration0006 adds the retention objects.
    sqlx::query("SET ROLE ocr_owner")
        .execute(&mut migrator)
        .await
        .unwrap();
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 5)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    legacy.run(&mut migrator).await.unwrap();
    assert!(matches!(
        Store::from_pool(admin.clone()).check_schema().await,
        Err(StoreError::Schema(SchemaError::Pending(6)))
    ));
    administration::bootstrap(&mut owner_admin, &passwords)
        .await
        .unwrap();
    administration::migrate(&mut migrator).await.unwrap();
    administration::migrate(&mut migrator).await.unwrap();
    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(versions, MIGRATOR.iter().count() as i64);
    administration::grant(&mut owner_admin).await.unwrap();
    administration::grant(&mut owner_admin).await.unwrap();
    for (change, restore) in [
        (
            "ALTER TABLE public.job_review_events DISABLE TRIGGER job_review_append_only",
            "ALTER TABLE public.job_review_events ENABLE TRIGGER job_review_append_only",
        ),
        (
            "CREATE TRIGGER unexpected BEFORE INSERT ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.protect_job_review_event()",
            "DROP TRIGGER unexpected ON public.jobs",
        ),
        (
            "ALTER TABLE public.jobs ENABLE ROW LEVEL SECURITY",
            "ALTER TABLE public.jobs DISABLE ROW LEVEL SECURITY",
        ),
        (
            "CREATE POLICY unexpected ON public.jobs USING (true)",
            "DROP POLICY unexpected ON public.jobs",
        ),
        (
            "CREATE INDEX unrelated_jobs ON public.jobs(id)",
            "DROP INDEX public.unrelated_jobs",
        ),
        (
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SECURITY INVOKER",
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SECURITY DEFINER",
        ),
        (
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SET search_path=public,pg_catalog,pg_temp",
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SET search_path=pg_catalog,public,pg_temp",
        ),
        (
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SET lock_timeout='9s'",
            "ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) SET lock_timeout='2s'",
        ),
        (
            "ALTER FUNCTION public.guard_held_job_source() SECURITY DEFINER",
            "ALTER FUNCTION public.guard_held_job_source() SECURITY INVOKER",
        ),
        (
            "ALTER FUNCTION public.guard_held_job_source() RESET ALL",
            "ALTER FUNCTION public.guard_held_job_source() SET search_path=pg_catalog,pg_temp",
        ),
        (
            "CREATE FUNCTION public.change_job_retention_hold(integer) RETURNS bigint LANGUAGE plpgsql AS $$ BEGIN RETURN 1; END; $$; ALTER FUNCTION public.change_job_retention_hold(integer) OWNER TO ocr_owner",
            "DROP FUNCTION public.change_job_retention_hold(integer)",
        ),
        (
            "DROP TRIGGER jobs_retention_delete_guard ON public.jobs",
            "CREATE TRIGGER jobs_retention_delete_guard BEFORE DELETE ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.guard_job_retention_delete()",
        ),
        (
            "DROP TRIGGER job_retention_events_no_truncate ON public.job_retention_events",
            "CREATE TRIGGER job_retention_events_no_truncate BEFORE TRUNCATE ON public.job_retention_events FOR EACH STATEMENT EXECUTE FUNCTION public.protect_job_retention_event()",
        ),
        (
            "DROP TRIGGER jobs_held_source_guard ON public.jobs; CREATE TRIGGER jobs_held_source_guard BEFORE UPDATE OF result ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.guard_held_job_source()",
            "DROP TRIGGER jobs_held_source_guard ON public.jobs; CREATE TRIGGER jobs_held_source_guard BEFORE UPDATE OF input ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.guard_held_job_source()",
        ),
        (
            "DROP TRIGGER jobs_retention_truncate_guard ON public.jobs; CREATE TRIGGER jobs_retention_truncate_guard BEFORE DELETE ON public.jobs FOR EACH STATEMENT EXECUTE FUNCTION public.guard_job_retention_delete()",
            "DROP TRIGGER jobs_retention_truncate_guard ON public.jobs; CREATE TRIGGER jobs_retention_truncate_guard BEFORE TRUNCATE ON public.jobs FOR EACH STATEMENT EXECUTE FUNCTION public.guard_job_retention_delete()",
        ),
    ] {
        sqlx::raw_sql(change).execute(&admin).await.unwrap();
        assert!(matches!(
            administration::grant(&mut owner_admin).await,
            Err(AdminError::Ownership)
        ));
        sqlx::raw_sql(restore).execute(&admin).await.unwrap();
    }
    let hold_function_definition: String = sqlx::query_scalar("SELECT pg_get_functiondef('public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)'::regprocedure)")
        .fetch_one(&admin).await.unwrap();
    sqlx::raw_sql("DROP FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text); CREATE FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public,pg_temp SET lock_timeout='2s' AS $$ BEGIN RETURN 0; END; $$; ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) OWNER TO ocr_owner")
        .execute(&admin).await.unwrap();
    assert!(matches!(
        administration::grant(&mut owner_admin).await,
        Err(AdminError::Ownership)
    ));
    sqlx::query("DROP FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(&hold_function_definition)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("ALTER FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) OWNER TO ocr_owner")
        .execute(&admin).await.unwrap();
    administration::grant(&mut owner_admin).await.unwrap();
    println!(
        "TRACE dedicated database guard: unrelated objects/GUCs/RLS, retention function overload/return-type/SECURITY DEFINER/search_path(NULL included)/deadline drift, missing DELETE/TRUNCATE guards and changed trigger event/column bits rejected before mutation"
    );
    assert!(matches!(
        administration::migrate(&mut owner_admin).await,
        Err(AdminError::MigratorRequired)
    ));
    println!(
        "TRACE migrator: real0001–5 owner schema accepted by additive bootstrap, runtime check rejected pending6; dedicated login SET ROLE NOLOGIN owner; upgrade and replay produced exactly six migration rows"
    );

    let api_pool = pool(&options, LOGIN_ROLES[1], &passwords[1]).await;
    let worker_pool = pool(&options, LOGIN_ROLES[2], &passwords[2]).await;
    let maintenance = pool(&options, LOGIN_ROLES[3], &passwords[3]).await;
    let backup = pool(&options, LOGIN_ROLES[4], &passwords[4]).await;
    let api = Store::from_pool(api_pool.clone());
    let worker = Store::from_pool(worker_pool.clone());
    let mut api_connection = PgConnection::connect_with(
        &options
            .clone()
            .username(LOGIN_ROLES[1])
            .password(&passwords[1]),
    )
    .await
    .unwrap();
    assert!(matches!(
        administration::migrate(&mut api_connection).await,
        Err(AdminError::MigratorRequired)
    ));
    assert!(matches!(
        administration::migrate_dev(&mut api_connection).await,
        Err(AdminError::AdminRequired)
    ));
    drop(api_connection);
    assert!(
        PgConnection::connect_with(
            &options
                .clone()
                .username("ocr_owner")
                .password("synthetic-unused")
        )
        .await
        .is_err()
    );
    api.check_schema().await.unwrap();
    worker.check_schema().await.unwrap();
    let invoker: bool=sqlx::query_scalar("SELECT bool_and(NOT prosecdef AND proconfig=ARRAY['search_path=pg_catalog, public']) FROM pg_proc WHERE oid IN ('public.maintain_job_outbox()'::regprocedure,'public.protect_job_review_event()'::regprocedure)").fetch_one(&admin).await.unwrap();
    assert!(invoker);
    let retention_definer: bool = sqlx::query_scalar("SELECT prosecdef AND proretset AND prorettype='pg_catalog.record'::regtype AND proconfig=ARRAY['search_path=pg_catalog, public, pg_temp','lock_timeout=2s'] FROM pg_proc WHERE oid='public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)'::regprocedure")
        .fetch_one(&admin).await.unwrap();
    assert!(retention_definer);
    for role in &LOGIN_ROLES[1..] {
        let execute: bool = sqlx::query_scalar("SELECT has_function_privilege($1,'public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)','EXECUTE')")
            .bind(role).fetch_one(&admin).await.unwrap();
        assert_eq!(execute, *role == "ocr_maintenance");
        for column in [
            "retention_policy_id",
            "retention_class",
            "retain_until",
            "retention_review_revision",
            "retention_revision",
            "retention_active_holds",
        ] {
            let mutate: bool =
                sqlx::query_scalar("SELECT has_column_privilege($1,'public.jobs',$2,'UPDATE')")
                    .bind(role)
                    .bind(column)
                    .fetch_one(&admin)
                    .await
                    .unwrap();
            assert!(!mutate, "{role} must not mutate {column}");
        }
    }
    let id = Uuid::new_v4();
    let submitted = api
        .submit(NewJob {
            id,
            filename: "synthetic.pdf".into(),
            media_type: "application/pdf".into(),
            sha256: "synthetic".into(),
            input: b"synthetic source".to_vec(),
            idempotency_key: Some("restricted-credentials".into()),
            extraction_profile: "vi_rental_contract_v1".into(),
        })
        .await
        .unwrap();
    assert!(submitted.created);
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM job_outbox WHERE job_id=$1")
        .bind(id)
        .fetch_one(&worker_pool)
        .await
        .unwrap();
    assert_eq!(outbox, 1);
    let deliveries = worker.claim_outbox(10, 30).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert!(
        worker
            .mark_dispatched(id, deliveries[0].token, 60)
            .await
            .unwrap()
    );
    let lease = worker.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    assert!(worker.renew(id, lease.lease_token, 120).await.unwrap());
    let key = "a".repeat(64);
    assert_eq!(
        worker
            .save_region_checkpoint(id, lease.lease_token, &key, "synthetic text")
            .await
            .unwrap(),
        CheckpointWrite::Stored
    );
    assert_eq!(
        worker
            .region_checkpoint(id, lease.lease_token, &key)
            .await
            .unwrap(),
        Some("synthetic text".into())
    );
    // Retry reinserts/upserts the invoker outbox before a second claim.
    assert!(
        worker
            .fail(id, lease.lease_token, "synthetic retry", true, 3, 0)
            .await
            .unwrap()
    );
    let lease = worker.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    let result =
        json!({"extraction":ocr_extraction::extract(&[]),"markdown":"immutable raw","layout":[]});
    assert!(
        worker
            .complete_retaining_input(id, lease.lease_token, result.clone())
            .await
            .unwrap()
    );
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM job_outbox WHERE job_id=$1")
        .bind(id)
        .fetch_one(&worker_pool)
        .await
        .unwrap();
    assert_eq!(outbox, 0);
    let decision = ReviewRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 0,
        action: ReviewAction::Reject,
        reason: "Synthetic source review".into(),
        corrections: BTreeMap::new(),
    };
    let first = api
        .append_review(id, "synthetic-service-principal", decision.clone())
        .await
        .unwrap();
    let replay = api
        .append_review(id, "synthetic-service-principal", decision)
        .await
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(replay.revision, 1);
    assert_eq!(api.review_history(id, 0).await.unwrap().events.len(), 1);
    assert_eq!(api.get(id).await.unwrap().unwrap().result, Some(result));
    println!(
        "TRACE restricted pipeline: API submit → invoker outbox → dispatcher → lease/heartbeat → immutable checkpoint → retry outbox → complete → review + replay succeeded"
    );

    let hold_one = Uuid::new_v4();
    let hold_two = Uuid::new_v4();
    let hold_request = Uuid::new_v4();
    assert_eq!(
        change_hold(&maintenance, id, hold_request, 0, hold_one, "hold").await,
        (1, 1, true, false)
    );
    assert_eq!(
        change_hold(&maintenance, id, hold_request, 0, hold_one, "hold").await,
        (1, 1, true, true)
    );
    assert_eq!(
        change_hold(&maintenance, id, Uuid::new_v4(), 1, hold_two, "hold").await,
        (2, 2, true, false)
    );
    assert_eq!(
        change_hold(&maintenance, id, Uuid::new_v4(), 2, hold_one, "release").await,
        (3, 1, true, false)
    );
    let hold_state: (i64, i32, i64, i64) = sqlx::query_as(
        "SELECT retention_revision,retention_active_holds,
            (SELECT count(*) FROM public.job_retention_holds WHERE job_id=$1),
            (SELECT count(*) FROM public.job_retention_events WHERE job_id=$1)
        FROM public.jobs WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&maintenance)
    .await
    .unwrap();
    assert_eq!(hold_state, (3, 1, 1, 3));
    let principals: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT principal FROM public.job_retention_events WHERE job_id=$1",
    )
    .bind(id)
    .fetch_all(&maintenance)
    .await
    .unwrap();
    assert_eq!(principals, vec!["ocr_maintenance"]);
    let raw_after_hold = api.get(id).await.unwrap().unwrap().result.unwrap();
    assert_eq!(raw_after_hold["markdown"], "immutable raw");
    let held_input_error = sqlx::query("UPDATE public.jobs SET input='forged'::bytea WHERE id=$1")
        .bind(id)
        .execute(&worker_pool)
        .await
        .unwrap_err();
    assert_eq!(
        held_input_error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("P0001")
    );
    assert_eq!(
        sqlx::query_scalar::<_, Vec<u8>>("SELECT input FROM public.jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&worker_pool)
            .await
            .unwrap(),
        b"synthetic source"
    );
    println!(
        "TRACE restricted retention: maintenance-only typed owner function hold/replay/second-hold/release → revisions1/1/2/3, active1, state1, audit3, actor=session_user(ocr_maintenance); raw/result/source unchanged; old worker input mutation blocked by locked-row guard"
    );

    for restricted in [&api_pool, &worker_pool, &maintenance, &backup] {
        for sql in [
            "CREATE ROLE ocr_forbidden",
            "CREATE TABLE public.forbidden(id integer)",
            "CREATE SCHEMA forbidden",
            "CREATE TEMP TABLE forbidden(id integer)",
            "ALTER TABLE public.job_review_events DISABLE TRIGGER job_review_append_only",
            "UPDATE public.job_review_events SET reason='forged'",
            "DELETE FROM public.job_review_events",
            "TRUNCATE public.job_review_events",
            "DELETE FROM public.jobs",
            "TRUNCATE public.jobs",
            "UPDATE public._sqlx_migrations SET success=false",
            "SET ROLE ocr_owner",
            "UPDATE public.jobs SET retention_active_holds=0",
            "UPDATE public.jobs SET retention_revision=0",
            "UPDATE public.jobs SET retain_until=clock_timestamp()",
            "INSERT INTO public.job_retention_holds(job_id,hold_id,created_at) VALUES(NULL,NULL,clock_timestamp())",
            "UPDATE public.job_retention_holds SET created_at=clock_timestamp()",
            "DELETE FROM public.job_retention_holds",
            "TRUNCATE public.job_retention_holds",
            "INSERT INTO public.job_retention_events(job_id) VALUES(NULL)",
            "UPDATE public.job_retention_events SET reason='forged'",
            "DELETE FROM public.job_retention_events",
            "TRUNCATE public.job_retention_events",
        ] {
            denied(restricted, sql).await;
        }
    }
    denied(&api_pool, "UPDATE public.jobs SET result='{}'").await;
    for caller in [&api_pool, &worker_pool, &backup] {
        denied(
            caller,
            "SELECT * FROM public.change_job_retention_hold(NULL,NULL,NULL,NULL,NULL,NULL)",
        )
        .await;
    }
    for caller in [&api_pool, &worker_pool] {
        denied(caller, "SELECT * FROM public.job_retention_holds").await;
        denied(caller, "SELECT * FROM public.job_retention_events").await;
    }
    denied(
        &worker_pool,
        "INSERT INTO public.job_review_events(job_id) VALUES(NULL)",
    )
    .await;
    assert!(matches!(
        worker.prune(0).await,
        Err(StoreError::Database(_))
    ));
    for reader in [&maintenance, &backup] {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM public.job_review_events")
            .fetch_one(reader)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
    // The append-only trigger also rejects an administrator's direct edit,
    // independently from the runtime ACL denial tested above.
    let error = sqlx::query("UPDATE public.job_review_events SET reason='forged'")
        .execute(&admin)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("P0001")
    );
    for sql in [
        "DELETE FROM public.jobs",
        "TRUNCATE public.jobs CASCADE",
        "UPDATE public.job_retention_events SET reason='forged'",
        "DELETE FROM public.job_retention_events",
        "TRUNCATE public.job_retention_events",
    ] {
        let error = sqlx::query(sql).execute(&admin).await.unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("P0001"),
            "{sql}"
        );
    }
    let error = sqlx::query("DELETE FROM public.job_review_events")
        .execute(&admin)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("P0001")
    );
    println!(
        "TRACE privilege negatives: all four restricted logins denied DDL/temp/trigger-disable/jobs-delete/retention-column-state-audit mutation; API/worker/backup denied hold function; API/worker denied hold/event reads; admin DELETE/jobs-TRUNCATE-CASCADE and retention-audit UPDATE/DELETE/TRUNCATE blocked by DB guards; review invoker audit rejected direct admin edit"
    );

    sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=1")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(
        api.check_schema().await,
        Err(StoreError::Schema(SchemaError::Dirty(1)))
    ));
    sqlx::query(
        "UPDATE public._sqlx_migrations SET success=true,checksum='\\x00'::bytea WHERE version=1",
    )
    .execute(&admin)
    .await
    .unwrap();
    assert!(matches!(
        api.check_schema().await,
        Err(StoreError::Schema(SchemaError::Checksum(1)))
    ));
    sqlx::query("UPDATE public._sqlx_migrations SET checksum=$1 WHERE version=1")
        .bind(
            MIGRATOR
                .iter()
                .find(|m| m.version == 1)
                .unwrap()
                .checksum
                .as_ref(),
        )
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO public._sqlx_migrations(version,description,success,checksum,execution_time) VALUES(999,'unsupported',true,'\\x00'::bytea,0)").execute(&admin).await.unwrap();
    assert!(matches!(
        api.check_schema().await,
        Err(StoreError::Schema(SchemaError::Unsupported(999)))
    ));
    sqlx::query("DELETE FROM public._sqlx_migrations WHERE version=999")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("UPDATE public._sqlx_migrations SET version=998 WHERE version=6")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("DELETE FROM public._sqlx_migrations WHERE version=998")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(
        api.check_schema().await,
        Err(StoreError::Schema(SchemaError::Pending(6)))
    ));
    let m = MIGRATOR.iter().find(|m| m.version == 6).unwrap();
    sqlx::query("INSERT INTO public._sqlx_migrations(version,description,success,checksum,execution_time) VALUES($1,$2,true,$3,0)").bind(m.version).bind(m.description.as_ref()).bind(m.checksum.as_ref()).execute(&admin).await.unwrap();
    api.check_schema().await.unwrap();
    // Grants repair broad/column ACL drift without altering application data.
    sqlx::query("GRANT UPDATE(reason) ON public.job_review_events TO ocr_api,PUBLIC")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql("GRANT UPDATE(retention_revision,retention_active_holds) ON public.jobs TO PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup; GRANT DELETE,TRUNCATE ON public.jobs TO PUBLIC,ocr_maintenance; GRANT INSERT(hold_id) ON public.job_retention_holds TO PUBLIC,ocr_api; GRANT UPDATE(reason) ON public.job_retention_events TO PUBLIC,ocr_maintenance; GRANT EXECUTE ON FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) TO PUBLIC")
        .execute(&admin).await.unwrap();
    administration::grant(&mut owner_admin).await.unwrap();
    denied(
        &api_pool,
        "UPDATE public.job_review_events SET reason='forged'",
    )
    .await;
    for caller in [&api_pool, &worker_pool, &maintenance, &backup] {
        for sql in [
            "UPDATE public.jobs SET retention_active_holds=0",
            "UPDATE public.jobs SET retention_revision=0",
            "DELETE FROM public.jobs",
            "TRUNCATE public.jobs",
            "INSERT INTO public.job_retention_holds(hold_id) VALUES(NULL)",
            "UPDATE public.job_retention_events SET reason='forged'",
        ] {
            denied(caller, sql).await;
        }
    }
    for caller in [&api_pool, &worker_pool, &backup] {
        denied(
            caller,
            "SELECT * FROM public.change_job_retention_hold(NULL,NULL,NULL,NULL,NULL,NULL)",
        )
        .await;
    }
    let surviving_rows: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM public.jobs WHERE id=$1),
            (SELECT count(*) FROM public.job_retention_holds WHERE job_id=$1),
            (SELECT count(*) FROM public.job_retention_events WHERE job_id=$1),
            (SELECT count(*) FROM public.job_review_events WHERE job_id=$1),
            (SELECT count(*) FROM public.region_checkpoints WHERE job_id=$1),
            (SELECT count(*) FROM public.job_outbox WHERE job_id=$1)",
    )
    .bind(id)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(surviving_rows, (1, 1, 3, 1, 1, 0));
    assert_eq!(
        api.get(id).await.unwrap().unwrap().result,
        Some(raw_after_hold)
    );
    println!(
        "TRACE schema negatives: dirty/checksum/unsupported/pending6 rejected read-only; exact six-row history accepted; grants removed PUBLIC/direct retention table+column ACL and owner-function EXECUTE drift; surviving jobs/holds/retention-audit/review/checkpoint/outbox=1/1/3/1/1/0, raw unchanged"
    );

    for restricted in [api_pool, worker_pool, maintenance, backup] {
        restricted.close().await;
    }
    drop(api);
    drop(worker);
    drop(migrator);
    drop(owner_admin);
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&mut cluster)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated disposable PostgreSQL"]
async fn missing_schema_check_performs_no_ddl() {
    let url = std::env::var("TEST_ADMIN_DATABASE_URL").expect("TEST_ADMIN_DATABASE_URL required");
    let mut connection = PgConnection::connect(&url).await.unwrap();
    let name = format!("ocr_schema_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {name}"))
        .execute(&mut connection)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", name.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    assert!(matches!(
        Store::from_pool(pool.clone()).check_schema().await,
        Err(StoreError::Schema(SchemaError::Unavailable))
    ));
    let created: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!created);
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {name}"))
        .execute(&mut connection)
        .await
        .unwrap();
}
