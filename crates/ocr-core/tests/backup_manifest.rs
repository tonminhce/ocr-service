//! All databases below are generated children of the disposable TEST_DATABASE_URL.
//! These tests use binary COPY for content replication; the packaged rehearsal
//! separately proves standard pg_dump/pg_restore, encryption and process exits.
use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use ocr_core::{
    backup::{BackupContent, BackupError, content_manifest},
    domain::NewJob,
    postgres::safe_options,
    retention::{RetentionAction, RetentionHoldRequest},
    review::{ReviewAction, ReviewRequest},
    store::Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

const TABLES: &[&str] = &[
    "jobs",
    "job_outbox",
    "job_review_events",
    "region_checkpoints",
    "job_retention_holds",
    "job_retention_events",
    "_sqlx_migrations",
];
const PRIVATE_TEXT: &str = "private-contract-name-and-identity-sentinel";

struct Database {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    name: String,
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
        let name = format!("ocr_backup_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name} TEMPLATE template0"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options.database(&name).application_name(&name))
            .await
            .unwrap();
        let store = Store::from_pool(pool.clone());
        store.migrate().await.unwrap();
        Self {
            admin,
            pool,
            store,
            name,
        }
    }
    async fn job(&self, key: &str) -> Uuid {
        let id = Uuid::new_v4();
        let bytes = format!("%PDF-{PRIVATE_TEXT}-{key}").into_bytes();
        self.store
            .submit(NewJob {
                id,
                filename: format!("{PRIVATE_TEXT}-{key}.pdf"),
                media_type: "application/pdf".into(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                input: bytes,
                idempotency_key: Some(format!("private-key-{key}")),
                extraction_profile: "vi_rental_contract_v1".into(),
            })
            .await
            .unwrap();
        id
    }
    async fn seed(&self) -> (Uuid, Uuid, Uuid) {
        let completed = self.job("completed").await;
        let lease = self
            .store
            .claim_by_id(completed, 120, 3)
            .await
            .unwrap()
            .unwrap();
        self.store
            .save_region_checkpoint(completed, lease.lease_token, &"a".repeat(64), PRIVATE_TEXT)
            .await
            .unwrap();
        self.store
            .change_retention_hold(RetentionHoldRequest {
                job_id: completed,
                request_id: Uuid::new_v4(),
                expected_revision: 0,
                hold_id: Uuid::new_v4(),
                action: RetentionAction::Hold,
                reason: PRIVATE_TEXT.into(),
            })
            .await
            .unwrap();
        assert!(self.store.complete_retaining_input(completed,lease.lease_token,
            json!({"extraction":ocr_extraction::extract(&[]),"markdown":PRIVATE_TEXT,"layout":[]})).await.unwrap());
        self.store
            .append_review(
                completed,
                "fixture-service-principal",
                ReviewRequest {
                    request_id: Uuid::new_v4(),
                    expected_revision: 0,
                    action: ReviewAction::Reject,
                    reason: PRIVATE_TEXT.into(),
                    corrections: BTreeMap::new(),
                },
            )
            .await
            .unwrap();
        sqlx::query("UPDATE jobs SET retention_policy_id='fixture_policy',retention_class='fixture_class',retain_until=clock_timestamp()+INTERVAL '1 day',retention_review_revision=1 WHERE id=$1")
            .bind(completed).execute(&self.pool).await.unwrap();
        let queued = self.job("queued").await;
        sqlx::query("UPDATE job_outbox SET dispatch_token=$2,dispatch_lease_expires_at=clock_timestamp()+INTERVAL '10 seconds' WHERE job_id=$1")
            .bind(queued).bind(Uuid::new_v4()).execute(&self.pool).await.unwrap();
        let processing = self.job("processing").await;
        self.store
            .claim_by_id(processing, 120, 3)
            .await
            .unwrap()
            .unwrap();
        (completed, queued, processing)
    }
    async fn manifest(&self) -> BackupContent {
        let mut tx = self.pool.begin().await.unwrap();
        readonly(&mut tx).await;
        let manifest = content_manifest(&mut tx).await.unwrap();
        tx.rollback().await.unwrap();
        manifest
    }
    async fn reset_restore_fixture(&self) {
        // Reset this generated destination fixture, never a source/live schema.
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
            .execute(&self.pool)
            .await
            .unwrap();
        self.store.migrate().await.unwrap();
    }
    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {}", self.name))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}
async fn readonly(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) {
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut **tx)
        .await
        .unwrap();
}

struct Mutation<'a> {
    table: &'a str,
    column: &'a str,
    expression: String,
}

async fn copy_contents(
    source: &mut PgConnection,
    destination: &Database,
    mutation: Option<&Mutation<'_>>,
    remap_job: Option<(Uuid, Uuid)>,
) -> Result<(), sqlx::Error> {
    let mut tx = destination.pool.begin().await?;
    for table in TABLES {
        if *table == "job_outbox" {
            // Jobs INSERT fired the normal outbox trigger. Replace only this
            // disposable destination's derived notifications with exact COPY.
            sqlx::query("DELETE FROM public.job_outbox")
                .execute(&mut *tx)
                .await?;
        }
        if *table == "_sqlx_migrations" {
            sqlx::query("TRUNCATE public._sqlx_migrations")
                .execute(&mut *tx)
                .await?;
        }
        let columns:Vec<String>=sqlx::query_scalar("SELECT a.attname::text FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass($1) AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum")
            .bind(format!("public.{table}")).fetch_all(&mut *source).await?;
        let projections: Vec<String> = columns
            .iter()
            .map(|column| {
                if let Some((old, new)) = remap_job
                    && ((*table == "jobs" && column == "id")
                        || (*table != "_sqlx_migrations" && column == "job_id"))
                {
                    return format!(
                        "CASE WHEN {column}='{old}'::uuid THEN '{new}'::uuid ELSE {column} END"
                    );
                }
                if let Some(change) = mutation
                    && change.table == *table
                    && change.column == column
                {
                    return change.expression.clone();
                }
                column.clone()
            })
            .collect();
        // Table/column/expression strings are fixed test code or server catalog
        // identifiers already checked by the source's successful manifest.
        let query = format!(
            "COPY (SELECT {} FROM public.{table}) TO STDOUT WITH (FORMAT binary)",
            projections.join(",")
        );
        let mut stream = source.copy_out_raw(&query).await?;
        let mut sink = tx
            .copy_in_raw(&format!(
                "COPY public.{table} FROM STDIN WITH (FORMAT binary)"
            ))
            .await?;
        while let Some(chunk) = stream.try_next().await? {
            sink.send(chunk).await?;
        }
        sink.finish().await?;
    }
    tx.commit().await
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL18 with CREATE DATABASE privileges"]
async fn manifest_requires_explicit_readonly_repeatable_transaction_and_stable_session_formatting()
{
    let db = Database::new().await;
    db.seed().await;
    let mut connection = db.pool.acquire().await.unwrap();
    assert_eq!(
        content_manifest(&mut connection).await.unwrap_err(),
        BackupError::InvalidTransaction
    );
    sqlx::raw_sql("SET default_transaction_isolation='repeatable read'; SET default_transaction_read_only=on;")
        .execute(&mut *connection).await.unwrap();
    assert_eq!(
        content_manifest(&mut connection).await.unwrap_err(),
        BackupError::InvalidTransaction
    );
    sqlx::raw_sql("RESET default_transaction_isolation; RESET default_transaction_read_only;")
        .execute(&mut *connection)
        .await
        .unwrap();
    let mut tx = connection.begin().await.unwrap();
    assert_eq!(
        content_manifest(&mut tx).await.unwrap_err(),
        BackupError::InvalidTransaction
    );
    tx.rollback().await.unwrap();
    let mut tx = connection.begin().await.unwrap();
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        content_manifest(&mut tx).await.unwrap_err(),
        BackupError::InvalidTransaction
    );
    tx.rollback().await.unwrap();
    sqlx::raw_sql("SET TimeZone='Pacific/Honolulu'; SET DateStyle='SQL, DMY'; SET bytea_output='escape'; SET extra_float_digits=0;")
        .execute(&mut *connection).await.unwrap();
    let expected = db.manifest().await;
    let mut tx = connection.begin().await.unwrap();
    readonly(&mut tx).await;
    let actual = content_manifest(&mut tx).await.unwrap();
    actual.validate().unwrap();
    assert_eq!(expected, actual);
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SHOW TimeZone")
            .fetch_one(&mut *connection)
            .await
            .unwrap(),
        "Pacific/Honolulu"
    );
    drop(connection);
    let encoded = serde_json::to_string(&actual).unwrap();
    for private in [
        PRIVATE_TEXT,
        "private-key-",
        "fixture-service-principal",
        "%PDF-",
    ] {
        assert!(!encoded.contains(private));
    }
    assert!(encoded.len() < 4096);
    assert_eq!(actual.tables.len(), 7);
    assert_eq!(actual.functions.len(), 6);
    eprintln!(
        "backup trace contract: autocommit_defaults=refused rw_or_read_committed=refused caller_RR_RO=required UTC_ISO_hex=stable local_settings_rollback=verified tables=7 functions=6 document_text_or_identity_output=none"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL18 with CREATE DATABASE privileges"]
async fn exported_snapshot_keeps_manifest_consistent_across_concurrent_committed_writer() {
    let db = Database::new().await;
    let (completed, _, _) = db.seed().await;
    let mut exporter = db.pool.begin().await.unwrap();
    readonly(&mut exporter).await;
    let snapshot: String = sqlx::query_scalar("SELECT pg_catalog.pg_export_snapshot()")
        .fetch_one(&mut *exporter)
        .await
        .unwrap();
    assert!(snapshot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
    let baseline = content_manifest(&mut exporter).await.unwrap();
    db.job("committed-after-snapshot").await;
    sqlx::query("UPDATE jobs SET filename=filename||'-concurrent-change' WHERE id=$1")
        .bind(completed)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut importer = db.pool.begin().await.unwrap();
    readonly(&mut importer).await;
    sqlx::query(&format!("SET TRANSACTION SNAPSHOT '{snapshot}'"))
        .execute(&mut *importer)
        .await
        .unwrap();
    let imported = content_manifest(&mut importer).await.unwrap();
    assert_eq!(baseline, imported);
    importer.rollback().await.unwrap();
    assert_eq!(baseline, content_manifest(&mut exporter).await.unwrap());
    exporter.rollback().await.unwrap();
    assert_ne!(baseline, db.manifest().await);
    eprintln!(
        "backup trace snapshot: exporter_RR_RO=true concurrent_insert_and_metadata_update=committed imported_same_snapshot=identical exporter_repeat=identical fresh_snapshot=changed"
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL18 with CREATE DATABASE privileges"]
async fn logical_copy_preserves_every_table_and_function_digest_before_runtime_reconciliation() {
    let source = Database::new().await;
    let ids = source.seed().await;
    let destination = Database::new().await;
    let mut tx = source.pool.begin().await.unwrap();
    readonly(&mut tx).await;
    let expected = content_manifest(&mut tx).await.unwrap();
    copy_contents(&mut tx, &destination, None, None)
        .await
        .unwrap();
    assert_eq!(expected, destination.manifest().await);
    let encoded = serde_json::to_string(&expected).unwrap();
    for id in [ids.0, ids.1, ids.2] {
        assert!(!encoded.contains(&id.to_string()));
    }
    assert!(expected.tables.iter().all(|t| t.rows > 0));
    assert_eq!(
        expected
            .tables
            .iter()
            .find(|t| t.table == "job_outbox")
            .unwrap()
            .rows,
        2
    );
    tx.rollback().await.unwrap();
    eprintln!(
        "backup trace logical_copy: all7tables=exact all6function_defs_and_owner=exact active_processing_and_dispatch_leases=preserved raw_source_result_review_checkpoint_holds_audit_history=preserved runtime_not_started=true no_per_job_identifiers=true"
    );
    source.cleanup().await;
    destination.cleanup().await;
}

fn expression(table: &str, column: &str, completed: Uuid, queued: Uuid) -> String {
    let fresh = Uuid::new_v4();
    match column {
        "job_id" => format!(
            "CASE WHEN job_id='{queued}'::uuid THEN '{completed}'::uuid ELSE '{queued}'::uuid END"
        ),
        "request_id" | "hold_id" => format!("'{fresh}'::uuid"),
        "lease_token" | "dispatch_token" => {
            format!("CASE WHEN {column} IS NULL THEN NULL ELSE '{fresh}'::uuid END")
        }
        "available_at"
        | "created_at"
        | "updated_at"
        | "retain_until"
        | "lease_expires_at"
        | "next_dispatch_at"
        | "dispatch_lease_expires_at"
        | "installed_on" => format!("{column}+INTERVAL '1 microsecond'"),
        "filename" | "reason" | "principal" | "description" | "content" => {
            format!("{column}||'-changed'")
        }
        "media_type" => "'application/octet-stream'::text".into(),
        "sha256" | "content_sha256" => format!("'{}'::text", "f".repeat(64)),
        "input" => "input||decode('ff','hex')".into(),
        "idempotency_key" => "COALESCE(idempotency_key,id::text)||'-changed'".into(),
        "error" => "COALESCE(error,'')||'-changed'".into(),
        "result" | "request" | "snapshot" => {
            format!("{column}||'{{\"backup_test_changed\":true}}'::jsonb")
        }
        "extraction_profile" => {
            "CASE WHEN extraction_profile='none' THEN 'vi_rental_contract_v1' ELSE 'none' END"
                .into()
        }
        "retention_policy_id" | "retention_class" => {
            format!("CASE WHEN {column} IS NULL THEN NULL ELSE 'fixture_changed' END")
        }
        "status" if table == "jobs" => {
            "CASE WHEN status='succeeded' THEN 'failed' ELSE status END".into()
        }
        "status" => "'needs_review'::text".into(),
        "action" if table == "job_review_events" => "'reopen'::text".into(),
        "action" => "'release'::text".into(),
        "cache_key" => format!("'{}'::text", "b".repeat(64)),
        "source_present" | "success" => format!("NOT {column}"),
        "checksum" => "checksum||decode('ff','hex')".into(),
        "version" => "version+1000".into(),
        "revision"
        | "expected_revision"
        | "attempts"
        | "retention_review_revision"
        | "retention_revision"
        | "retention_active_holds"
        | "execution_time" => format!("{column}+1"),
        "active_holds" => "active_holds-1".into(),
        _ => panic!("new persisted column needs a meaningful mutation: {table}.{column}"),
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL18 with CREATE DATABASE privileges"]
async fn every_persisted_column_is_covered_and_migration_identity_drift_fails_closed() {
    let source = Database::new().await;
    let (completed, queued, _) = source.seed().await;
    let destination = Database::new().await;
    let mut snapshot = source.pool.begin().await.unwrap();
    readonly(&mut snapshot).await;
    let baseline = content_manifest(&mut snapshot).await.unwrap();
    let mut checked = 0;
    for table in TABLES {
        let columns:Vec<String>=sqlx::query_scalar("SELECT a.attname::text FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass($1) AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum")
            .bind(format!("public.{table}")).fetch_all(&mut *snapshot).await.unwrap();
        for column in columns {
            destination.reset_restore_fixture().await;
            let mut remap = None;
            let change = if *table == "jobs" && column == "id" {
                remap = Some((completed, Uuid::new_v4()));
                None
            } else {
                Some(Mutation {
                    table,
                    column: &column,
                    expression: expression(table, &column, completed, queued),
                })
            };
            copy_contents(&mut snapshot, &destination, change.as_ref(), remap)
                .await
                .unwrap();
            let mut tx = destination.pool.begin().await.unwrap();
            readonly(&mut tx).await;
            let result = content_manifest(&mut tx).await;
            tx.rollback().await.unwrap();
            if *table == "_sqlx_migrations"
                && matches!(column.as_str(), "version" | "success" | "checksum")
            {
                assert_eq!(
                    result.unwrap_err(),
                    BackupError::SchemaMismatch,
                    "{table}.{column}"
                );
            } else {
                let changed = result.unwrap();
                let original = baseline.tables.iter().find(|t| t.table == *table).unwrap();
                let changed = changed.tables.iter().find(|t| t.table == *table).unwrap();
                assert_eq!(
                    original.rows, changed.rows,
                    "{table}.{column} must preserve row count"
                );
                assert_ne!(
                    original.sha256, changed.sha256,
                    "missing digest coverage: {table}.{column}"
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 61);
    snapshot.rollback().await.unwrap();
    eprintln!(
        "backup trace column_coverage: persisted_columns={checked} same_row_count_payload_mutations=detected immutable_audit_changes=inserted_only_during_copy no_guard_disabled=true migration_version_checksum_success=fail_closed"
    );
    source.cleanup().await;
    destination.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL18 with CREATE DATABASE privileges"]
async fn function_body_security_owner_drift_and_unknown_catalog_objects_are_detected() {
    let db = Database::new().await;
    db.seed().await;
    let initial = db.manifest().await;
    sqlx::query("ALTER FUNCTION public.guard_held_job_source() SET lock_timeout='3s'")
        .execute(&db.pool)
        .await
        .unwrap();
    let changed = db.manifest().await;
    assert_eq!(initial.tables, changed.tables);
    assert_ne!(initial.functions, changed.functions);
    sqlx::query("ALTER FUNCTION public.guard_held_job_source() OWNER TO pg_database_owner")
        .execute(&db.pool)
        .await
        .unwrap();
    let owner_changed = db.manifest().await;
    assert_ne!(changed.functions, owner_changed.functions);
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION public.guard_job_retention_delete() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER SET search_path=pg_catalog,pg_temp AS $$ BEGIN PERFORM 1; RAISE EXCEPTION USING ERRCODE='P0001',MESSAGE='retention_apply_disabled'; END; $$;")
        .execute(&db.pool).await.unwrap();
    let body_changed = db.manifest().await;
    assert_eq!(owner_changed.tables, body_changed.tables);
    let mut body_changes = 0;
    for (before, after) in owner_changed.functions.iter().zip(&body_changed.functions) {
        assert_eq!(before.function, after.function);
        if before.function == "public.guard_job_retention_delete()" {
            assert_ne!(before.sha256, after.sha256);
            body_changes += 1;
        } else {
            assert_eq!(before.sha256, after.sha256);
        }
    }
    assert_eq!(body_changes, 1);
    for (create, drop) in [
        (
            "CREATE FUNCTION public.guard_held_job_source(integer) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
            "DROP FUNCTION public.guard_held_job_source(integer)",
        ),
        (
            "CREATE TABLE public.unrecognized_table(id integer)",
            "DROP TABLE public.unrecognized_table",
        ),
        (
            "ALTER TABLE public.jobs ADD COLUMN unrecognized_column text",
            "ALTER TABLE public.jobs DROP COLUMN unrecognized_column",
        ),
        (
            "SELECT pg_catalog.lo_create(0)",
            "SELECT pg_catalog.lo_unlink(oid) FROM pg_catalog.pg_largeobject_metadata",
        ),
        (
            "CREATE LANGUAGE ocr_backup_unsupported_language HANDLER plpgsql_call_handler",
            "DROP LANGUAGE ocr_backup_unsupported_language",
        ),
        (
            "CREATE PUBLICATION ocr_backup_unsupported_publication FOR TABLE public.jobs",
            "DROP PUBLICATION ocr_backup_unsupported_publication",
        ),
        (
            "CREATE SUBSCRIPTION ocr_backup_unsupported_subscription CONNECTION 'host=127.0.0.1 port=1 dbname=unused' PUBLICATION unused WITH(connect=false,create_slot=false,enabled=false,slot_name=NONE)",
            "DROP SUBSCRIPTION ocr_backup_unsupported_subscription",
        ),
    ] {
        sqlx::query(create).execute(&db.pool).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        readonly(&mut tx).await;
        assert_eq!(
            content_manifest(&mut tx).await.unwrap_err(),
            BackupError::SchemaMismatch
        );
        tx.rollback().await.unwrap();
        sqlx::query(drop).execute(&db.pool).await.unwrap();
    }
    eprintln!(
        "backup trace function_catalog: function_config_body_owner_changes=detected unknown_overload_table_column_largeobject_language_publication_or_subscription=fail_closed migration_history_alone_not_trusted_attestation=true"
    );
    db.cleanup().await;
}
