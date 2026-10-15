//! Isolated disposable PostgreSQL >=16 only; fixed OCR cluster roles are shared
//! with administration tests. Run serially with --ignored --test-threads=1.
use std::{str::FromStr, time::Duration};

use ocr_core::administration::{self, AdminError};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use uuid::Uuid;

struct Fixture {
    cluster: PgConnection,
    options: PgConnectOptions,
    name: String,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("TEST_ADMIN_DATABASE_URL")
            .expect("TEST_ADMIN_DATABASE_URL required; never use a live DB");
        let mut cluster = PgConnection::connect(&url).await.unwrap();
        let name = format!("ocr_restore_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut cluster)
            .await
            .unwrap();
        println!("TRACE restore-target disposable fixture created: {name}");
        let options = PgConnectOptions::from_str(&url).unwrap().database(&name);
        let mut admin = PgConnection::connect_with(&options).await.unwrap();
        let passwords =
            std::array::from_fn(|i| format!("isolated-role-{i}-quote'back\\slash-safe"));
        administration::bootstrap(&mut admin, &passwords)
            .await
            .unwrap();
        admin.close().await.unwrap();
        Self {
            cluster,
            options,
            name,
        }
    }

    async fn admin(&self) -> PgConnection {
        PgConnection::connect_with(&self.options).await.unwrap()
    }

    async fn login(&self, role: &str, index: usize) -> PgConnection {
        PgConnection::connect_with(
            &self
                .options
                .clone()
                .username(role)
                .password(&format!("isolated-role-{index}-quote'back\\slash-safe")),
        )
        .await
        .unwrap()
    }

    async fn change(&self, sql: &str) {
        let mut admin = self.admin().await;
        sqlx::raw_sql(sql).execute(&mut admin).await.unwrap();
        admin.close().await.unwrap();
    }

    async fn attempt(&self, expected: &str) -> Result<(), AdminError> {
        let mut migrator = self.login("ocr_migrator", 0).await;
        let result = administration::prepare_restore_target(&mut migrator, expected).await;
        // Failure can follow acquisition of a session advisory lock. Always
        // close; returning this connection to a pool would retain that lock.
        migrator.close().await.unwrap();
        result
    }

    async fn reject_change(&self, label: &str, create: &str, repair: &str) {
        self.change(create).await;
        let result = self.attempt(&self.name).await;
        // Repair global fixed-role state even if the assertion fails.
        self.change(repair).await;
        assert!(
            matches!(
                result,
                Err(AdminError::RestoreTarget | AdminError::RoleCollision | AdminError::Ownership)
            ),
            "{label}: {result:?}"
        );
        self.attempt(&self.name)
            .await
            .unwrap_or_else(|e| panic!("{label}: repaired fixture must accept: {e:?}"));
        println!("TRACE restore-target rejected {label}; fixture drift repaired");
    }

    async fn finish(mut self) {
        sqlx::query(&format!("DROP DATABASE {}", self.name))
            .execute(&mut self.cluster)
            .await
            .unwrap();
        self.cluster.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated disposable PostgreSQL >=16 cluster and fixed OCR roles"]
async fn empty_target_retains_owner_identity_and_session_lock_without_ddl() {
    let fixture = Fixture::new().await;
    let mut migrator = fixture.login("ocr_migrator", 0).await;
    let original_schema_oid: i64 =
        sqlx::query_scalar("SELECT oid::bigint FROM pg_namespace WHERE nspname='public'")
            .fetch_one(&mut migrator)
            .await
            .unwrap();
    administration::prepare_restore_target(&mut migrator, &fixture.name)
        .await
        .unwrap();
    let (login, role, schema_oid, objects, defaults, runtime_create): (
        String,
        String,
        i64,
        i64,
        i64,
        bool,
    ) = sqlx::query_as(
        "SELECT session_user::text,current_user::text,(SELECT oid::bigint FROM pg_namespace WHERE nspname='public'),(SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public'),(SELECT count(*) FROM pg_default_acl),has_schema_privilege('ocr_api','public','CREATE')",
    ).fetch_one(&mut migrator).await.unwrap();
    assert_eq!(
        (login.as_str(), role.as_str()),
        ("ocr_migrator", "ocr_owner")
    );
    assert_eq!(schema_oid, original_schema_oid, "public was retained");
    assert_eq!((objects, defaults, runtime_create), (0, 1, false));

    // A session lock survives the preflight transaction commit and is released
    // only when its dedicated connection closes.
    let mut observer = fixture.admin().await;
    let before_close: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(186878833,5)")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert!(!before_close);
    migrator.close().await.unwrap();
    // Client close sends Terminate but does not await the server's backend exit.
    // Observe eventual server cleanup under a deadline instead of racing it.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let released: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(186878833,5)")
                .fetch_one(&mut observer)
                .await
                .unwrap();
            if released {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("closing the dedicated connection must release its server session lock");
    observer.close().await.unwrap();
    println!(
        "TRACE restore-target accepted: actual login migrator, final role owner; public OID retained, objects=0, default ACL rows=1, runtime CREATE=false; session lock retained after commit and released on close"
    );
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires dedicated disposable PostgreSQL >=16 cluster and fixed OCR roles"]
async fn rejects_identity_inventory_acl_ownership_and_role_drift() {
    let fixture = Fixture::new().await;
    let long_name = "a".repeat(64);
    for invalid in [
        "",
        "1invalid",
        "white space",
        "quoted\"name",
        "cơ_sở",
        &long_name,
    ] {
        assert!(matches!(
            fixture.attempt(invalid).await,
            Err(AdminError::RestoreTarget)
        ));
    }
    assert!(matches!(
        fixture.attempt("explicit_but_wrong_database").await,
        Err(AdminError::RestoreTarget)
    ));
    let mut api = fixture.login("ocr_api", 1).await;
    assert!(matches!(
        administration::prepare_restore_target(&mut api, &fixture.name).await,
        Err(AdminError::MigratorRequired)
    ));
    api.close().await.unwrap();
    let mut admin = fixture.admin().await;
    sqlx::query("SET ROLE ocr_owner")
        .execute(&mut admin)
        .await
        .unwrap();
    assert!(matches!(
        administration::prepare_restore_target(&mut admin, &fixture.name).await,
        Err(AdminError::MigratorRequired)
    ));
    admin.close().await.unwrap();
    println!(
        "TRACE restore-target identity: empty/non-ASCII/invalid/overlong/mismatched database names and wrong actual logins rejected, including admin SET ROLE owner"
    );

    for (label, create, repair) in [
        (
            "supported OCR table already present",
            "CREATE TABLE public.jobs(id uuid); ALTER TABLE public.jobs OWNER TO ocr_owner",
            "DROP TABLE public.jobs",
        ),
        (
            "unrelated table",
            "CREATE TABLE public.unrelated(value integer)",
            "DROP TABLE public.unrelated",
        ),
        (
            "unrelated schema",
            "CREATE SCHEMA unrelated",
            "DROP SCHEMA unrelated",
        ),
        (
            "unrelated enum type",
            "CREATE TYPE public.unrelated_type AS ENUM ('synthetic')",
            "DROP TYPE public.unrelated_type",
        ),
        (
            "unrelated function",
            "CREATE FUNCTION public.unrelated_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
            "DROP FUNCTION public.unrelated_function()",
        ),
        (
            "large object",
            "SELECT lo_create(0)",
            "SELECT lo_unlink(oid) FROM pg_largeobject_metadata",
        ),
        (
            "global default TABLE privilege",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner GRANT SELECT ON TABLES TO ocr_api",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner REVOKE SELECT ON TABLES FROM ocr_api",
        ),
        (
            "schema default TABLE privilege",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner IN SCHEMA public GRANT SELECT ON TABLES TO ocr_api",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner IN SCHEMA public REVOKE SELECT ON TABLES FROM ocr_api",
        ),
        (
            "PUBLIC default FUNCTION execute",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner GRANT EXECUTE ON FUNCTIONS TO PUBLIC",
            "ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC",
        ),
        (
            "runtime schema CREATE privilege",
            "GRANT CREATE ON SCHEMA public TO ocr_api",
            "REVOKE CREATE ON SCHEMA public FROM ocr_api",
        ),
        (
            "PUBLIC schema USAGE privilege",
            "GRANT USAGE ON SCHEMA public TO PUBLIC",
            "REVOKE USAGE ON SCHEMA public FROM PUBLIC",
        ),
        (
            "schema privilege grant option",
            "GRANT USAGE ON SCHEMA public TO ocr_api WITH GRANT OPTION",
            "REVOKE GRANT OPTION FOR USAGE ON SCHEMA public FROM ocr_api",
        ),
        (
            "schema owner drift",
            "ALTER SCHEMA public OWNER TO ocr_owner",
            "ALTER SCHEMA public OWNER TO pg_database_owner; REVOKE ALL ON SCHEMA public FROM PUBLIC,ocr_owner,ocr_migrator,ocr_api,ocr_worker,ocr_maintenance,ocr_backup; GRANT USAGE,CREATE ON SCHEMA public TO ocr_owner; GRANT USAGE ON SCHEMA public TO ocr_api,ocr_worker,ocr_maintenance,ocr_backup",
        ),
        (
            "role attribute drift",
            "ALTER ROLE ocr_api CREATEDB",
            "ALTER ROLE ocr_api NOCREATEDB",
        ),
        (
            "unexpected membership",
            "GRANT ocr_owner TO ocr_api",
            "REVOKE ocr_owner FROM ocr_api",
        ),
        (
            "missing owner membership",
            "REVOKE ocr_owner FROM ocr_migrator",
            "GRANT ocr_owner TO ocr_migrator WITH ADMIN FALSE,INHERIT FALSE,SET TRUE",
        ),
        (
            "role GUC override",
            "ALTER ROLE ocr_api SET statement_timeout='90s'",
            "ALTER ROLE ocr_api RESET statement_timeout",
        ),
        (
            "missing managed search_path default",
            "ALTER ROLE ocr_api RESET search_path",
            "ALTER ROLE ocr_api SET search_path=pg_catalog,public",
        ),
    ] {
        fixture.reject_change(label, create, repair).await;
    }
    for (label, create, repair) in [
        (
            "runtime database TEMP privilege",
            format!("GRANT TEMP ON DATABASE {} TO ocr_api", fixture.name),
            format!("REVOKE TEMP ON DATABASE {} FROM ocr_api", fixture.name),
        ),
        (
            "PUBLIC database CONNECT privilege",
            format!("GRANT CONNECT ON DATABASE {} TO PUBLIC", fixture.name),
            format!("REVOKE CONNECT ON DATABASE {} FROM PUBLIC", fixture.name),
        ),
        (
            "database privilege grant option",
            format!(
                "GRANT CONNECT ON DATABASE {} TO ocr_api WITH GRANT OPTION",
                fixture.name
            ),
            format!(
                "REVOKE GRANT OPTION FOR CONNECT ON DATABASE {} FROM ocr_api",
                fixture.name
            ),
        ),
        (
            "database owner drift",
            format!("ALTER DATABASE {} OWNER TO ocr_owner", fixture.name),
            format!(
                "ALTER DATABASE {} OWNER TO \"{}\"",
                fixture.name,
                fixture.options.get_username().replace('"', "\"\"")
            ),
        ),
        (
            "database-wide GUC override",
            format!(
                "ALTER DATABASE {} SET statement_timeout='90s'",
                fixture.name
            ),
            format!("ALTER DATABASE {} RESET ALL", fixture.name),
        ),
        (
            "per-database role GUC override",
            format!(
                "ALTER ROLE ocr_api IN DATABASE {} SET statement_timeout='90s'",
                fixture.name
            ),
            format!("ALTER ROLE ocr_api IN DATABASE {} RESET ALL", fixture.name),
        ),
    ] {
        fixture.reject_change(label, &create, &repair).await;
    }
    // ALTER SCHEMA OWNER transfers the old owner's implicit ACL entries. Reset
    // bootstrap policy explicitly before proving the repaired target accepts.
    let mut repaired = fixture.admin().await;
    let passwords = std::array::from_fn(|i| format!("isolated-role-{i}-quote'back\\slash-safe"));
    administration::bootstrap(&mut repaired, &passwords)
        .await
        .unwrap();
    repaired.close().await.unwrap();
    fixture.attempt(&fixture.name).await.unwrap();
    println!(
        "TRACE restore-target all synthetic inventory/ACL/ownership/role/GUC drift refused; explicit bootstrap repair restores acceptance"
    );
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires dedicated disposable PostgreSQL >=16 cluster and fixed OCR roles"]
async fn rejects_another_backend_and_a_competing_session_lock() {
    let fixture = Fixture::new().await;
    let mut other = fixture.admin().await;
    assert!(matches!(
        fixture.attempt(&fixture.name).await,
        Err(AdminError::RestoreBusy)
    ));
    sqlx::query("SELECT pg_advisory_lock(186878833,5)")
        .execute(&mut other)
        .await
        .unwrap();
    assert!(matches!(
        fixture.attempt(&fixture.name).await,
        Err(AdminError::RestoreBusy)
    ));
    other.close().await.unwrap();
    fixture.attempt(&fixture.name).await.unwrap();
    println!(
        "TRACE restore-target competing backend and advisory lock each refused; closing connections releases exclusion and empty target accepts"
    );
    // The exporter holds shared key3 for snapshot+dump. Migrate must refuse an
    // exclusive lock rather than change schema during that capture interval.
    let mut exporter = fixture.admin().await;
    sqlx::query("SELECT pg_advisory_lock_shared(186878833,3)")
        .execute(&mut exporter)
        .await
        .unwrap();
    let mut migrator = fixture.login("ocr_migrator", 0).await;
    assert!(matches!(
        administration::migrate(&mut migrator).await,
        Err(AdminError::OperationBusy)
    ));
    let migration_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations')::text")
            .fetch_one(&mut migrator)
            .await
            .unwrap();
    assert!(migration_table.is_none());
    sqlx::query("SELECT pg_advisory_unlock_shared(186878833,3)")
        .execute(&mut exporter)
        .await
        .unwrap();
    administration::migrate(&mut migrator).await.unwrap();
    let released_after_success: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_lock(186878833,3)")
            .fetch_one(&mut exporter)
            .await
            .unwrap();
    assert!(released_after_success);
    sqlx::query("SELECT pg_advisory_unlock(186878833,3)")
        .execute(&mut exporter)
        .await
        .unwrap();
    sqlx::query("UPDATE public._sqlx_migrations SET checksum=decode('00','hex') WHERE version=1")
        .execute(&mut exporter)
        .await
        .unwrap();
    assert!(matches!(
        administration::migrate(&mut migrator).await,
        Err(AdminError::Migration)
    ));
    let released_after_error: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(186878833,3)")
        .fetch_one(&mut exporter)
        .await
        .unwrap();
    assert!(released_after_error);
    migrator.close().await.unwrap();
    exporter.close().await.unwrap();
    println!(
        "TRACE source-DDL coordination: shared exporter key3 blocks migrate before DDL; exclusive migration key3 released after success and checksum-error failure; original Migration error preserved"
    );
    fixture.finish().await;
}
