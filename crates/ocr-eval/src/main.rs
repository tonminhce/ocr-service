use anyhow::{Context, Result, bail, ensure};
use ocr_eval::{
    Dataset, Split,
    benchmark::{BenchmarkConfig, run},
    score,
};
use std::{collections::BTreeMap, path::Path};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context(
        "usage: ocr-eval score|score-v2|benchmark MANIFEST --split dev|calibration|test [options]",
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
            serde_json::to_value(run(&dataset, split, &config).await?)?
        }
        _ => bail!("command must be score, score-v2 or benchmark"),
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
