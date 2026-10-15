//! Narrow, secret-safe configuration for the PG18/age operations image.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::postgres::PgConnectOptions;
use tokio::process::Command;

use super::{Error, Result};

pub const HELP: &str = "ocr-backup create --output NEW_DIRECTORY --recipient-file FILE [BOUNDS]\nocr-backup verify --bundle DIRECTORY --identity-file FILE --manifest-sha256 TRUSTED_SHA256 [BOUNDS]\nocr-backup restore --bundle DIRECTORY --identity-file FILE --manifest-sha256 TRUSTED_SHA256 --expected-empty-database NAME [BOUNDS]\n\nBOUNDS: --scratch-dir /scratch --timeout-seconds 600 --max-bytes 10737418240\nLinux tmpfs scratch is required. create uses the ocr_backup DATABASE_URL_FILE;\nrestore uses a fresh isolated ocr_migrator DATABASE_URL_FILE. verify needs no DB.\nTools are fixed PG18 pg_dump/pg_restore and age1, never a shell. Existing bundles\nare never overwritten. Restore does not clean, migrate, grant runtime, reconcile\npost-snapshot deletions, or activate a deployment. Obtain the expected manifest\nciphertext hash independently from a trusted source; age does not sign origins.";

pub struct Args {
    pub operation: String,
    pub location: PathBuf,
    pub key_file: PathBuf,
    pub expected_hash: Option<String>,
    pub expected_database: Option<String>,
    pub scratch: PathBuf,
    pub budget: Duration,
    pub max_bytes: u64,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn parse(args: Vec<String>) -> Result<Args> {
    let Some(operation) = args.first() else {
        return Err(Error::Arguments);
    };
    if !matches!(operation.as_str(), "create" | "verify" | "restore") {
        return Err(Error::Arguments);
    }
    let mut values = BTreeMap::new();
    for pair in args[1..].chunks(2) {
        let [key, value] = pair else {
            return Err(Error::Arguments);
        };
        let allowed = matches!(
            key.as_str(),
            "--scratch-dir" | "--timeout-seconds" | "--max-bytes"
        ) || if operation == "create" {
            matches!(key.as_str(), "--output" | "--recipient-file")
        } else {
            matches!(
                key.as_str(),
                "--bundle" | "--identity-file" | "--manifest-sha256"
            ) || (operation == "restore" && key == "--expected-empty-database")
        };
        if !allowed || value.is_empty() || values.insert(key.as_str(), value.as_str()).is_some() {
            return Err(Error::Arguments);
        }
    }
    let required = |name| values.get(name).copied().ok_or(Error::Arguments);
    let timeout = values
        .get("--timeout-seconds")
        .unwrap_or(&"600")
        .parse::<u64>()
        .map_err(|_| Error::Arguments)?;
    let max_bytes = values
        .get("--max-bytes")
        .unwrap_or(&"10737418240")
        .parse::<u64>()
        .map_err(|_| Error::Arguments)?;
    if !(1..=3600).contains(&timeout) || !(1..=1_099_511_627_776).contains(&max_bytes) {
        return Err(Error::Arguments);
    }
    let expected_hash = if operation != "create" {
        let value = required("--manifest-sha256")?;
        if !digest(value) {
            return Err(Error::Arguments);
        }
        Some(value.to_owned())
    } else {
        None
    };
    let expected_database = if operation == "restore" {
        let value = required("--expected-empty-database")?;
        if !identifier(value) {
            return Err(Error::Arguments);
        }
        Some(value.to_owned())
    } else {
        None
    };
    Ok(Args {
        operation: operation.clone(),
        location: required(if operation == "create" {
            "--output"
        } else {
            "--bundle"
        })?
        .into(),
        key_file: required(if operation == "create" {
            "--recipient-file"
        } else {
            "--identity-file"
        })?
        .into(),
        expected_hash,
        expected_database,
        scratch: values
            .get("--scratch-dir")
            .copied()
            .unwrap_or("/scratch")
            .into(),
        budget: Duration::from_secs(timeout),
        max_bytes,
    })
}

pub fn regular_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| Error::File)?;
    if !file.metadata().map_err(|_| Error::File)?.is_file() {
        return Err(Error::File);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::File)?;
    if bytes.len() > limit {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}

pub fn private_file(directory: &Path, content: &[u8]) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|_| Error::File)?;
    file.write_all(content).map_err(|_| Error::File)?;
    file.as_file().sync_all().map_err(|_| Error::File)?;
    Ok(file)
}

pub fn key_copy(path: &Path, scratch: &Path, identity: bool) -> Result<tempfile::NamedTempFile> {
    let bytes = regular_bytes(path, 65536)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Key)?;
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();
    if lines.len() != 1 {
        return Err(Error::Key);
    }
    let key = lines[0];
    let valid = if identity {
        key.starts_with("AGE-SECRET-KEY-1")
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    } else {
        key.starts_with("age1")
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    };
    if !valid || !(50..=128).contains(&key.len()) {
        return Err(Error::Key);
    }
    private_file(scratch, key.as_bytes())
}

pub fn require_tmpfs(directory: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let metadata = std::fs::symlink_metadata(directory).map_err(|_| Error::Scratch)?;
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(Error::Scratch);
        }
        let path =
            std::ffi::CString::new(directory.as_os_str().as_bytes()).map_err(|_| Error::Scratch)?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0
            || unsafe { stat.assume_init() }.f_type != libc::TMPFS_MAGIC
        {
            return Err(Error::Scratch);
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = directory;
        Err(Error::Scratch)
    }
}

pub fn tool(name: &str) -> Command {
    let mut command = Command::new(format!("/usr/bin/{name}"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8");
    command
}

pub struct Database {
    pub options: PgConnectOptions,
    host: String,
    port: u16,
    user: String,
    database: String,
    sslmode: String,
    passfile: tempfile::NamedTempFile,
}

fn decode(value: &str) -> Result<String> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let a = bytes
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or(Error::Credentials)?;
            let b = bytes
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or(Error::Credentials)?;
            result.push((a * 16 + b) as u8);
        } else {
            result.push(byte)
        }
    }
    let value = String::from_utf8(result).map_err(|_| Error::Credentials)?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Credentials);
    }
    Ok(value)
}

// Reject inherited libpq/SQLx connection configuration before either client is
// initialized. PostgreSQL image metadata does not configure client connections.
fn connection_environment_name(name: &str) -> bool {
    name.starts_with("PG") && !matches!(name, "PG_MAJOR" | "PG_VERSION" | "PG_SHA256" | "PGDATA")
}

fn pass_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace(':', "\\:")
}

impl Database {
    pub fn load(scratch: &Path) -> Result<Self> {
        let uri = ocr_core::secrets::required("DATABASE_URL").map_err(|_| Error::Credentials)?;
        Self::from_uri(&uri, scratch)
    }
    fn from_uri(uri: &str, scratch: &Path) -> Result<Self> {
        if std::env::vars_os().any(|(name, _)| connection_environment_name(&name.to_string_lossy()))
        {
            return Err(Error::Credentials);
        }
        let parsed = url::Url::parse(uri).map_err(|_| Error::Credentials)?;
        // Narrow common subset prevents libpq connection-string expansion and
        // different effective endpoints between SQLx and PostgreSQL utilities.
        if !matches!(parsed.scheme(), "postgres" | "postgresql")
            || parsed.fragment().is_some()
            || parsed.host_str().is_none()
            || parsed.query_pairs().any(|(key, _)| key != "sslmode")
            || parsed.query_pairs().count() > 1
        {
            return Err(Error::Credentials);
        }
        let user = decode(parsed.username())?;
        let database = decode(parsed.path().strip_prefix('/').ok_or(Error::Credentials)?)?;
        let password = decode(parsed.password().ok_or(Error::Credentials)?)?;
        if !matches!(user.as_str(), "ocr_backup" | "ocr_migrator") || !identifier(&database) {
            return Err(Error::Credentials);
        }
        let options = ocr_core::postgres::safe_options(uri)
            .map_err(|_| Error::Credentials)?
            .application_name("ocr-backup-controller");
        if options.get_socket().is_some()
            || options.get_username() != user
            || options.get_database() != Some(database.as_str())
        {
            return Err(Error::Credentials);
        }
        let host = options.get_host().to_owned();
        let port = options.get_port();
        if host.is_empty()
            || host.len() > 253
            || port == 0
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_:[]".contains(&b))
        {
            return Err(Error::Credentials);
        }
        let sslmode = parsed
            .query_pairs()
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| "prefer".into());
        if !matches!(
            sslmode.as_str(),
            "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full"
        ) {
            return Err(Error::Credentials);
        }
        let line = format!(
            "{}:{}:{}:{}:{}\n",
            pass_escape(&host),
            port,
            pass_escape(&database),
            pass_escape(&user),
            pass_escape(&password)
        );
        let passfile = private_file(scratch, line.as_bytes())?;
        Ok(Self {
            options,
            host,
            port,
            user,
            database,
            sslmode,
            passfile,
        })
    }
    pub fn command(&self, name: &str) -> Command {
        let mut command = tool(name);
        command.env("PGHOST", &self.host).env("PGPORT", self.port.to_string())
            .env("PGUSER", &self.user).env("PGDATABASE", &self.database)
            .env("PGSSLMODE", &self.sslmode).env("PGPASSFILE", self.passfile.path())
            .env("PGCONNECT_TIMEOUT", "10").env("PGAPPNAME", "ocr-backup-tool")
            .env("PGOPTIONS", "-c lock_timeout=2000 -c statement_timeout=300000 -c idle_in_transaction_session_timeout=0");
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn libpq_config_matches_decoded_sqlx_endpoint_and_keeps_password_out_of_child_env() {
        let dir = tempfile::tempdir().unwrap();
        let database = Database::from_uri("postgres://ocr_backup:SECRET%3A%5C%2B%C3%A9@localhost:5433/ocr_restore?sslmode=disable",dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(database.passfile.path()).unwrap(),
            "localhost:5433:ocr_restore:ocr_backup:SECRET\\:\\\\+é\n"
        );
        let command = database.command("pg_dump");
        let config = format!("{command:?}");
        assert!(!config.contains("SECRET"));
        assert!(!config.contains("PGPASSWORD"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(database.passfile.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn endpoint_overrides_connstrings_and_unknown_parameters_are_rejected_without_values() {
        let dir = tempfile::tempdir().unwrap();
        for uri in [
            "postgres://ocr_backup:SENTINEL@localhost/ocr?password=VALUE",
            "postgres://ocr_backup:SENTINEL@localhost/dbname%3Dother",
            "postgres://ocr_backup:SENTINEL@localhost/ocr?sslmode=disable&sslmode=require",
            "postgres://ocr_backup:SENTINEL%0A@localhost/ocr",
        ] {
            let error = Database::from_uri(uri, dir.path()).err().unwrap();
            assert!(!error.to_string().contains("SENTINEL"));
        }
    }
    #[test]
    fn inherited_connection_settings_fail_but_image_metadata_is_allowed() {
        for name in [
            "PGOPTIONS",
            "PGSSLMODE",
            "PGSSLROOTCERT",
            "PGSERVICE",
            "PGPASSFILE",
            "PGPASSWORD",
            "PGHOSTADDR",
        ] {
            assert!(connection_environment_name(name));
        }
        for name in [
            "PG_MAJOR",
            "PG_VERSION",
            "PG_SHA256",
            "PGDATA",
            "PATH",
            "DATABASE_URL_FILE",
        ] {
            assert!(!connection_environment_name(name));
        }
    }
    #[test]
    fn parser_requires_pinned_manifest_target_and_bounded_unique_flags() {
        for args in [
            vec!["restore", "--bundle", "x"],
            vec![
                "create",
                "--output",
                "x",
                "--recipient-file",
                "y",
                "--max-bytes",
                "0",
            ],
            vec![
                "create",
                "--output",
                "x",
                "--output",
                "z",
                "--recipient-file",
                "y",
            ],
        ] {
            assert!(parse(args.into_iter().map(String::from).collect()).is_err());
        }
        assert!(!identifier("dbname=other"));
        assert!(!digest(&"A".repeat(64)));
    }
}
