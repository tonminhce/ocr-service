//! Bounded end-to-end API load runner. It never changes worker/model settings,
//! fetches result artifacts, prints credentials, or retries document submission.
use crate::{Dataset, Document, Split};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use reqwest::{Client, StatusCode, Url, multipart};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct BenchmarkConfig {
    pub base_url: Url,
    pub clients: usize,
    pub deadline: Duration,
    pub poll_interval: Duration,
    pub declared_worker_concurrency: usize,
    pub declared_region_concurrency: usize,
    pub extraction_profile: String,
    token: String,
}
impl BenchmarkConfig {
    pub fn new(
        base_url: &str,
        clients: usize,
        deadline_seconds: u64,
        worker: usize,
        region: usize,
        profile: String,
        token: String,
    ) -> Result<Self> {
        let base_url = Url::parse(base_url).context("base URL is invalid")?;
        ensure!(
            matches!(base_url.scheme(), "http" | "https")
                && base_url.host_str().is_some()
                && base_url.username().is_empty()
                && base_url.password().is_none()
                && base_url.query().is_none()
                && base_url.fragment().is_none()
                && base_url.path() == "/",
            "base URL must be an HTTP(S) origin without credentials, path, query or fragment"
        );
        ensure!((1..=4).contains(&clients), "clients must be 1..4");
        ensure!(
            (5..=3600).contains(&deadline_seconds),
            "per-document deadline must be 5..3600 seconds"
        );
        ensure!(
            (1..=128).contains(&worker) && (1..=128).contains(&region),
            "declared W/R concurrency must be 1..128"
        );
        ensure!(
            matches!(profile.as_str(), "none" | "vi_rental_contract_v1"),
            "unsupported extraction profile"
        );
        ensure!(
            (16..=4096).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic()),
            "OCR_API_TOKEN must contain 16..4096 visible ASCII characters"
        );
        Ok(Self {
            base_url,
            clients,
            deadline: Duration::from_secs(deadline_seconds),
            poll_interval: Duration::from_millis(500),
            declared_worker_concurrency: worker,
            declared_region_concurrency: region,
            extraction_profile: profile,
            token,
        })
    }
}
#[derive(Debug, Serialize)]
pub struct BenchmarkDocument {
    pub document_id: String,
    pub accepted: bool,
    pub job_id: Option<Uuid>,
    pub status: String,
    pub latency_ms: f64,
    pub attempts: Option<i32>,
    pub status_polls: u64,
    pub error_code: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub schema_version: String,
    pub run_id: Uuid,
    pub dataset_name: String,
    pub synthetic: bool,
    pub split: Split,
    pub clients: usize,
    pub declared_worker_concurrency: usize,
    pub declared_region_concurrency: usize,
    pub runtime_configuration_verified: bool,
    pub extraction_profile: String,
    pub elapsed_seconds: f64,
    pub submitted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub documents_per_minute: f64,
    pub success_latency_p50_ms: Option<f64>,
    pub success_latency_p95_ms: Option<f64>,
    pub documents: Vec<BenchmarkDocument>,
    pub limitations: Vec<String>,
}
pub async fn run(
    dataset: &Dataset,
    split: Split,
    config: &BenchmarkConfig,
) -> Result<BenchmarkReport> {
    let documents = dataset.selected(split)?;
    let run_id = Uuid::new_v4();
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(config.clients)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let started = Instant::now();
    let mut results: Vec<_> = stream::iter(documents)
        .map(|document| execute_document(&client, dataset, document, config, run_id))
        .buffer_unordered(config.clients)
        .collect()
        .await;
    let elapsed = started.elapsed().as_secs_f64();
    results.sort_by(|a, b| a.document_id.cmp(&b.document_id));
    let latencies: Vec<_> = results
        .iter()
        .filter(|d| d.status == "succeeded")
        .map(|d| d.latency_ms)
        .collect();
    let succeeded = latencies.len();
    let submitted = results.iter().filter(|d| d.accepted).count();
    Ok(BenchmarkReport {
        schema_version: "ocr-benchmark/1.0.0".into(), run_id,
        dataset_name: dataset.manifest.dataset_name.clone(), synthetic: dataset.manifest.synthetic, split,
        clients: config.clients, declared_worker_concurrency: config.declared_worker_concurrency,
        declared_region_concurrency: config.declared_region_concurrency, runtime_configuration_verified: false,
        extraction_profile: config.extraction_profile.clone(), elapsed_seconds: elapsed, submitted,
        succeeded, failed: results.len() - succeeded,
        documents_per_minute: if elapsed > 0.0 { succeeded as f64 * 60.0 / elapsed } else { 0.0 },
        success_latency_p50_ms: percentile(&latencies, 0.50), success_latency_p95_ms: percentile(&latencies, 0.95),
        documents: results,
        limitations: vec!["W/R are operator-declared run metadata; this client neither verifies nor changes runtime concurrency.".into(), "Latency includes source-file loading, upload, queueing, processing and compact status polling; successful latency excludes failed/timed-out documents.".into(), "Timeout/cancellation stops client waiting; already accepted durable server jobs continue and may overlap a later run.".into(), "Polling resolution is 500ms; this runner does not measure GPU utilization, server RSS or OCR correctness.".into()],
    })
}
async fn execute_document(
    client: &Client,
    dataset: &Dataset,
    document: &Document,
    config: &BenchmarkConfig,
    run_id: Uuid,
) -> BenchmarkDocument {
    let started = Instant::now();
    let mut progress = Progress::default();
    let outcome = tokio::time::timeout(
        config.deadline,
        execute(client, dataset, document, config, run_id, &mut progress),
    )
    .await;
    let (status, attempts, error_code) = match outcome {
        Ok(Ok(status)) => {
            let error = (status.status == "failed").then(|| "server_job_failed".into());
            (status.status, Some(status.attempts), error)
        }
        Ok(Err(error)) => ("failed".into(), None, Some(error)),
        Err(_) => (
            "timeout".into(),
            None,
            Some("document_deadline_exceeded".into()),
        ),
    };
    BenchmarkDocument {
        document_id: document.id.clone(),
        accepted: progress.accepted,
        job_id: progress.job_id,
        status,
        latency_ms: started.elapsed().as_secs_f64() * 1000.0,
        attempts,
        status_polls: progress.polls,
        error_code,
    }
}
#[derive(Deserialize)]
struct Submitted {
    id: Uuid,
}
#[derive(Deserialize)]
struct Status {
    status: String,
    attempts: i32,
}
#[derive(Default)]
struct Progress {
    polls: u64,
    accepted: bool,
    job_id: Option<Uuid>,
}
async fn execute(
    client: &Client,
    dataset: &Dataset,
    document: &Document,
    config: &BenchmarkConfig,
    run_id: Uuid,
    progress: &mut Progress,
) -> std::result::Result<Status, String> {
    // At most four input buffers exist. Validate hashes again at the point of use.
    let path = dataset
        .source_file(document)
        .map_err(|_| "source_read_or_hash_failed")?;
    let expected = document.source_sha256.clone();
    // The permit remains in the blocking closure if the async deadline fires.
    // Repeated timeouts cannot grow blocking file/hash work without a bound.
    static INPUT_SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = INPUT_SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "source_capacity_closed")?;
    let bytes = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let bytes = crate::read_bounded(&path, crate::MAX_SOURCE_BYTES)?;
        anyhow::ensure!(
            crate::digest(&bytes) == expected,
            "source changed after validation"
        );
        Ok::<_, anyhow::Error>(bytes)
    })
    .await
    .map_err(|_| "source_task_failed")?
    .map_err(|_| "source_read_or_hash_failed")?;
    let (mime, filename) = media(&bytes).ok_or("unsupported_source_media")?;
    let part = multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(mime)
        .map_err(|_| "multipart_failed")?;
    let form = multipart::Form::new()
        .part("file", part)
        .text("extraction_profile", config.extraction_profile.clone());
    let key = format!("bench-{run_id}-{}", &document.source_sha256[..16]);
    let response = client
        .post(
            config
                .base_url
                .join("v1/ocr/jobs")
                .expect("fixed relative URL"),
        )
        .bearer_auth(&config.token)
        .header("Idempotency-Key", key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| "submit_transport_failed")?;
    if response.status() != StatusCode::ACCEPTED {
        return Err(format!("submit_http_{}", response.status().as_u16()));
    }
    progress.accepted = true;
    let submitted: Submitted =
        serde_json::from_slice(&response_bounded(response, 64 * 1024).await?)
            .map_err(|_| "submit_response_invalid")?;
    progress.job_id = Some(submitted.id);
    loop {
        progress.polls += 1;
        let url = config
            .base_url
            .join(&format!("v1/ocr/jobs/{}/status", submitted.id))
            .expect("fixed relative URL");
        let response = client
            .get(url)
            .bearer_auth(&config.token)
            .send()
            .await
            .map_err(|_| "poll_transport_failed")?;
        if response.status() != StatusCode::OK {
            return Err(format!("poll_http_{}", response.status().as_u16()));
        }
        let status: Status = serde_json::from_slice(&response_bounded(response, 16 * 1024).await?)
            .map_err(|_| "status_response_invalid")?;
        match status.status.as_str() {
            "succeeded" | "failed" => return Ok(status),
            "queued" | "processing" => tokio::time::sleep(config.poll_interval).await,
            _ => return Err("unknown_job_status".into()),
        }
    }
}
async fn response_bounded(
    response: reqwest::Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("response_byte_budget_exceeded".into());
    }
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "response_read_failed")?;
        if chunk.len() > limit.saturating_sub(output.len()) {
            return Err("response_byte_budget_exceeded".into());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}
fn media(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"%PDF-") {
        Some(("application/pdf", "benchmark.pdf"))
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "benchmark.png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", "benchmark.jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "benchmark.webp"))
    } else {
        None
    }
}
fn percentile(values: &[f64], quantile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    Some(values[((values.len() as f64 * quantile).ceil() as usize).saturating_sub(1)])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_bounds_and_secret_free_error_messages() {
        for clients in [0, 5] {
            assert!(
                BenchmarkConfig::new(
                    "http://127.0.0.1:8080",
                    clients,
                    60,
                    1,
                    1,
                    "none".into(),
                    "a".repeat(32)
                )
                .is_err()
            );
        }
        for url in [
            "http://user:secret@localhost/",
            "http://localhost/?token=secret",
            "file:///tmp/input",
            "http://localhost/api",
        ] {
            let error = BenchmarkConfig::new(url, 1, 60, 1, 1, "none".into(), "a".repeat(32))
                .err()
                .unwrap();
            assert!(!error.to_string().contains("secret"));
        }
        assert_eq!(percentile(&[], 0.95), None);
        assert_eq!(percentile(&[10.0, 40.0, 20.0, 30.0], 0.50), Some(20.0));
        assert_eq!(percentile(&[10.0, 40.0, 20.0, 30.0], 0.95), Some(40.0));
    }
}
