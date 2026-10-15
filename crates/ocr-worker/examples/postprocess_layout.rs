//! Replay post-processing against a saved OCR result and its original input.
//! This runs the real Rust implementation without repeating OCR/model calls.
use ocr_worker::post_processing;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "usage: postprocess_layout ORIGINAL.pdf OCR_RESULT.json"
    );
    let input = tokio::fs::read(&args[0]).await?;
    let mut result: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&args[1]).await?)?;
    let media_type = if input.starts_with(b"%PDF-") {
        "application/pdf"
    } else {
        "image/png"
    };
    post_processing::apply(
        &mut result,
        &input,
        media_type,
        "vi_rental_contract_v1",
        30,
        CancellationToken::new(),
    )
    .await?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &result)?;
    Ok(())
}
