//! Explicit one-shot operations. No live adoption, credential rotation, or delete.
use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool, Row};
use thiserror::Error;

use crate::schema::{self, MIGRATOR};

const BOOTSTRAP: &str = include_str!("../administration/bootstrap.sql");
const GRANTS: &str = include_str!("../administration/grants.sql");
const DEDICATED_DATABASE: &str = include_str!("../administration/dedicated_database.sql");
pub const PASSWORD_NAMES: [&str; 5] = [
    "OCR_MIGRATOR_PASSWORD",
    "OCR_API_PASSWORD",
    "OCR_WORKER_PASSWORD",
    "OCR_MAINTENANCE_PASSWORD",
    "OCR_BACKUP_PASSWORD",
];
pub const LOGIN_ROLES: [&str; 5] = [
    "ocr_migrator",
    "ocr_api",
    "ocr_worker",
    "ocr_maintenance",
    "ocr_backup",
];

#[derive(Debug, Error)]
pub enum AdminError {
    #[error("administration database operation failed (details redacted)")]
    Database,
    #[error("administration operation exceeded its deadline")]
    Deadline,
    #[error("bootstrap requires a dedicated database and a superuser administration connection")]
    AdminRequired,
    #[error("existing OCR role has unexpected attributes or membership; refusing to alter it")]
    RoleCollision,
    #[error(
        "database inventory is unsupported or not exclusively OCR-owned; automatic adoption is disabled"
    )]
    Ownership,
    #[error("operation requires the ocr_migrator login")]
    MigratorRequired,
    #[error("restore target identity, empty inventory, ownership or privileges are unsupported")]
    RestoreTarget,
    #[error("restore target has another connection or an active cooperative restore")]
    RestoreBusy,
    #[error("schema operation is blocked by another cooperative administration operation")]
    OperationBusy,
    #[error("role password must contain 16..=1024 bytes and no NUL")]
    Password,
    #[error("schema operation failed: {0}")]
    Schema(#[from] schema::SchemaError),
    #[error("migration failed (details redacted); inspect schema with the administrator")]
    Migration,
}

/// Does not expose URLs, SQL with credential literals, server messages or values.
pub async fn connect(url: &str) -> Result<PgConnection, AdminError> {
    let options = crate::postgres::safe_options(url).map_err(|_| AdminError::Database)?;
    tokio::time::timeout(
        Duration::from_secs(10),
        PgConnection::connect_with(&options),
    )
    .await
    .map_err(|_| AdminError::Deadline)?
    .map_err(|_| AdminError::Database)
}

/// Creates missing roles only. Existing passwords are never changed. All changes
/// commit together; any conflicting role, membership or non-owned public object
/// aborts the operation. The database must already exist, on a dedicated volume.
pub async fn bootstrap(
    connection: &mut PgConnection,
    passwords: &[String; 5],
) -> Result<(), AdminError> {
    if passwords
        .iter()
        .any(|p| !(16..=1024).contains(&p.len()) || p.contains('\0'))
    {
        return Err(AdminError::Password);
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut tx = connection.begin().await.map_err(|_| AdminError::Database)?;
        admin_only(&mut tx).await?;
        // Serialize repeated bootstrap/grant on this dedicated database. Logging
        // changes are transaction-local and restore on commit/rollback. Extension
        // audit logging remains an operator responsibility.
        sqlx::raw_sql("SET LOCAL log_statement='none'; SET LOCAL log_min_error_statement='panic'; SET LOCAL log_error_verbosity='terse'; SET LOCAL log_parameter_max_length=0; SET LOCAL log_parameter_max_length_on_error=0; SET LOCAL log_duration=off; SET LOCAL log_min_duration_statement=-1; SET LOCAL log_min_duration_sample=-1; SET LOCAL log_transaction_sample_rate=0;")
            .execute(&mut *tx).await.map_err(|_|AdminError::Database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(186878833,3)")
            .execute(&mut *tx).await.map_err(|_|AdminError::Database)?;
        check_role_collisions(&mut tx).await?;
        check_ownership(&mut tx).await?;
        let owner_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ocr_owner')")
            .fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
        if !owner_exists {
            sqlx::query("CREATE ROLE ocr_owner NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")
                .execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
        }
        for (role, password) in LOGIN_ROLES.iter().zip(passwords) {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname=$1)")
                .bind(role).fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
            if !exists {
                // No password-bearing DDL is constructed in the client. These
                // transaction-local values are bound parameters; PostgreSQL
                // safely formats identifiers/literals inside the static block.
                // Operators must also control server/pgaudit parameter logging.
                sqlx::query("SELECT set_config('ocr.bootstrap.role_name',$1,true),set_config('ocr.bootstrap.role_password',$2,true)")
                    .bind(role).bind(password).execute(&mut *tx).await.map_err(|_|AdminError::Database)?;
                sqlx::raw_sql("DO $ocr$ BEGIN EXECUTE format('CREATE ROLE %I LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD %L',current_setting('ocr.bootstrap.role_name'),current_setting('ocr.bootstrap.role_password')); END $ocr$;")
                    .execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
                sqlx::query("SELECT set_config('ocr.bootstrap.role_name','',true),set_config('ocr.bootstrap.role_password','',true)")
                    .execute(&mut *tx).await.map_err(|_|AdminError::Database)?;
            }
        }
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
        let identifier = quote_identifier(&database);
        sqlx::raw_sql(&format!("REVOKE ALL ON DATABASE {identifier} FROM PUBLIC,ocr_owner,ocr_migrator,ocr_api,ocr_worker,ocr_maintenance,ocr_backup; GRANT CONNECT ON DATABASE {identifier} TO ocr_migrator,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;"))
            .execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
        sqlx::raw_sql(BOOTSTRAP).execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
        tx.commit().await.map_err(|_| AdminError::Database)
    }).await.map_err(|_| AdminError::Deadline)?
}

pub async fn migrate(connection: &mut PgConnection) -> Result<(), AdminError> {
    let mut administration_lock_held = false;
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        let login: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
        if login != "ocr_migrator" {
            return Err(AdminError::MigratorRequired);
        }
        sqlx::query("SET ROLE ocr_owner")
            .execute(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
        sqlx::query("SET search_path = public, pg_catalog")
            .execute(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
        sqlx::query("SET statement_timeout='60s'")
            .execute(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
        let locked: bool =
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock(186878833,3)")
                .fetch_one(&mut *connection)
                .await
                .map_err(|_| AdminError::Database)?;
        if !locked {
            return Err(AdminError::OperationBusy);
        }
        administration_lock_held = true;
        MIGRATOR
            .run(&mut *connection)
            .await
            .map_err(|_| AdminError::Migration)?;
        // grant is a separate administration action, never a runtime privilege.
        Ok(())
    })
    .await
    .unwrap_or(Err(AdminError::Deadline));
    // SQLx migrations manage their own transactions. A session lock spans the
    // whole migration batch without adding an outer transaction/savepoint.
    // Attempt cleanup on every result, including a deadline, preserving the
    // original migration error. Callers must close this dedicated connection on
    // cancellation or failed cleanup, never return it to a pool.
    if administration_lock_held {
        let released = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_scalar::<_, bool>("SELECT pg_catalog.pg_advisory_unlock(186878833,3)")
                .fetch_one(&mut *connection),
        )
        .await;
        if result.is_ok() && !matches!(released, Ok(Ok(true))) {
            return Err(AdminError::Database);
        }
    }
    result
}

/// Preflights an already bootstrapped, empty restore database without changing
/// objects or granting runtime access. `expected_database` must be an explicit
/// ASCII SQL identifier, and the actual login must be `ocr_migrator`.
///
/// Call once on a newly opened, dedicated connection. Success leaves
/// `current_user = ocr_owner` and `session_user = ocr_migrator`, with session
/// advisory lock `(186878833, 5)` held until this connection is closed. Keep it
/// open through restore and verification; close it on *every* error as well.
/// Never return it to a pool. The no-other-backend check and advisory lock are
/// cooperative isolation, not exclusion against an administrator opening a new
/// connection after this check.
pub async fn prepare_restore_target(
    connection: &mut PgConnection,
    expected_database: &str,
) -> Result<(), AdminError> {
    let valid_identifier = !expected_database.is_empty()
        && expected_database.len() <= 63
        && expected_database
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()));
    if !valid_identifier {
        return Err(AdminError::RestoreTarget);
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        // Set a trusted search path before evaluating any inventory. This is a
        // session setting deliberately retained on this throwaway connection.
        sqlx::raw_sql("SET search_path=pg_catalog,public; SET lock_timeout='2s'; SET statement_timeout='5s';")
            .execute(&mut *connection).await.map_err(|_| AdminError::Database)?;
        let (login, database): (String, String) = sqlx::query_as(
            "SELECT session_user::text,pg_catalog.current_database()::text",
        ).fetch_one(&mut *connection).await.map_err(|_| AdminError::Database)?;
        if login != "ocr_migrator" {
            return Err(AdminError::MigratorRequired);
        }
        if database != expected_database {
            return Err(AdminError::RestoreTarget);
        }
        let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock(186878833,5)")
            .fetch_one(&mut *connection).await.map_err(|_| AdminError::Database)?;
        if !acquired {
            return Err(AdminError::RestoreBusy);
        }
        // Unprivileged pg_stat_activity hides other users' state/backend_type,
        // but datname and pid are visible. Do not filter by hidden columns.
        let other_backend: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=pg_catalog.current_database() AND pid<>pg_catalog.pg_backend_pid())",
        ).fetch_one(&mut *connection).await.map_err(|_| AdminError::Database)?;
        if other_backend {
            return Err(AdminError::RestoreBusy);
        }
        let mut tx = connection.begin().await.map_err(|_| AdminError::Database)?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
        check_role_collisions(&mut tx).await?;
        let roles_ready: bool = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM pg_roles WHERE rolname=ANY($1))=6 AND (SELECT count(*) FROM pg_auth_members m JOIN pg_roles r ON r.oid=m.roleid JOIN pg_roles u ON u.oid=m.member WHERE r.rolname='ocr_owner' AND u.rolname='ocr_migrator' AND NOT m.admin_option AND NOT m.inherit_option AND m.set_option)=1 AND (SELECT count(*) FROM pg_db_role_setting s JOIN pg_roles r ON r.oid=s.setrole WHERE s.setdatabase=0 AND r.rolname=ANY($2) AND s.setconfig=ARRAY[CASE WHEN r.rolname='ocr_migrator' THEN 'search_path=public, pg_catalog' ELSE 'search_path=pg_catalog, public' END])=5",
        ).bind(["ocr_owner","ocr_migrator","ocr_api","ocr_worker","ocr_maintenance","ocr_backup"])
            .bind(LOGIN_ROLES).fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
        if !roles_ready {
            return Err(AdminError::RoleCollision);
        }
        sqlx::query("SET ROLE ocr_owner")
            .execute(&mut *tx).await.map_err(|_| AdminError::Database)?;
        check_ownership(&mut tx).await?;
        // A supported OCR object is still a nonempty target. Never run migrate
        // or clear an existing database in preparation for full pg_restore.
        let nonempty: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname NOT IN ('pg_catalog','pg_toast','information_schema','public')) OR EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public') OR EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public') OR EXISTS(SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname='public') OR EXISTS(SELECT 1 FROM pg_largeobject_metadata) OR EXISTS(SELECT 1 FROM pg_language WHERE lanname NOT IN ('internal','c','sql','plpgsql')) OR EXISTS(SELECT 1 FROM pg_transform)",
        ).fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
        if nonempty {
            return Err(AdminError::RestoreTarget);
        }
        let safe_acl: bool = sqlx::query_scalar(RESTORE_TARGET_ACL)
            .fetch_one(&mut *tx).await.map_err(|_| AdminError::Database)?;
        if !safe_acl {
            return Err(AdminError::RestoreTarget);
        }
        // SET ROLE is transactional until commit; on success retain the owner
        // identity needed to inspect restored tables before any reviewed grant.
        tx.commit().await.map_err(|_| AdminError::Database)
    }).await.map_err(|_| AdminError::Deadline)?
}

// Exact bootstrap ACLs matter: --no-acl during restore inherits target defaults.
// In particular an old TABLE default grant could expose documents immediately.
const RESTORE_TARGET_ACL: &str = r#"
WITH database AS (
    SELECT d.*,r.rolsuper,r.rolname FROM pg_database d JOIN pg_roles r ON r.oid=d.datdba
    WHERE d.datname=current_database()
), schema AS (
    SELECT n.* FROM pg_namespace n JOIN pg_roles r ON r.oid=n.nspowner
    WHERE n.nspname='public' AND r.rolname='pg_database_owner'
), expected_schema(grantee,privilege_type) AS (VALUES
    ('pg_database_owner','USAGE'),('pg_database_owner','CREATE'),
    ('ocr_owner','USAGE'),('ocr_owner','CREATE'),
    ('ocr_api','USAGE'),('ocr_worker','USAGE'),
    ('ocr_maintenance','USAGE'),('ocr_backup','USAGE')
), actual_schema AS (
    SELECT coalesce(r.rolname,'PUBLIC')::text AS grantee,a.privilege_type
    FROM schema n CROSS JOIN LATERAL aclexplode(coalesce(n.nspacl,acldefault('n',n.nspowner))) a
    LEFT JOIN pg_roles r ON r.oid=a.grantee
), expected_database(grantee,privilege_type) AS (
    SELECT rolname::text,p FROM database CROSS JOIN (VALUES ('CREATE'),('CONNECT'),('TEMPORARY')) v(p)
    UNION ALL SELECT r,p FROM (VALUES ('ocr_migrator'),('ocr_api'),('ocr_worker'),('ocr_maintenance'),('ocr_backup')) v(r)
    CROSS JOIN (VALUES ('CONNECT')) q(p)
), actual_database AS (
    SELECT coalesce(r.rolname,'PUBLIC')::text AS grantee,a.privilege_type
    FROM database d CROSS JOIN LATERAL aclexplode(coalesce(d.datacl,acldefault('d',d.datdba))) a
    LEFT JOIN pg_roles r ON r.oid=a.grantee
)
SELECT (SELECT count(*) FROM database WHERE rolsuper AND NOT datistemplate AND datallowconn
        AND rolname NOT IN ('ocr_owner','ocr_migrator','ocr_api','ocr_worker','ocr_maintenance','ocr_backup'))=1
    AND (SELECT count(*) FROM schema)=1
    AND NOT EXISTS((SELECT * FROM actual_schema EXCEPT SELECT * FROM expected_schema)
        UNION ALL (SELECT * FROM expected_schema EXCEPT SELECT * FROM actual_schema))
    AND NOT EXISTS(SELECT 1 FROM schema n CROSS JOIN LATERAL aclexplode(coalesce(n.nspacl,acldefault('n',n.nspowner))) a
        WHERE a.is_grantable OR a.grantor<>n.nspowner)
    AND NOT EXISTS((SELECT * FROM actual_database EXCEPT SELECT * FROM expected_database)
        UNION ALL (SELECT * FROM expected_database EXCEPT SELECT * FROM actual_database))
    AND NOT EXISTS(SELECT 1 FROM database d CROSS JOIN LATERAL aclexplode(coalesce(d.datacl,acldefault('d',d.datdba))) a
        WHERE a.is_grantable OR a.grantor<>d.datdba)
    AND (SELECT count(*) FROM pg_default_acl)=1
    AND (SELECT count(*) FROM pg_default_acl d JOIN pg_roles r ON r.oid=d.defaclrole
        WHERE r.rolname='ocr_owner' AND d.defaclnamespace=0 AND d.defaclobjtype='f'
            AND d.defaclacl=ARRAY[format('%I=X/%I','ocr_owner','ocr_owner')::aclitem])=1
"#;

/// Explicit compatibility command for disposable/local development only. The
/// production Compose never exposes this command to a runtime principal.
pub async fn migrate_dev(connection: &mut PgConnection) -> Result<(), AdminError> {
    tokio::time::timeout(Duration::from_secs(120), async {
        let admin: bool =
            sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname=current_user")
                .fetch_one(&mut *connection)
                .await
                .map_err(|_| AdminError::Database)?;
        if !admin {
            return Err(AdminError::AdminRequired);
        }
        MIGRATOR
            .run(&mut *connection)
            .await
            .map_err(|_| AdminError::Migration)
    })
    .await
    .map_err(|_| AdminError::Deadline)?
}

pub async fn grant(connection: &mut PgConnection) -> Result<(), AdminError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut tx = connection.begin().await.map_err(|_| AdminError::Database)?;
        admin_only(&mut tx).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(186878833,3)")
            .execute(&mut *tx)
            .await
            .map_err(|_| AdminError::Database)?;
        check_role_collisions(&mut tx).await?;
        check_ownership(&mut tx).await?;
        sqlx::raw_sql(GRANTS)
            .execute(&mut *tx)
            .await
            .map_err(|_| AdminError::Database)?;
        tx.commit().await.map_err(|_| AdminError::Database)
    })
    .await
    .map_err(|_| AdminError::Deadline)?
}

pub async fn check(url: &str) -> Result<(), AdminError> {
    let options = crate::postgres::safe_options(url).map_err(|_| AdminError::Database)?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .map_err(|_| AdminError::Database)?;
    let result = schema::check(&pool).await;
    pool.close().await;
    result.map_err(AdminError::Schema)
}

async fn admin_only(connection: &mut PgConnection) -> Result<(), AdminError> {
    sqlx::query("SET LOCAL lock_timeout='2s'")
        .execute(&mut *connection)
        .await
        .map_err(|_| AdminError::Database)?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *connection)
        .await
        .map_err(|_| AdminError::Database)?;
    let admin: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| AdminError::Database)?;
    if !admin {
        return Err(AdminError::AdminRequired);
    }
    Ok(())
}

async fn check_role_collisions(connection: &mut PgConnection) -> Result<(), AdminError> {
    let rows = sqlx::query("SELECT rolname,rolsuper,rolcreatedb,rolcreaterole,rolreplication,rolbypassrls,rolcanlogin,rolinherit FROM pg_roles WHERE rolname=ANY($1)")
        .bind(["ocr_owner","ocr_migrator","ocr_api","ocr_worker","ocr_maintenance","ocr_backup"])
        .fetch_all(&mut *connection).await.map_err(|_| AdminError::Database)?;
    for row in rows {
        let name: &str = row.get("rolname");
        let unexpected = [
            "rolsuper",
            "rolcreatedb",
            "rolcreaterole",
            "rolreplication",
            "rolbypassrls",
            "rolinherit",
        ]
        .into_iter()
        .any(|key| row.get::<bool, _>(key));
        if unexpected || row.get::<bool, _>("rolcanlogin") != (name != "ocr_owner") {
            return Err(AdminError::RoleCollision);
        }
    }
    let unexpected: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_auth_members m JOIN pg_roles r ON r.oid=m.roleid JOIN pg_roles u ON u.oid=m.member WHERE (r.rolname=ANY($1) OR u.rolname=ANY($1)) AND NOT(r.rolname='ocr_owner' AND u.rolname='ocr_migrator' AND NOT m.admin_option AND NOT m.inherit_option AND m.set_option))")
        .bind(["ocr_owner","ocr_migrator","ocr_api","ocr_worker","ocr_maintenance","ocr_backup"])
        .fetch_one(&mut *connection).await.map_err(|_| AdminError::Database)?;
    if unexpected {
        return Err(AdminError::RoleCollision);
    }
    let owns_objects: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_shdepend d JOIN pg_roles r ON r.oid=d.refobjid WHERE d.refclassid='pg_authid'::regclass AND d.deptype='o' AND r.rolname=ANY($1))")
        .bind(LOGIN_ROLES).fetch_one(&mut *connection).await.map_err(|_| AdminError::Database)?;
    if owns_objects {
        return Err(AdminError::RoleCollision);
    }
    // ALTER ROLE ... IN DATABASE overrides take precedence over global defaults;
    // preserving an old session_replication_role override could bypass triggers.
    // Only this tool's expected global search_path setting is supported.
    let unexpected_settings: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_db_role_setting s JOIN pg_roles r ON r.oid=s.setrole CROSS JOIN LATERAL unnest(s.setconfig) value WHERE r.rolname=ANY($1) AND (s.setdatabase<>0 OR r.rolname='ocr_owner' OR value<>CASE WHEN r.rolname='ocr_migrator' THEN 'search_path=public, pg_catalog' ELSE 'search_path=pg_catalog, public' END)) OR EXISTS(SELECT 1 FROM pg_db_role_setting WHERE setrole=0 AND setdatabase=(SELECT oid FROM pg_database WHERE datname=current_database()) AND cardinality(setconfig)>0)")
        .bind(["ocr_owner","ocr_migrator","ocr_api","ocr_worker","ocr_maintenance","ocr_backup"])
        .fetch_one(&mut *connection).await.map_err(|_|AdminError::Database)?;
    if unexpected_settings {
        return Err(AdminError::RoleCollision);
    }
    Ok(())
}

async fn check_ownership(connection: &mut PgConnection) -> Result<(), AdminError> {
    let unexpected: bool = sqlx::query_scalar(DEDICATED_DATABASE)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| AdminError::Database)?;
    if unexpected {
        return Err(AdminError::Ownership);
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Fixed-role report contains no secrets, object contents or filenames.
pub async fn role_report(pool: &PgPool) -> Result<Vec<(String, bool)>, AdminError> {
    sqlx::query_as(
        "SELECT rolname::text,rolcanlogin FROM pg_roles WHERE rolname=ANY($1) ORDER BY rolname",
    )
    .bind([
        "ocr_owner",
        "ocr_migrator",
        "ocr_api",
        "ocr_worker",
        "ocr_maintenance",
        "ocr_backup",
    ])
    .fetch_all(pool)
    .await
    .map_err(|_| AdminError::Database)
}
