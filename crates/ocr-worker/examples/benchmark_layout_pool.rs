//! Local layout-only benchmark; never invokes the recognition/model HTTP API.
//! Uses the normal Settings environment and ORT_DYLIB_PATH. Source pixels and
//! layout content are not printed; timings and region counts are safe to share.

use std::{sync::Arc, time::Instant};

use anyhow::{Context, Result, ensure};
use ocr_worker::{layout_runner::LayoutPool, settings::Settings};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .context("usage: benchmark_layout_pool IMAGE [SAMPLES]")?;
    let count: usize = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(5);
    ensure!((2..=100).contains(&count), "samples must be 2..=100");
    ensure!(args.next().is_none(), "unexpected argument");
    let settings = Settings::from_env()?;
    let page = Arc::new(image::open(input)?.to_rgb8());
    let slots = Arc::new(Semaphore::new(2));
    let warm_pool = LayoutPool::new(&settings);
    let mut warm = Vec::new();
    let mut expected = None;
    let mut cold_start = None;
    let mut regions = 0;
    for iteration in 0..=count {
        let start = Instant::now();
        let (layout, timings) = warm_pool
            .detect_with_timings(page.clone(), slots.clone(), CancellationToken::new())
            .await?;
        let total = start.elapsed().as_secs_f64();
        let bytes = serde_json::to_vec(&layout)?;
        if let Some(expected) = expected.as_ref() {
            ensure!(&bytes == expected, "layout differs across warm calls");
        } else {
            expected = Some(bytes);
            regions = layout.regions.len() + layout.discarded.len();
        }
        if iteration == 0 {
            cold_start =
                Some(json!({"total_seconds":total,"startup_seconds":timings.startup_seconds}));
        } else {
            warm.push(total);
        }
    }
    warm_pool.shutdown().await;
    let mut cold = Vec::new();
    for _ in 0..count {
        let pool = LayoutPool::new(&settings);
        let start = Instant::now();
        let layout = pool
            .detect(page.clone(), slots.clone(), CancellationToken::new())
            .await?;
        let total = start.elapsed().as_secs_f64();
        ensure!(
            Some(serde_json::to_vec(&layout)?) == expected,
            "layout differs across cold calls"
        );
        pool.shutdown().await;
        cold.push(total);
    }
    warm.sort_by(f64::total_cmp);
    cold.sort_by(f64::total_cmp);
    let warm_median = warm[count / 2];
    let cold_median = cold[count / 2];
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version":"layout-pool-benchmark/1.0.0",
            "samples":count,"build":if cfg!(debug_assertions) {"debug"} else {"release"},
            "page_width":page.width(),"page_height":page.height(),"regions":regions,
            "model_sha256":settings.layout_model_sha256,"cold_start":cold_start,
            "cold_seconds":cold,"warm_seconds":warm,"outputs_identical":true,
            "cold_median_seconds":cold_median,"warm_median_seconds":warm_median,
            "speedup_layout_only":cold_median/warm_median,
            "scope":"sequential layout only; no OCR inference, no document throughput claim"
        }))?
    );
    Ok(())
}
