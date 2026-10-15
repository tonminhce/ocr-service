//! Offline local Compose provisioning; does not connect to any service.
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    path::Path,
    process::ExitCode,
};

use uuid::Uuid;

fn token() -> String {
    // UUID v4 uses the OS random source. Two UUIDs retain 244 random bits.
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn initialize(path: &Path) -> Result<usize, &'static str> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    // Deliberately refuse an existing path, including symlinks. Never rotate or
    // overwrite a running deployment's credentials as a side effect of init.
    builder.create(path).map_err(
        |_| "cannot create a new secrets directory; parent must exist and target must not exist",
    )?;
    let mut values = Vec::new();
    let admin_password = token();
    values.push((
        "postgres_admin_password".to_string(),
        admin_password.clone(),
    ));
    values.push((
        "admin_database_url".to_string(),
        format!("postgres://ocr_admin:{admin_password}@postgres:5432/ocr"),
    ));
    for role in ["migrator", "api", "worker", "maintenance", "backup"] {
        let password = token();
        values.push((format!("{role}_password"), password.clone()));
        values.push((
            format!("{role}_database_url"),
            format!("postgres://ocr_{role}:{password}@postgres:5432/ocr"),
        ));
    }
    let redis_password = token();
    values.push(("redis_password".to_string(), redis_password.clone()));
    values.push((
        "redis_url".to_string(),
        format!("redis://:{redis_password}@redis:6379/0"),
    ));
    for name in ["api_token", "review_token", "metrics_token"] {
        values.push((name.to_string(), token()));
    }
    // This must be replaced with the existing inference server credential.
    // Generating another key here would falsely suggest model authorization.
    values.push((
        "vllm_api_key".to_string(),
        "replace-with-existing-inference-server-key".to_string(),
    ));
    let count = values.len();
    for (name, value) in values {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Compose file-backed secrets do not remap UID/mode. The parent is
            // 0700 on the host; each selectively mounted file must be readable
            // by the container's non-root UID 10001.
            options.mode(0o444);
        }
        let mut file = options
            .open(path.join(name))
            .map_err(|_| "cannot create secret file; partial directory retained")?;
        writeln!(file, "{value}")
            .map_err(|_| "cannot persist secret file; partial directory retained")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o444))
                .map_err(|_| "cannot set secret mount permissions")?;
        }
        file.sync_all()
            .map_err(|_| "cannot persist secret file; partial directory retained")?;
    }
    // Persist directory entries on filesystems that support directory fsync.
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "cannot persist secrets directory")?;
    Ok(count)
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!(
            "ocr-secrets init <new-directory>\nCreates local Compose secret files; never overwrites. Replace vllm_api_key before real inference."
        );
        return ExitCode::SUCCESS;
    }
    if args.len() != 2 || args[0] != "init" {
        eprintln!("usage: ocr-secrets init <new-directory>");
        return ExitCode::FAILURE;
    }
    match initialize(Path::new(&args[1])) {
        Ok(count) => {
            println!(
                "created {count} secret files; replace vllm_api_key before real inference; no services were changed"
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_consistent_secrets_and_refuses_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("secrets");
        assert_eq!(initialize(&directory).unwrap(), 18);
        let api_password = fs::read_to_string(directory.join("api_password")).unwrap();
        assert_eq!(api_password.trim_end().len(), 64);
        assert!(
            api_password
                .trim_end()
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        let api_url = fs::read_to_string(directory.join("api_database_url")).unwrap();
        assert!(api_url.contains(&format!("ocr_api:{}@postgres", api_password.trim_end())));
        assert_ne!(
            fs::read(directory.join("api_token")).unwrap(),
            fs::read(directory.join("review_token")).unwrap()
        );
        assert!(initialize(&directory).is_err());
        assert_eq!(
            fs::read_to_string(directory.join("api_database_url")).unwrap(),
            api_url
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(directory.join("api_token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
        }
    }
}
