//! Bounded PG18 logical-backup/local-restore tooling. Never activates runtime.
use std::{path::Path, process::ExitCode, time::Duration};

use chrono::{DateTime, Utc};
use ocr_core::{
    administration,
    backup::{self, BackupContent},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

#[path = "backup/config.rs"]
mod config;
#[path = "backup/process.rs"]
mod process;

type Result<T> = std::result::Result<T, Error>;
const MANIFEST_LIMIT: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("unsupported or incomplete backup arguments; use --help")]
    Arguments,
    #[error("backup credentials or endpoint are unsupported (details redacted)")]
    Credentials,
    #[error(
        "backup file operation failed (details redacted); inspect completion state before retrying"
    )]
    File,
    #[error("backup key format is unsupported; use one native age1 recipient/identity")]
    Key,
    #[error("backup scratch must be a private owned Linux tmpfs directory")]
    Scratch,
    #[error("backup byte budget exceeded")]
    Bounds,
    #[error("backup manifest, ciphertext or restored content integrity check failed")]
    Integrity,
    #[error("backup supports PostgreSQL18 tools/server and age1 only")]
    ToolVersion,
    #[error("backup database operation failed (details redacted)")]
    Database,
    #[error("backup source requires the ocr_backup login")]
    SourceRole,
    #[error("backup total operation deadline elapsed; restore target must remain isolated")]
    Deadline,
    #[error("backup tool stage failed: {0}")]
    Tool(#[from] process::ProcessError),
    #[error("backup content stage failed: {0}")]
    Content(#[from] backup::BackupError),
    #[error("restore target preflight failed: {0}")]
    Target(#[from] administration::AdminError),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveDigest {
    bytes: u64,
    sha256: String,
}
impl From<&process::IoDigest> for ArchiveDigest {
    fn from(value: &process::IoDigest) -> Self {
        Self {
            bytes: value.bytes,
            sha256: value.sha256.clone(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    backup_id: Uuid,
    snapshot_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    pg_dump_version: String,
    age_version: String,
    plaintext: ArchiveDigest,
    ciphertext: ArchiveDigest,
    content: BackupContent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    schema_version: String,
    manifest_ciphertext: ArchiveDigest,
}

fn supported_age_version(value: &str) -> bool {
    let mut components = value.strip_prefix('v').unwrap_or(value).split('.');
    components.next() == Some("1")
        && components.clone().count() == 2
        && components.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn create_file(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).map_err(|_| Error::File)
}
fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = create_file(path)?;
    file.write_all(bytes).map_err(|_| Error::File)?;
    file.sync_all().map_err(|_| Error::File)
}
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| Error::File)
}

async fn checked_file(path: &Path, limit: u64) -> Result<(ArchiveDigest, std::fs::File)> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| Error::File)?;
    let metadata = file.metadata().map_err(|_| Error::File)?;
    if !metadata.is_file() {
        return Err(Error::File);
    }
    if metadata.len() > limit {
        return Err(Error::Bounds);
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer).await.map_err(|_| Error::File)?;
        if count == 0 {
            break;
        }
        bytes = bytes.checked_add(count as u64).ok_or(Error::Bounds)?;
        if bytes > limit {
            return Err(Error::Bounds);
        }
        hash.update(&buffer[..count]);
    }
    file.rewind().await.map_err(|_| Error::File)?;
    Ok((
        ArchiveDigest {
            bytes,
            sha256: format!("{:x}", hash.finalize()),
        },
        file.into_std().await,
    ))
}

async fn version(tool: &str, prefix: &str) -> Result<String> {
    let mut command = config::tool(tool);
    command.arg("--version");
    let bytes = process::run_capture(command, None, 256, Duration::from_secs(5)).await?;
    let version = std::str::from_utf8(&bytes)
        .map_err(|_| Error::ToolVersion)?
        .trim();
    if !(if tool == "age" {
        supported_age_version(version)
    } else {
        version.starts_with(prefix)
    }) || version.chars().any(char::is_control)
    {
        return Err(Error::ToolVersion);
    }
    Ok(version.to_owned())
}

async fn connect(database: &config::Database) -> Result<PgConnection> {
    tokio::time::timeout(
        Duration::from_secs(10),
        PgConnection::connect_with(&database.options),
    )
    .await
    .map_err(|_| Error::Deadline)?
    .map_err(|_| Error::Database)
}

fn encrypt_command(recipient: &Path) -> tokio::process::Command {
    let mut command = config::tool("age");
    command
        .arg("--encrypt")
        .arg("--recipients-file")
        .arg(recipient);
    command
}
fn decrypt_command(identity: &Path) -> tokio::process::Command {
    let mut command = config::tool("age");
    command.arg("--decrypt").arg("--identity").arg(identity);
    command
}

async fn create(args: &config::Args) -> Result<()> {
    let key = config::key_copy(&args.key_file, &args.scratch, false)?;
    let database = config::Database::load(&args.scratch)?;
    let pg_dump_version = version("pg_dump", "pg_dump (PostgreSQL) 18.").await?;
    let age_version = version("age", "v1.").await?;
    let mut connection = connect(&database).await?;
    let login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&mut connection)
        .await
        .map_err(|_| Error::Database)?;
    if login != "ocr_backup" {
        return Err(Error::SourceRole);
    }
    let mut transaction = connection.begin().await.map_err(|_| Error::Database)?;
    sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='30s'; SET LOCAL idle_in_transaction_session_timeout='0';")
        .execute(&mut *transaction).await.map_err(|_|Error::Database)?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock_shared(186878833,3)")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| Error::Database)?;
    if !locked {
        return Err(Error::Database);
    }
    let snapshot: String = sqlx::query_scalar("SELECT pg_export_snapshot()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| Error::Database)?;
    if snapshot.is_empty()
        || snapshot.len() > 128
        || !snapshot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        return Err(Error::Database);
    }
    let snapshot_at: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| Error::Database)?;
    let content = backup::content_manifest(&mut transaction).await?;
    // Content captures ACCESS SHARE locks through the exporter transaction.
    // Cooperative migrator/grant/bootstrap use the same operations lock; an
    // administrator bypassing it is outside this DDL/provenance boundary.
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&args.location).map_err(|_| Error::File)?;
    let partial = args.location.join("database.dump.age.partial");
    let output = tokio::fs::File::from_std(create_file(&partial)?);
    let mut dump = database.command("pg_dump");
    dump.args([
        "--format=custom",
        "--compress=zstd:3",
        "--no-acl",
        "--no-password",
        "--no-tablespaces",
        "--no-publications",
        "--no-subscriptions",
        "--lock-wait-timeout=2s",
    ])
    .arg("--snapshot")
    .arg(snapshot);
    let archive = process::encrypt_pipeline(
        dump,
        encrypt_command(key.path()),
        output,
        args.max_bytes,
        args.max_bytes,
        args.budget,
    )
    .await?;
    let completed_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| Error::Database)?;
    transaction.commit().await.map_err(|_| Error::Database)?;
    std::fs::rename(&partial, args.location.join("database.dump.age")).map_err(|_| Error::File)?;
    let manifest = Manifest {
        schema_version: "ocr-encrypted-backup/1.0.0".into(),
        backup_id: Uuid::new_v4(),
        snapshot_at,
        completed_at,
        pg_dump_version,
        age_version,
        plaintext: (&archive.plain).into(),
        ciphertext: (&archive.cipher).into(),
        content,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::Integrity)?;
    if bytes.len() > MANIFEST_LIMIT {
        return Err(Error::Bounds);
    }
    let ciphertext = process::run_capture(
        encrypt_command(key.path()),
        Some(bytes),
        MANIFEST_LIMIT,
        args.budget,
    )
    .await?;
    persist(&args.location.join("manifest.json.age"), &ciphertext)?;
    let seal = Seal {
        schema_version: "ocr-backup-seal/1.0.0".into(),
        manifest_ciphertext: ArchiveDigest {
            bytes: ciphertext.len() as u64,
            sha256: sha256(&ciphertext),
        },
    };
    let seal_bytes = serde_json::to_vec(&seal).map_err(|_| Error::Integrity)?;
    // Completion seal is published last. Hash is externally pinned on read;
    // this adjacent seal alone does not authenticate an archive's sender.
    persist(&args.location.join("complete.json"), &seal_bytes)?;
    sync_directory(&args.location)?;
    let parent = args
        .location
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_directory(parent)?;
    print_json(
        &serde_json::json!({"status":"backup_created","backup_id":manifest.backup_id,
        "manifest_ciphertext_sha256":seal.manifest_ciphertext.sha256,"ciphertext_bytes":archive.cipher.bytes,
        "plaintext_bytes":archive.plain.bytes,"snapshot_at":manifest.snapshot_at,"content":manifest.content,
        "production_backup_certified":false,"point_in_time_recovery":false}),
    )
}

struct VerifiedArchive {
    manifest: Manifest,
    // Hold both files until pg_restore finishes; key never enters argv/env.
    _identity: tempfile::NamedTempFile,
    plaintext: tempfile::NamedTempFile,
}

async fn verify_archive(args: &config::Args) -> Result<VerifiedArchive> {
    let key = config::key_copy(&args.key_file, &args.scratch, true)?;
    let _ = version("age", "v1.").await?;
    let seal: Seal = serde_json::from_slice(&config::regular_bytes(
        &args.location.join("complete.json"),
        4096,
    )?)
    .map_err(|_| Error::Integrity)?;
    if seal.schema_version != "ocr-backup-seal/1.0.0"
        || Some(seal.manifest_ciphertext.sha256.as_str()) != args.expected_hash.as_deref()
        || !config::digest(&seal.manifest_ciphertext.sha256)
        || seal.manifest_ciphertext.bytes > MANIFEST_LIMIT as u64
    {
        return Err(Error::Integrity);
    }
    let manifest_path = args.location.join("manifest.json.age");
    let encrypted = config::regular_bytes(&manifest_path, MANIFEST_LIMIT)?;
    if encrypted.len() as u64 != seal.manifest_ciphertext.bytes
        || sha256(&encrypted) != seal.manifest_ciphertext.sha256
    {
        return Err(Error::Integrity);
    }
    // Use the checked opened content through stdin, rather than reopen a
    // mutable manifest pathname in the age child.
    let mut decrypt_manifest = config::tool("age");
    decrypt_manifest
        .args(["--decrypt", "--identity"])
        .arg(key.path());
    let decoded = process::run_capture(
        decrypt_manifest,
        Some(encrypted),
        MANIFEST_LIMIT,
        args.budget,
    )
    .await?;
    let manifest: Manifest = serde_json::from_slice(&decoded).map_err(|_| Error::Integrity)?;
    if manifest.schema_version != "ocr-encrypted-backup/1.0.0"
        || manifest.backup_id.is_nil()
        || manifest.completed_at < manifest.snapshot_at
        || !manifest
            .pg_dump_version
            .starts_with("pg_dump (PostgreSQL) 18.")
        || !supported_age_version(&manifest.age_version)
        || manifest.plaintext.bytes == 0
        || manifest.ciphertext.bytes == 0
        || !config::digest(&manifest.plaintext.sha256)
        || !config::digest(&manifest.ciphertext.sha256)
        || manifest.plaintext.bytes > args.max_bytes
        || manifest.ciphertext.bytes > args.max_bytes
    {
        return Err(Error::Integrity);
    }
    manifest.content.validate()?;
    let archive_path = args.location.join("database.dump.age");
    let (cipher, checked_cipher) = checked_file(&archive_path, args.max_bytes).await?;
    if cipher.bytes != manifest.ciphertext.bytes || cipher.sha256 != manifest.ciphertext.sha256 {
        return Err(Error::Integrity);
    }
    let plaintext = config::private_file(&args.scratch, &[])?;
    let output = tokio::fs::File::from_std(plaintext.reopen().map_err(|_| Error::File)?);
    // age can emit authenticated early chunks before a later failure. Entire
    // decryption/hash validation must complete before any restore SQL runs.
    let plain = process::run_file_to_file(
        decrypt_command(key.path()),
        checked_cipher,
        output,
        args.max_bytes,
        args.budget,
    )
    .await?;
    if plain.bytes != manifest.plaintext.bytes || plain.sha256 != manifest.plaintext.sha256 {
        return Err(Error::Integrity);
    }
    Ok(VerifiedArchive {
        manifest,
        _identity: key,
        plaintext,
    })
}

async fn restore(args: &config::Args) -> Result<()> {
    let _ = version("pg_restore", "pg_restore (PostgreSQL) 18.").await?;
    // Corruption/wrong key fail before connecting to the destination.
    let verified = verify_archive(args).await?;
    let database = config::Database::load(&args.scratch)?;
    let mut connection = connect(&database).await?;
    let server: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&mut connection)
        .await
        .map_err(|_| Error::Database)?;
    if server / 10000 != 18 {
        return Err(Error::ToolVersion);
    }
    administration::prepare_restore_target(
        &mut connection,
        args.expected_database.as_deref().ok_or(Error::Arguments)?,
    )
    .await?;
    let mut restore = database.command("pg_restore");
    restore
        .args([
            "--single-transaction",
            "--exit-on-error",
            "--no-owner",
            "--no-acl",
            "--no-password",
            "--no-tablespaces",
            "--role=ocr_owner",
        ])
        .arg("--dbname")
        .arg(args.expected_database.as_deref().ok_or(Error::Arguments)?)
        .arg("--")
        .arg(verified.plaintext.path());
    let stdout = process::run_capture(restore, None, 4096, args.budget).await?;
    if !stdout.is_empty() {
        return Err(Error::Integrity);
    }
    let mut transaction = connection.begin().await.map_err(|_| Error::Database)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|_| Error::Database)?;
    let restored = backup::content_manifest(&mut transaction).await?;
    transaction.commit().await.map_err(|_| Error::Database)?;
    if restored != verified.manifest.content {
        return Err(Error::Integrity);
    }
    print_json(
        &serde_json::json!({"status":"restored_content_verified","backup_id":verified.manifest.backup_id,
        "content":restored,"runtime_granted":false,"ready_for_service":false,
        "post_snapshot_deletion_reconciliation":"required_external_authoritative_source",
        "retention_apply_enabled":false,"production_restore_certified":false}),
    )
}

fn print_json(value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value).map_err(|_| Error::File)?;
    output.write_all(b"\n").map_err(|_| Error::File)
}

async fn run() -> Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(|value| value.into_string().map_err(|_| Error::Arguments))
        .collect::<Result<Vec<_>>>()?;
    if args == ["--help"] || args == ["-h"] {
        println!("{}", config::HELP);
        return Ok(());
    }
    let args = config::parse(args)?;
    config::require_tmpfs(&args.scratch)?;
    tokio::time::timeout(args.budget,async {
        match args.operation.as_str() {
            "create" => create(&args).await,
            "restore" => restore(&args).await,
            "verify" => {
                let verified = verify_archive(&args).await?;
                print_json(&serde_json::json!({"status":"encrypted_archive_verified","backup_id":verified.manifest.backup_id,
                    "content":verified.manifest.content,"database_connected":false,"ready_for_service":false}))
            }
            _ => Err(Error::Arguments),
        }
    }).await.map_err(|_|Error::Deadline)?
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ocr-backup: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_native_age1_tool_versions() {
        for version in ["v1.2.1", "1.2.1"] {
            assert!(supported_age_version(version));
        }
        for version in ["v2.0.0", "1.2.1\nmalicious", "1.2", "1.foo.1", ""] {
            assert!(!supported_age_version(version));
        }
    }
    #[tokio::test]
    async fn checked_cipher_is_bounded_regular_and_same_open_file_after_replacement() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cipher");
        std::fs::write(&path, b"checked-cipher").unwrap();
        assert!(matches!(checked_file(&path, 1).await, Err(Error::Bounds)));
        let (digest, mut file) = checked_file(&path, 64).await.unwrap();
        assert_eq!(digest.sha256, sha256(b"checked-cipher"));
        std::fs::rename(&path, dir.path().join("old")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"checked-cipher");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, dir.path().join("symlink")).unwrap();
            assert!(matches!(
                checked_file(&dir.path().join("symlink"), 64).await,
                Err(Error::File)
            ));
        }
        assert!(matches!(
            checked_file(dir.path(), 64).await,
            Err(Error::File)
        ));
    }
}
