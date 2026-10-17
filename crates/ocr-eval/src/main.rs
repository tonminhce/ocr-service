use anyhow::{Context, Result, bail, ensure};
use ocr_eval::{
    Dataset, Split,
    arrival::{ArrivalConfig, ArrivalPattern, run as run_arrival},
    benchmark::{BenchmarkConfig, run as run_closed_loop},
    score,
};
use std::{collections::BTreeMap, path::Path};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context(
        "usage: ocr-eval score|score-v2|benchmark|arrival MANIFEST --split dev|calibration|test [options]",
    )?;
    let manifest = args.next().context("manifest path is required")?;
    let mut options = BTreeMap::new();
    while let Some(name) = args.next() {
        ensure!(name.starts_with("--"), "options must use --name value");
        let value = args.next().context("missing option value")?;
        ensure!(options.insert(name, value).is_none(), "duplicate option");
    }
    let split = Split::parse(&options.remove("--split").context("--split is required")?)?;
    if command == "score-v2" {
        ensure!(options.is_empty(), "unknown score-v2 option");
        let dataset = ocr_eval::v2::Dataset::load(Path::new(&manifest))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&ocr_eval::v2::score(&dataset, split)?)?
        );
        return Ok(());
    }
    let dataset = Dataset::load(Path::new(&manifest))?;
    let output = match command.as_str() {
        "score" => {
            ensure!(options.is_empty(), "unknown score option");
            serde_json::to_value(score(&dataset, split)?)?
        }
        "benchmark" => {
            let base = options
                .remove("--base-url")
                .context("--base-url is required")?;
            let clients = number(&mut options, "--clients", None)?;
            let deadline = number(&mut options, "--deadline-seconds", Some(600))?;
            let worker = number(&mut options, "--worker-concurrency", None)?;
            let region = number(&mut options, "--region-concurrency", None)?;
            let profile = options
                .remove("--profile")
                .unwrap_or_else(|| "vi_rental_contract_v1".into());
            ensure!(
                options.is_empty(),
                "unknown benchmark option; credentials must use OCR_API_TOKEN environment variable"
            );
            let token = std::env::var("OCR_API_TOKEN")
                .context("OCR_API_TOKEN environment variable is required")?;
            let config = BenchmarkConfig::new(
                &base,
                clients,
                deadline as u64,
                worker,
                region,
                profile,
                token,
            )?;
            serde_json::to_value(run_closed_loop(&dataset, split, &config).await?)?
        }
        "arrival" => {
            let base = options
                .remove("--base-url")
                .context("--base-url is required")?;
            let clients = number(&mut options, "--clients", None)?;
            let deadline = number(&mut options, "--deadline-seconds", Some(600))?;
            let worker = number(&mut options, "--worker-concurrency", None)?;
            let region = number(&mut options, "--region-concurrency", None)?;
            let profile = options
                .remove("--profile")
                .unwrap_or_else(|| "vi_rental_contract_v1".into());
            let pattern = match options
                .remove("--arrival-mode")
                .as_deref()
                .unwrap_or("fixed")
            {
                "fixed" => ArrivalPattern::FixedRate,
                "poisson" => ArrivalPattern::SeededPoisson,
                _ => bail!("--arrival-mode must be fixed or poisson"),
            };
            let rate = options
                .remove("--rate-per-second")
                .context("--rate-per-second is required")?
                .parse::<f64>()
                .context("--rate-per-second must be numeric")?;
            let duration = number(&mut options, "--duration-seconds", None)? as u64;
            let max_jobs = number(&mut options, "--max-jobs", None)?;
            let max_upload_bytes = number_u64(&mut options, "--max-upload-bytes", None)?;
            let max_pending = number(&mut options, "--max-pending-tasks", None)?;
            let max_outstanding = number(&mut options, "--max-outstanding-jobs", Some(100))?;
            let drain_deadline = number(&mut options, "--drain-deadline-seconds", Some(300))?;
            let seed = match options.remove("--seed") {
                Some(value) => value
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("--seed must be an unsigned integer"))?,
                None => {
                    ensure!(
                        pattern != ArrivalPattern::SeededPoisson,
                        "--seed is required for poisson arrival mode"
                    );
                    0
                }
            };
            ensure!(options.is_empty(), "unknown arrival option");
            let token = std::env::var("OCR_API_TOKEN")
                .context("OCR_API_TOKEN environment variable is required")?;
            let benchmark_config = BenchmarkConfig::new(
                &base,
                clients,
                deadline as u64,
                worker,
                region,
                profile,
                token,
            )?;
            let arrival_config = ArrivalConfig::new(
                pattern,
                rate,
                duration,
                max_jobs,
                max_upload_bytes,
                max_pending,
                seed,
            )?
            .with_registry_limits(max_outstanding, drain_deadline as u64)?;
            serde_json::to_value(
                run_arrival(&dataset, split, &benchmark_config, &arrival_config).await?,
            )?
        }
        _ => bail!("command must be score, score-v2, benchmark or arrival"),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
fn number(
    options: &mut BTreeMap<String, String>,
    key: &str,
    default: Option<usize>,
) -> Result<usize> {
    match options.remove(key) {
        Some(value) => value
            .parse()
            .with_context(|| format!("{key} must be an unsigned integer")),
        None => default.with_context(|| format!("{key} is required")),
    }
}

fn number_u64(
    options: &mut BTreeMap<String, String>,
    key: &str,
    default: Option<u64>,
) -> Result<u64> {
    match options.remove(key) {
        Some(value) => value
            .parse()
            .map_err(|_| anyhow::anyhow!("{key} must be an unsigned integer")),
        None => default.with_context(|| format!("{key} is required")),
    }
}
