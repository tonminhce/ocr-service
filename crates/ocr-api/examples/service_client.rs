//! A service-to-service caller: submit once with a stable retry key, then poll.
use std::{
    env,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use ocr_core::{domain::Job, projection::JobStatus};
use reqwest::multipart::{Form, Part};

#[tokio::main]
async fn main() -> Result<()> {
    if env::args().nth(1).as_deref() == Some("--healthcheck") {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?
            .get("http://127.0.0.1:8080/health/ready")
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }
    let path = env::args()
        .nth(1)
        .context("usage: service-client <PDF-or-image-path>")?;
    let token = ocr_core::secrets::required("OCR_API_TOKEN")?;
    let base = env::var("OCR_API_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let key = env::var("IDEMPOTENCY_KEY").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let filename = std::path::Path::new(&path)
        .file_name()
        .context("file needs a name")?
        .to_string_lossy()
        .into_owned();
    let bytes = tokio::fs::read(&path).await.context("read input file")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(35))
        .build()?;
    let profile = env::var("EXTRACTION_PROFILE").unwrap_or_else(|_| "none".into());
    let response = client
        .post(format!("{}/v1/ocr/jobs", base.trim_end_matches('/')))
        .bearer_auth(&token)
        .header("idempotency-key", &key)
        .multipart(
            Form::new()
                .text("extraction_profile", profile)
                .part("file", Part::bytes(bytes).file_name(filename)),
        )
        .send()
        .await?;
    let status = response.status();
    ensure!(
        status.is_success(),
        "OCR submission failed with HTTP {status}: {}",
        response.text().await?
    );
    let accepted: Job = response.json().await.context("decode accepted job")?;
    let job_id = accepted.id;
    let mut status = accepted.status;
    let mut error = accepted.error;
    let limit = env::var("CLIENT_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "600".into())
        .parse::<u64>()?;
    let deadline = Instant::now() + Duration::from_secs(limit);
    loop {
        match status.as_str() {
            "succeeded" => {
                let result: serde_json::Value = client
                    .get(format!(
                        "{}/v1/ocr/jobs/{job_id}/artifacts/result",
                        base.trim_end_matches('/')
                    ))
                    .bearer_auth(&token)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                if let Ok(expected) = env::var("EXPECTED_PAGES") {
                    let count = result["pages"].as_array().context("missing pages")?.len();
                    ensure!(
                        count == expected.parse::<usize>()?,
                        "expected {expected} pages, got {count}"
                    );
                }
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            "failed" => bail!("OCR job {} failed: {}", job_id, error.unwrap_or_default()),
            "queued" | "processing" => {}
            other => bail!("unknown OCR job state: {other}"),
        }
        ensure!(
            Instant::now() < deadline,
            "client polling deadline expired for job {} (the durable job continues)",
            job_id
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let response = client
            .get(format!(
                "{}/v1/ocr/jobs/{}/status",
                base.trim_end_matches('/'),
                job_id
            ))
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?;
        let snapshot: JobStatus = response.json().await?;
        status = snapshot.status;
        error = snapshot.error;
    }
}
