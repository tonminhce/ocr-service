//! Exact, read-only runtime schema validation. DDL belongs to the one-shot admin.
use std::time::Duration;

use sqlx::{PgPool, Row, migrate::Migrator};
use thiserror::Error;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("schema check deadline elapsed")]
    Deadline,
    #[error("schema history is unavailable; run the one-shot migrator")]
    Unavailable,
    #[error("schema migration {0} is dirty")]
    Dirty(i64),
    #[error("schema migration {0} is unsupported by this binary")]
    Unsupported(i64),
    #[error("schema migration {0} is pending; run the one-shot migrator")]
    Pending(i64),
    #[error("schema migration {0} checksum differs from this binary")]
    Checksum(i64),
    #[error("schema history read failed")]
    Database,
}

/// Does not create the history table or take a migration/DDL lock. Exact history
/// is intentional: rolling an old binary back over a newer schema fails closed.
pub async fn check(pool: &PgPool) -> Result<(), SchemaError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut tx = pool.begin().await.map_err(|_| SchemaError::Database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| SchemaError::Database)?;
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *tx)
            .await
            .map_err(|_| SchemaError::Database)?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *tx)
            .await
            .map_err(|_| SchemaError::Database)?;
        let rows = sqlx::query(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version LIMIT 1024",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|e| e.code().as_deref() == Some("42P01"))
            {
                SchemaError::Unavailable
            } else {
                SchemaError::Database
            }
        })?;
        for row in &rows {
            let version: i64 = row.try_get("version").map_err(|_| SchemaError::Database)?;
            if !row
                .try_get::<bool, _>("success")
                .map_err(|_| SchemaError::Database)?
            {
                return Err(SchemaError::Dirty(version));
            }
            let expected = MIGRATOR
                .iter()
                .find(|m| m.version == version)
                .ok_or(SchemaError::Unsupported(version))?;
            let actual: Vec<u8> = row.try_get("checksum").map_err(|_| SchemaError::Database)?;
            if actual.as_slice() != expected.checksum.as_ref() {
                return Err(SchemaError::Checksum(version));
            }
        }
        for expected in MIGRATOR.iter() {
            if !rows
                .iter()
                .any(|row| row.get::<i64, _>("version") == expected.version)
            {
                return Err(SchemaError::Pending(expected.version));
            }
        }
        tx.commit().await.map_err(|_| SchemaError::Database)?;
        Ok(())
    })
    .await
    .map_err(|_| SchemaError::Deadline)?
}
