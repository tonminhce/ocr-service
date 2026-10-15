use ocr_core::{administration, secrets};

#[path = "admin/retention_cli.rs"]
mod retention_cli;

const HELP: &str = "ocr-admin schema check|migrate|migrate-dev\nocr-admin roles bootstrap|grant\n\nDATABASE_URL or DATABASE_URL_FILE is required. bootstrap/grant use a dedicated\ndatabase administrator connection; migrate requires ocr_migrator. migrate-dev is\nan explicit superuser-only local/demo compatibility command, never a production\nstartup mode. bootstrap also requires OCR_MIGRATOR_PASSWORD, OCR_API_PASSWORD,\nOCR_WORKER_PASSWORD, OCR_MAINTENANCE_PASSWORD, OCR_BACKUP_PASSWORD (or each _FILE).\nMissing roles are created; existing credentials are never rotated. No automatic\nadoption, rotation, retention or deletion occurs.";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("ocr-admin: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("{HELP}\n\n{}", retention_cli::HELP);
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "retention") {
        return retention_cli::run(&args[1..]).await;
    }
    let command = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["schema", "check"] => 0,
        ["schema", "migrate"] => 1,
        ["roles", "bootstrap"] => 2,
        ["roles", "grant"] => 3,
        ["schema", "migrate-dev"] => 4,
        _ => return Err(HELP.into()),
    };
    let url = secrets::required("DATABASE_URL")?;
    if command == 0 {
        administration::check(&url).await?;
    } else {
        let mut connection = administration::connect(&url).await?;
        match command {
            1 => administration::migrate(&mut connection).await?,
            2 => {
                let mut values = Vec::new();
                for name in administration::PASSWORD_NAMES {
                    values.push(secrets::required(name)?);
                }
                let passwords: [String; 5] = values
                    .try_into()
                    .map_err(|_| "invalid password configuration")?;
                administration::bootstrap(&mut connection, &passwords).await?;
            }
            3 => administration::grant(&mut connection).await?,
            4 => administration::migrate_dev(&mut connection).await?,
            _ => unreachable!(),
        }
    }
    println!(
        "{{\"status\":\"ok\",\"operation\":\"{} {}\",\"schema_max_version\":{}}}",
        args[0],
        args[1],
        ocr_core::schema::MIGRATOR
            .iter()
            .map(|m| m.version)
            .max()
            .unwrap_or(0)
    );
    Ok(())
}
