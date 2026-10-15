//! Replay post-processing against a saved OCR result and its original input.
//! This runs the real Rust implementation without repeating OCR/model calls.
use ocr_worker::{
    parser_client::{ParserLimits, ParserSession},
    parser_service, post_processing,
};
use std::sync::Arc;
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
    let parser = ParserSession::open(
        &parser_service::socket_path_from_env()?,
        input,
        media_type,
        ParserLimits::default(),
        CancellationToken::new(),
    )
    .await?;
    let source_sha256 = parser.source_sha256().to_owned();
    post_processing::apply(
        &mut result,
        media_type,
        "vi_rental_contract_v1",
        Some(Arc::new(parser)),
        source_sha256,
        CancellationToken::new(),
    )
    .await?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &result)?;
    Ok(())
}
