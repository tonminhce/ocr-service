//! Snapshot-bound logical content verification for the local restore rehearsal.
//!
//! The caller owns an explicit SQLx REPEATABLE READ, READ ONLY transaction. It
//! must keep that transaction/snapshot alive for the associated dump and roll
//! back on any error. This module never exports documents, per-row identifiers,
//! or per-row fingerprints in its returned manifest, and performs no writes.
//!
//! Every table root uses SHA-256 over this byte framing, in order:
//! `SCHEMA_VERSION || NUL || table || NUL`, then `0x01 || row_digest[32]`
//! for each row in the specified primary-key order, then `0xff || rows:u64be`.
//! Row digests use PostgreSQL 18 JSONB text under UTC/ISO/hex settings. Jobs
//! hash source bytes and JSONB results separately, avoiding a giant hex-encoded
//! copy of the original input inside the row's JSON representation. This is a
//! versioned, PostgreSQL-18-only logical digest, not a physical storage hash.
use std::time::Duration;

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use thiserror::Error;

use crate::schema::MIGRATOR;

pub const SCHEMA_VERSION: &str = "ocr-backup-content/1.0.0";
pub const MAX_ROWS_PER_TABLE: u64 = 10_000_000;
pub const MANIFEST_DEADLINE: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupTableDigest {
    pub table: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupFunctionDigest {
    pub function: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupContent {
    pub schema_version: String,
    pub postgres_major: u32,
    pub tables: Vec<BackupTableDigest>,
    pub functions: Vec<BackupFunctionDigest>,
}

impl BackupContent {
    /// Validate an externally decoded manifest before comparing it to the DB.
    pub fn validate(&self) -> Result<(), BackupError> {
        if self.schema_version != SCHEMA_VERSION
            || self.postgres_major != 18
            || self.tables.len() != TABLES.len()
            || self.tables.iter().zip(TABLES).any(|(actual, expected)| {
                actual.table != expected.table
                    || actual.rows > MAX_ROWS_PER_TABLE
                    || actual.sha256.len() != 64
                    || !actual
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self.functions.len() != FUNCTIONS.len()
            || self
                .functions
                .iter()
                .zip(FUNCTIONS)
                .any(|(actual, expected)| {
                    actual.function != *expected || !valid_hash(&actual.sha256)
                })
        {
            return Err(BackupError::InvalidManifest);
        }
        Ok(())
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Error variants intentionally discard server messages, SQLx diagnostics and
/// connection configuration. The caller must roll back/drop after any failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupError {
    #[error("backup content requires an explicit SQLx REPEATABLE READ READ ONLY transaction")]
    InvalidTransaction,
    #[error("backup content supports PostgreSQL 18 only")]
    UnsupportedPostgres,
    #[error("backup schema or migration history differs from this binary")]
    SchemaMismatch,
    #[error("backup content row budget exceeded")]
    RowLimit,
    #[error("backup content deadline elapsed")]
    Deadline,
    #[error("backup row digest has an invalid shape")]
    InvalidDigest,
    #[error("backup content manifest has an invalid shape")]
    InvalidManifest,
    #[error("backup content database operation failed (details redacted)")]
    Database,
}

struct Table {
    table: &'static str,
    columns: &'static [&'static str],
    query: &'static str,
}

const TABLES: &[Table] = &[
    Table {
        table: "jobs",
        columns: &[
            "id",
            "filename",
            "media_type",
            "sha256",
            "input",
            "idempotency_key",
            "status",
            "attempts",
            "available_at",
            "lease_token",
            "lease_expires_at",
            "result",
            "error",
            "created_at",
            "updated_at",
            "extraction_profile",
            "retention_policy_id",
            "retention_class",
            "retain_until",
            "retention_review_revision",
            "retention_revision",
            "retention_active_holds",
        ],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.jsonb_build_array(\
            t.id,t.filename,t.media_type,t.sha256,\
            pg_catalog.encode(pg_catalog.sha256(t.input),'hex'),\
            t.idempotency_key,t.status,t.attempts,t.available_at,t.lease_token,t.lease_expires_at,\
            CASE WHEN t.result IS NULL THEN NULL ELSE pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(t.result::text,'UTF8')),'hex') END,\
            t.error,t.created_at,t.updated_at,t.extraction_profile,t.retention_policy_id,t.retention_class,\
            t.retain_until,t.retention_review_revision,t.retention_revision,t.retention_active_holds\
            )::text,'UTF8')) AS digest FROM public.jobs t ORDER BY t.id",
    },
    Table {
        table: "job_outbox",
        columns: &[
            "job_id",
            "next_dispatch_at",
            "dispatch_token",
            "dispatch_lease_expires_at",
        ],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public.job_outbox t ORDER BY t.job_id",
    },
    Table {
        table: "job_review_events",
        columns: &[
            "job_id",
            "revision",
            "request_id",
            "principal",
            "action",
            "reason",
            "request",
            "snapshot",
            "status",
            "created_at",
        ],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public.job_review_events t ORDER BY t.job_id,t.revision",
    },
    Table {
        table: "region_checkpoints",
        columns: &[
            "job_id",
            "cache_key",
            "content",
            "content_sha256",
            "created_at",
        ],
        // Text order is explicit, independent of the destination DB's locale.
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public.region_checkpoints t ORDER BY t.job_id,t.cache_key COLLATE pg_catalog.\"C\"",
    },
    Table {
        table: "job_retention_holds",
        columns: &["job_id", "hold_id", "created_at"],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public.job_retention_holds t ORDER BY t.job_id,t.hold_id",
    },
    Table {
        table: "job_retention_events",
        columns: &[
            "job_id",
            "revision",
            "request_id",
            "expected_revision",
            "hold_id",
            "action",
            "reason",
            "principal",
            "active_holds",
            "source_present",
            "created_at",
        ],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public.job_retention_events t ORDER BY t.job_id,t.revision",
    },
    Table {
        table: "_sqlx_migrations",
        columns: &[
            "version",
            "description",
            "installed_on",
            "success",
            "checksum",
            "execution_time",
        ],
        query: "SELECT pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(t)::text,'UTF8')) AS digest FROM public._sqlx_migrations t ORDER BY t.version",
    },
];

const FUNCTIONS: &[&str] = &[
    "public.maintain_job_outbox()",
    "public.protect_job_review_event()",
    "public.guard_job_retention_delete()",
    "public.guard_held_job_source()",
    "public.protect_job_retention_event()",
    "public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)",
];

// Names/kinds match the immutable migrations and supported additive0006. This
// prevents a successful-looking content manifest that silently omits a newly
// added public relation or arbitrary schema object. Catalog ownership/security
// policy enforcement is still the administrator's separate inventory gate.
const INDEXES: &[&str] = &[
    "jobs_pkey",
    "jobs_idempotency_key_key",
    "jobs_queued_due_idx",
    "jobs_processing_expiry_idx",
    "jobs_terminal_retention_idx",
    "jobs_retention_scan_idx",
    "job_outbox_pkey",
    "job_outbox_dispatch_due_idx",
    "job_review_events_pkey",
    "job_review_events_job_id_request_id_key",
    "region_checkpoints_pkey",
    "_sqlx_migrations_pkey",
    "job_retention_holds_pkey",
    "job_retention_events_pkey",
    "job_retention_events_job_id_request_id_key",
];

const TRIGGERS: &[&str] = &[
    "jobs_maintain_outbox",
    "job_review_append_only",
    "jobs_retention_delete_guard",
    "jobs_retention_truncate_guard",
    "jobs_held_source_guard",
    "job_retention_events_append_only",
    "job_retention_events_no_truncate",
];

/// Build all seven content roots from the caller's single existing snapshot.
/// Fixed budgets are for the isolated rehearsal: failures return no partial
/// manifest. Streaming is O(1) in table row count on the client; PostgreSQL still
/// reads/hashes all included logical bytes and may sort checkpoint keys.
pub async fn content_manifest(connection: &mut PgConnection) -> Result<BackupContent, BackupError> {
    if !connection.is_in_transaction() {
        return Err(BackupError::InvalidTransaction);
    }
    tokio::time::timeout(MANIFEST_DEADLINE, manifest_inner(connection))
        .await
        .map_err(|_| BackupError::Deadline)?
}

async fn manifest_inner(connection: &mut PgConnection) -> Result<BackupContent, BackupError> {
    let transaction_ok: bool = sqlx::query_scalar("SELECT pg_catalog.current_setting('transaction_isolation')='repeatable read' AND pg_catalog.current_setting('transaction_read_only')='on'")
        .fetch_one(&mut *connection).await.map_err(|_|BackupError::Database)?;
    if !transaction_ok {
        return Err(BackupError::InvalidTransaction);
    }
    let major: i32 = sqlx::query_scalar(
        "SELECT pg_catalog.current_setting('server_version_num')::integer/10000",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| BackupError::Database)?;
    if major != 18 {
        return Err(BackupError::UnsupportedPostgres);
    }
    // LOCAL settings disappear with the caller's transaction; no session-level
    // formatting or timeout changes can leak into later connection borrowers.
    sqlx::raw_sql("SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO, YMD'; SET LOCAL bytea_output='hex'; SET LOCAL extra_float_digits=3; SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='30s';")
        .execute(&mut *connection).await.map_err(|_|BackupError::Database)?;
    verify_history(connection).await?;
    verify_columns(connection).await?;
    verify_catalog(connection).await?;
    let functions = function_digests(connection).await?;
    let mut tables = Vec::with_capacity(TABLES.len());
    for table in TABLES {
        let query = format!("{} LIMIT {}", table.query, MAX_ROWS_PER_TABLE + 1);
        let mut stream = sqlx::query(&query).fetch(&mut *connection);
        let mut hasher = table_hasher(table.table);
        let mut rows = 0_u64;
        while let Some(row) = stream.try_next().await.map_err(|_| BackupError::Database)? {
            rows += 1;
            if rows > MAX_ROWS_PER_TABLE {
                return Err(BackupError::RowLimit);
            }
            let digest: Vec<u8> = row
                .try_get("digest")
                .map_err(|_| BackupError::InvalidDigest)?;
            if digest.len() != 32 {
                return Err(BackupError::InvalidDigest);
            }
            hasher.update([1]);
            hasher.update(digest);
        }
        hasher.update([0xff]);
        hasher.update(rows.to_be_bytes());
        tables.push(BackupTableDigest {
            table: table.table.to_owned(),
            rows,
            sha256: format!("{:x}", hasher.finalize()),
        });
    }
    Ok(BackupContent {
        schema_version: SCHEMA_VERSION.to_owned(),
        postgres_major: 18,
        tables,
        functions,
    })
}

fn table_hasher(table: &str) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(SCHEMA_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(table.as_bytes());
    hasher.update([0]);
    hasher
}

async fn verify_history(connection: &mut PgConnection) -> Result<(), BackupError> {
    let expected: Vec<_> = MIGRATOR.iter().collect();
    let mut rows=sqlx::query("SELECT version,success,CASE WHEN pg_catalog.octet_length(checksum)<=128 THEN checksum END AS checksum FROM public._sqlx_migrations ORDER BY version LIMIT 1025")
        .fetch(&mut *connection);
    let mut count = 0_usize;
    while let Some(row) = rows.try_next().await.map_err(|_| BackupError::Database)? {
        let migration = expected.get(count).ok_or(BackupError::SchemaMismatch)?;
        let version: i64 = row
            .try_get("version")
            .map_err(|_| BackupError::SchemaMismatch)?;
        let success: bool = row
            .try_get("success")
            .map_err(|_| BackupError::SchemaMismatch)?;
        let checksum: Option<Vec<u8>> = row
            .try_get("checksum")
            .map_err(|_| BackupError::SchemaMismatch)?;
        if version != migration.version
            || !success
            || checksum.as_deref() != Some(migration.checksum.as_ref())
        {
            return Err(BackupError::SchemaMismatch);
        }
        count += 1;
    }
    if count != expected.len() {
        return Err(BackupError::SchemaMismatch);
    }
    Ok(())
}

async fn verify_columns(connection: &mut PgConnection) -> Result<(), BackupError> {
    for table in TABLES {
        let mut rows=sqlx::query("SELECT a.attname::text FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=$1 AND c.relkind='r' AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum LIMIT 128")
            .bind(table.table).fetch(&mut *connection);
        let mut count = 0;
        while let Some(row) = rows.try_next().await.map_err(|_| BackupError::Database)? {
            let column: String = row
                .try_get("attname")
                .map_err(|_| BackupError::SchemaMismatch)?;
            if table.columns.get(count).copied() != Some(column.as_str()) {
                return Err(BackupError::SchemaMismatch);
            }
            count += 1;
        }
        if count != table.columns.len() {
            return Err(BackupError::SchemaMismatch);
        }
    }
    Ok(())
}

async fn verify_catalog(connection: &mut PgConnection) -> Result<(), BackupError> {
    let tables: Vec<_> = TABLES.iter().map(|t| t.table).collect();
    let unsupported:bool=sqlx::query_scalar("SELECT \
        EXISTS(SELECT 1 FROM pg_catalog.pg_namespace WHERE pg_catalog.left(nspname,3)<>'pg_' AND nspname NOT IN ('public','information_schema')) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND (c.relrowsecurity OR c.relforcerowsecurity OR NOT ((c.relkind='r' AND c.relname::text=ANY($1)) OR (c.relkind='i' AND c.relname::text=ANY($2))))) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_trigger g JOIN pg_catalog.pg_class c ON c.oid=g.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND NOT g.tgisinternal AND NOT g.tgname::text=ANY($3)) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_policy p JOIN pg_catalog.pg_class c ON c.oid=p.polrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_type y JOIN pg_catalog.pg_namespace n ON n.oid=y.typnamespace WHERE n.nspname='public' AND NOT ((y.typtype='c' AND EXISTS(SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=y.typrelid AND c.reltype=y.oid AND c.relkind='r' AND c.relname::text=ANY($1))) OR (y.typtype='b' AND EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_type b ON b.oid=c.reltype WHERE c.relkind='r' AND c.relname::text=ANY($1) AND b.typarray=y.oid AND y.typelem=b.oid)))) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_extension WHERE extname<>'plpgsql') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_largeobject_metadata) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_language WHERE lanname NOT IN ('internal','c','sql','plpgsql')) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_transform) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_publication) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_subscription WHERE subdbid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database())) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_event_trigger) OR EXISTS(SELECT 1 FROM pg_catalog.pg_foreign_server) OR EXISTS(SELECT 1 FROM pg_catalog.pg_foreign_data_wrapper) OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_operator x JOIN pg_catalog.pg_namespace n ON n.oid=x.oprnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_opclass x JOIN pg_catalog.pg_namespace n ON n.oid=x.opcnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_opfamily x JOIN pg_catalog.pg_namespace n ON n.oid=x.opfnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_collation x JOIN pg_catalog.pg_namespace n ON n.oid=x.collnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_conversion x JOIN pg_catalog.pg_namespace n ON n.oid=x.connamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_statistic_ext x JOIN pg_catalog.pg_namespace n ON n.oid=x.stxnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_ts_config x JOIN pg_catalog.pg_namespace n ON n.oid=x.cfgnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_ts_dict x JOIN pg_catalog.pg_namespace n ON n.oid=x.dictnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_ts_parser x JOIN pg_catalog.pg_namespace n ON n.oid=x.prsnamespace WHERE n.nspname='public') OR \
        EXISTS(SELECT 1 FROM pg_catalog.pg_ts_template x JOIN pg_catalog.pg_namespace n ON n.oid=x.tmplnamespace WHERE n.nspname='public')")
        .bind(tables).bind(INDEXES).bind(TRIGGERS).fetch_one(&mut *connection).await.map_err(|_|BackupError::Database)?;
    if unsupported {
        return Err(BackupError::SchemaMismatch);
    }
    Ok(())
}

async fn function_digests(
    connection: &mut PgConnection,
) -> Result<Vec<BackupFunctionDigest>, BackupError> {
    let mut functions = Vec::with_capacity(FUNCTIONS.len());
    for function in FUNCTIONS {
        let digest:Option<String>=sqlx::query_scalar("SELECT pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.jsonb_build_array(pg_catalog.pg_get_functiondef(p.oid),pg_catalog.pg_get_userbyid(p.proowner))::text,'UTF8')),'hex') FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public' AND p.prokind='f' AND p.oid=pg_catalog.to_regprocedure($1)")
            .bind(function).fetch_optional(&mut *connection).await.map_err(|_|BackupError::Database)?;
        let sha256 = digest.ok_or(BackupError::SchemaMismatch)?;
        if !valid_hash(&sha256) {
            return Err(BackupError::InvalidDigest);
        }
        functions.push(BackupFunctionDigest {
            function: (*function).to_owned(),
            sha256,
        });
    }
    // All six lookups succeeded above, so NULL entries cannot make this
    // comparison fail open when checking additional overloads/procedures.
    let unknown:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public' AND p.oid NOT IN (SELECT pg_catalog.to_regprocedure(f) FROM pg_catalog.unnest($1::text[]) f))")
        .bind(FUNCTIONS).fetch_one(&mut *connection).await.map_err(|_|BackupError::Database)?;
    if unknown {
        return Err(BackupError::SchemaMismatch);
    }
    Ok(functions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> BackupContent {
        BackupContent {
            schema_version: SCHEMA_VERSION.into(),
            postgres_major: 18,
            tables: TABLES
                .iter()
                .map(|t| BackupTableDigest {
                    table: t.table.into(),
                    rows: 0,
                    sha256: "a".repeat(64),
                })
                .collect(),
            functions: FUNCTIONS
                .iter()
                .map(|f| BackupFunctionDigest {
                    function: (*f).into(),
                    sha256: "b".repeat(64),
                })
                .collect(),
        }
    }

    #[test]
    fn decoded_manifests_require_exact_version_table_order_and_digest_bounds() {
        let manifest = valid();
        assert!(manifest.validate().is_ok());
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["actor"] = "claimed-human".into();
        assert!(serde_json::from_value::<BackupContent>(value).is_err());
        let mut invalid = manifest.clone();
        invalid.tables.swap(0, 1);
        assert_eq!(invalid.validate(), Err(BackupError::InvalidManifest));
        let mut invalid = manifest.clone();
        invalid.tables[0].sha256 = "A".repeat(64);
        assert!(invalid.validate().is_err());
        let mut invalid = manifest.clone();
        invalid.tables[0].rows = MAX_ROWS_PER_TABLE + 1;
        assert!(invalid.validate().is_err());
        let mut invalid = manifest.clone();
        invalid.postgres_major = 17;
        assert!(invalid.validate().is_err());
        let mut invalid = manifest;
        invalid.schema_version = "future".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn framing_separates_empty_tables_and_row_boundaries() {
        let mut a = table_hasher("jobs");
        a.update([0xff]);
        a.update(0_u64.to_be_bytes());
        let mut b = table_hasher("job_outbox");
        b.update([0xff]);
        b.update(0_u64.to_be_bytes());
        assert_ne!(a.finalize(), b.finalize());
        let mut a = table_hasher("jobs");
        a.update([1]);
        a.update([0; 32]);
        a.update([0xff]);
        a.update(1_u64.to_be_bytes());
        let mut b = table_hasher("jobs");
        b.update([1]);
        b.update([0; 32]);
        b.update([1]);
        b.update([0; 32]);
        b.update([0xff]);
        b.update(2_u64.to_be_bytes());
        assert_ne!(a.finalize(), b.finalize());
    }
}
