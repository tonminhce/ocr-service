use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use ocr_eval::{
    Dataset, Document, Manifest, Split,
    arrival::{ArrivalConfig, ArrivalPattern, run as run_arrival},
    benchmark::{BenchmarkConfig, SubmissionOutcome, run},
    digest,
};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
struct Counts {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    submitted: Arc<AtomicUsize>,
}
async fn submit(
    State(counts): State<Counts>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", "a".repeat(32))
    );
    assert!(
        headers["idempotency-key"]
            .to_str()
            .unwrap()
            .starts_with("bench-")
    );
    let current = counts.current.fetch_add(1, Ordering::SeqCst) + 1;
    counts.peak.fetch_max(current, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(25)).await;
    counts.current.fetch_sub(1, Ordering::SeqCst);
    let id = counts.submitted.fetch_add(1, Ordering::SeqCst) + 1;
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"id": Uuid::from_u128(id as u128)})),
    )
}
async fn status(Path(id): Path<Uuid>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"status": if id.as_u128().is_multiple_of(2) {"succeeded"} else {"failed"}, "attempts":1}),
    )
}
#[tokio::test]
async fn benchmark_bounds_clients_polls_compact_status_and_keeps_failures_visible() {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = Manifest {
        schema_version: "ocr-eval-manifest/1.0.0".into(),
        dataset_name: "synthetic-api-runner".into(),
        synthetic: true,
        documents: Vec::new(),
    };
    for index in 0..4 {
        let bytes = format!("%PDF-1.4\nSynthetic fake API fixture {index}\n");
        let path = format!("source-{index}.pdf");
        std::fs::write(directory.path().join(&path), &bytes).unwrap();
        manifest.documents.push(Document {
            id: format!("d{index}"),
            split: Split::Test,
            group_id: format!("g{index}"),
            source_sha256: digest(bytes.as_bytes()),
            source_path: path,
            result_path: None,
            result_sha256: None,
            labels: BTreeMap::new(),
            roles: BTreeMap::new(),
        });
    }
    let path = directory.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let dataset = Dataset::load(&path).unwrap();
    let counts = Counts {
        current: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        submitted: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/v1/ocr/jobs", post(submit))
        .route("/v1/ocr/jobs/{id}/status", get(status))
        .with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = BenchmarkConfig::new(&base, 2, 5, 1, 2, "none".into(), "a".repeat(32)).unwrap();
    let report = run(&dataset, Split::Test, &config).await.unwrap();
    server.abort();
    assert_eq!(counts.peak.load(Ordering::SeqCst), 2);
    assert_eq!(report.submitted, 4);
    assert_eq!((report.succeeded, report.failed), (2, 2));
    assert!(
        report
            .documents
            .iter()
            .all(|d| d.accepted && d.status_polls == 1)
    );
    assert!(
        report
            .documents
            .iter()
            .filter(|d| d.status == "failed")
            .all(|d| d.error_code.as_deref() == Some("server_job_failed"))
    );
    assert!(!report.runtime_configuration_verified);
    assert!(report.success_latency_p50_ms.is_some() && report.success_latency_p95_ms.is_some());
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains(&"a".repeat(32)) && !json.contains("source-0.pdf"));
    assert!(!json.contains("submit_latency_ms"));
}

#[derive(Clone)]
struct ArrivalState {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    submitted: Arc<AtomicUsize>,
    keys: Arc<tokio::sync::Mutex<Vec<String>>>,
    rejections: Arc<tokio::sync::Mutex<VecDeque<StatusCode>>>,
    delay: Duration,
    status_delay: Duration,
}

async fn arrival_submit(
    State(state): State<ArrivalState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", "b".repeat(32))
    );
    let key = headers["idempotency-key"].to_str().unwrap().to_owned();
    state.keys.lock().await.push(key);
    let rejection = state.rejections.lock().await.pop_front();
    if let Some(status) = rejection {
        return (
            status,
            Json(serde_json::json!({"error": "synthetic rejection"})),
        );
    }

    let current = state.current.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(current, Ordering::SeqCst);
    tokio::time::sleep(state.delay).await;
    state.current.fetch_sub(1, Ordering::SeqCst);
    let id = state.submitted.fetch_add(1, Ordering::SeqCst) + 1;
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"id": Uuid::from_u128((100 + id) as u128)})),
    )
}

async fn arrival_status(
    State(state): State<ArrivalState>,
    Path(_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    tokio::time::sleep(state.status_delay).await;
    Json(serde_json::json!({"status": "succeeded", "attempts": 1}))
}

async fn start_arrival_server(
    delay: Duration,
    status_delay: Duration,
    rejections: Vec<StatusCode>,
) -> (String, ArrivalState, tokio::task::JoinHandle<()>) {
    let state = ArrivalState {
        current: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        submitted: Arc::new(AtomicUsize::new(0)),
        keys: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        rejections: Arc::new(tokio::sync::Mutex::new(rejections.into_iter().collect())),
        delay,
        status_delay,
    };
    let app = Router::new()
        .route("/v1/ocr/jobs", post(arrival_submit))
        .route("/v1/ocr/jobs/{id}/status", get(arrival_status))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state, server)
}

fn arrival_dataset() -> (tempfile::TempDir, Dataset) {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = Manifest {
        schema_version: "ocr-eval-manifest/1.0.0".into(),
        dataset_name: "synthetic-arrival-runner".into(),
        synthetic: true,
        documents: Vec::new(),
    };
    for index in 0..2 {
        let bytes = format!("%PDF-1.4\nSynthetic arrival fixture {index:04}\n").into_bytes();
        let path = format!("source-{index}.pdf");
        std::fs::write(directory.path().join(&path), &bytes).unwrap();
        manifest.documents.push(Document {
            id: format!("d{index}"),
            split: Split::Test,
            group_id: format!("g{index}"),
            source_sha256: digest(&bytes),
            source_path: path,
            result_path: None,
            result_sha256: None,
            labels: BTreeMap::new(),
            roles: BTreeMap::new(),
        });
    }
    let path = directory.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    (directory, Dataset::load(&path).unwrap())
}

fn arrival_benchmark_config(base: &str, clients: usize) -> BenchmarkConfig {
    BenchmarkConfig::new(base, clients, 5, 1, 1, "none".into(), "b".repeat(32)).unwrap()
}

#[tokio::test]
async fn fixed_arrivals_continue_under_saturation_and_occurrences_get_unique_keys() {
    let (_directory, dataset) = arrival_dataset();
    let (base, state, server) =
        start_arrival_server(Duration::from_millis(220), Duration::ZERO, Vec::new()).await;
    let benchmark = arrival_benchmark_config(&base, 2);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 20.0, 1, 10, 1_000_000, 1, 19).unwrap();

    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    server.abort();

    assert_eq!(report.scheduled, 10);
    assert!(report.offered < report.scheduled);
    assert!(report.client_skipped_or_late > 0);
    assert_eq!(report.maximum_in_flight, 1);
    assert_eq!(state.peak.load(Ordering::SeqCst), 1);
    assert_eq!(report.accepted, report.offered);
    assert_eq!(report.completed, report.offered);
    assert_eq!(report.succeeded, report.offered);
    assert_eq!(report.outstanding, 0);
    assert!(report.offered_per_second < arrival.target_per_second);

    let keys = state.keys.lock().await;
    let unique: HashSet<_> = keys.iter().collect();
    assert_eq!(keys.len(), report.offered);
    assert_eq!(unique.len(), keys.len());
    assert!(
        keys.iter()
            .all(|key| key.starts_with(&format!("bench-{}-", report.run_id)))
    );
}

#[tokio::test]
async fn arrival_runner_enforces_total_upload_budget_and_reports_http_rejections() {
    let (_directory, dataset) = arrival_dataset();
    let one_source = dataset.source_size(&dataset.manifest.documents[0]).unwrap();
    let (base, _state, server) =
        start_arrival_server(Duration::ZERO, Duration::ZERO, Vec::new()).await;
    let benchmark = arrival_benchmark_config(&base, 4);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 10.0, 1, 3, one_source, 4, 2).unwrap();
    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    server.abort();
    assert_eq!(report.offered, 1);
    assert_eq!(report.upload_budget_skipped, 2);
    assert_eq!(report.source_bytes_budgeted, one_source);

    let (base, _state, server) = start_arrival_server(
        Duration::ZERO,
        Duration::ZERO,
        vec![
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ],
    )
    .await;
    let benchmark = arrival_benchmark_config(&base, 2);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 2, 2, 1_000_000, 2, 3).unwrap();
    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    server.abort();
    assert_eq!(report.offered, 2);
    assert_eq!(report.accepted, 0);
    assert_eq!(report.rejected, 1);
    assert_eq!(report.acceptance_unknown, 1);
    assert_eq!(report.not_submitted, 0);
    assert_eq!(report.accepted_outstanding, 0);
    assert_eq!(report.outstanding, 1);
    assert_eq!(report.failed, 2);
    assert!(report.documents.iter().all(|item| {
        item.result.as_ref().is_some_and(|result| {
            result
                .error_code
                .as_deref()
                .is_some_and(|code| code == "submit_http_429" || code == "submit_http_503")
        })
    }));
}

#[tokio::test]
async fn accepted_job_past_client_deadline_is_reported_outstanding() {
    let (_directory, dataset) = arrival_dataset();
    let (base, _state, server) =
        start_arrival_server(Duration::ZERO, Duration::from_secs(10), Vec::new()).await;
    let benchmark = arrival_benchmark_config(&base, 1);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 5, 1, 1_000_000, 1, 1).unwrap();

    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    server.abort();

    assert_eq!(report.accepted, 1);
    assert_eq!(report.acceptance_unknown, 0);
    assert_eq!(report.timed_out, 1);
    assert_eq!(report.completed, 0);
    assert_eq!(report.outstanding, 1);
    assert_eq!(report.accepted_outstanding, 1);
    assert_eq!(report.accepted_to_terminal_samples, 0);
    assert_eq!(report.documents.len(), 1);
    assert!(
        report.documents[0]
            .result
            .as_ref()
            .is_some_and(|result| result.job_id.is_some())
    );
}

async fn start_submission_response_server(
    response: Option<(StatusCode, &'static str)>,
    delay: Duration,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let consumed = Arc::new(AtomicUsize::new(0));
    let keys = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let server_consumed = consumed.clone();
    let server_keys = keys.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let bytes = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert!(bytes > 0 && request.len() + bytes <= 64 * 1024);
            request.extend_from_slice(&buffer[..bytes]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8(request[..header_end].to_vec()).unwrap();
        let header = |name: &str| {
            headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .unwrap()
                .1
                .trim()
                .to_owned()
        };
        let request_end = header_end + header("content-length").parse::<usize>().unwrap();
        let key = header("idempotency-key");
        assert!(request_end <= 64 * 1024);
        while request.len() < request_end {
            let bytes = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert!(bytes > 0 && request.len() + bytes <= 64 * 1024);
            request.extend_from_slice(&buffer[..bytes]);
        }
        server_keys.lock().await.push(key);
        server_consumed.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(delay).await;
        if let Some((status, body)) = response {
            let response = format!(
                "HTTP/1.1 {} Synthetic\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                status.as_u16(),
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (base, consumed, keys, server)
}

#[tokio::test]
async fn arrival_separates_rejected_unknown_and_accepted_unresolved_submissions() {
    for (response, expected_outcome, expected_accepted, expected_error) in [
        (
            Some((StatusCode::TOO_MANY_REQUESTS, "{}")),
            SubmissionOutcome::Rejected,
            false,
            "submit_http_429",
        ),
        (
            Some((
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"error":{"code":"enqueue_timeout"}}"#,
            )),
            SubmissionOutcome::AcceptanceUnknown,
            false,
            "submit_http_503",
        ),
        (
            Some((StatusCode::REQUEST_TIMEOUT, "{}")),
            SubmissionOutcome::AcceptanceUnknown,
            false,
            "submit_http_408",
        ),
        (
            Some((StatusCode::ACCEPTED, "not-json")),
            SubmissionOutcome::Accepted,
            true,
            "submit_response_invalid",
        ),
        (
            None,
            SubmissionOutcome::AcceptanceUnknown,
            false,
            "submit_transport_failed",
        ),
    ] {
        let (_directory, dataset) = arrival_dataset();
        let (base, consumed, keys, server) =
            start_submission_response_server(response, Duration::ZERO).await;
        let benchmark = arrival_benchmark_config(&base, 1);
        let arrival =
            ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 1, 1, 1_000_000, 1, 1).unwrap();
        let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
            .await
            .unwrap();
        server.await.unwrap();
        let result = report.documents[0].result.as_ref().unwrap();
        assert_eq!(result.submission_outcome, expected_outcome);
        assert_eq!(result.accepted, expected_accepted);
        assert_eq!(result.error_code.as_deref(), Some(expected_error));
        assert_eq!(result.job_id, None);
        let submission_started = result.submission_started_offset_ms.unwrap();
        assert!(submission_started >= report.documents[0].scheduled_offset_ms);
        assert!(submission_started <= report.elapsed_seconds * 1000.0);
        assert_eq!(report.offered, 1);
        assert_eq!(report.submission_attempted, 1);
        assert_eq!(
            report.submission_starts_in_window + report.submission_starts_after_window,
            report.submission_attempted
        );
        assert_eq!(report.submission_scheduling_lag_samples, 1);
        assert_eq!(
            report.submission_scheduling_lag_p50_ms,
            Some(submission_started)
        );
        assert_eq!(
            report.submission_starts_in_window_per_second,
            report.submission_starts_in_window as f64 / report.scheduled_window_seconds
        );
        assert_eq!(
            report.submission_attempts_per_second,
            1.0 / report.scheduled_window_seconds
        );
        assert_eq!(report.not_submitted, 0);
        assert_eq!(report.completed, 0);
        assert_eq!(
            report.rejected,
            usize::from(expected_outcome == SubmissionOutcome::Rejected)
        );
        assert_eq!(
            report.acceptance_unknown,
            usize::from(expected_outcome == SubmissionOutcome::AcceptanceUnknown)
        );
        assert_eq!(report.accepted, usize::from(expected_accepted));
        assert_eq!(report.accepted_outstanding, usize::from(expected_accepted));
        assert_eq!(report.outstanding, 1 - report.rejected);
        assert_eq!(
            report.offered,
            report.accepted + report.rejected + report.acceptance_unknown + report.not_submitted
        );
        assert_eq!(
            report.submission_attempted,
            report.accepted + report.rejected + report.acceptance_unknown
        );
        assert_eq!(consumed.load(Ordering::SeqCst), 1);
        let keys = keys.lock().await;
        assert_eq!(keys.len(), 1);
        assert_eq!(result.idempotency_key.as_deref(), Some(keys[0].as_str()));
        let document = &dataset.manifest.documents[0];
        assert_eq!(
            result.idempotency_key.as_deref(),
            Some(
                format!(
                    "bench-{}-0-{}",
                    report.run_id,
                    &document.source_sha256[..16]
                )
                .as_str()
            )
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains(&"b".repeat(32)));
        assert!(json.contains("idempotency_key"));
    }
}

#[tokio::test]
async fn submission_deadline_after_consumption_remains_unknown_and_unreconciled() {
    let (_directory, dataset) = arrival_dataset();
    let (base, consumed, keys, server) = start_submission_response_server(
        Some((
            StatusCode::ACCEPTED,
            r#"{"id":"00000000-0000-0000-0000-000000000001"}"#,
        )),
        Duration::from_secs(5),
    )
    .await;
    let mut benchmark = arrival_benchmark_config(&base, 1);
    benchmark.deadline = Duration::from_secs(1);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 1, 1, 1_000_000, 1, 1).unwrap();
    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    server.abort();
    assert_eq!(consumed.load(Ordering::SeqCst), 1);
    assert_eq!(
        (report.accepted, report.rejected, report.not_submitted),
        (0, 0, 0)
    );
    assert_eq!(report.acceptance_unknown, 1);
    assert_eq!(report.outstanding, 1);
    assert_eq!(report.accepted_outstanding, 0);
    assert_eq!(report.timed_out, 1);
    let result = report.documents[0].result.as_ref().unwrap();
    assert_eq!(
        result.submission_outcome,
        SubmissionOutcome::AcceptanceUnknown
    );
    let keys = keys.lock().await;
    assert_eq!(keys.len(), 1);
    assert_eq!(result.idempotency_key.as_deref(), Some(keys[0].as_str()));
}

#[tokio::test]
async fn changed_source_fails_locally_without_creating_unknown_server_work() {
    let (directory, dataset) = arrival_dataset();
    let source = &dataset.manifest.documents[0].source_path;
    std::fs::write(
        directory.path().join(source),
        b"%PDF-1.4 changed synthetic source",
    )
    .unwrap();
    let benchmark = arrival_benchmark_config("http://127.0.0.1:9", 1);
    let arrival =
        ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 1, 1, 1_000_000, 1, 1).unwrap();
    let report = run_arrival(&dataset, Split::Test, &benchmark, &arrival)
        .await
        .unwrap();
    assert_eq!(report.offered, 1);
    assert_eq!(report.submission_attempted, 0);
    assert_eq!(report.submission_attempts_per_second, 0.0);
    assert_eq!(
        report.offered_per_second,
        1.0 / report.scheduled_window_seconds
    );
    assert_eq!(report.not_submitted, 1);
    assert_eq!(report.acceptance_unknown, 0);
    assert_eq!(report.rejected, 0);
    assert_eq!(report.outstanding, 0);
    let result = report.documents[0].result.as_ref().unwrap();
    assert_eq!(result.submission_outcome, SubmissionOutcome::NotSubmitted);
    assert!(result.idempotency_key.is_none());
    assert!(result.submission_started_offset_ms.is_none());
    assert_eq!(report.submission_starts_in_window, 0);
    assert_eq!(report.submission_starts_after_window, 0);
    assert_eq!(report.submission_starts_in_window_per_second, 0.0);
    assert_eq!(report.submission_scheduling_lag_samples, 0);
    assert!(report.submission_scheduling_lag_p50_ms.is_none());
    assert!(report.submission_scheduling_lag_p95_ms.is_none());
    assert_eq!(
        result.error_code.as_deref(),
        Some("source_read_or_hash_failed")
    );
}

#[tokio::test]
async fn closed_loop_report_keeps_unknown_submission_outstanding_and_success_terminal() {
    let (_directory, mut dataset) = arrival_dataset();
    dataset.manifest.documents.truncate(1);
    let (base, consumed, keys, server) =
        start_submission_response_server(None, Duration::ZERO).await;
    let config = arrival_benchmark_config(&base, 1);
    let report = run(&dataset, Split::Test, &config).await.unwrap();
    server.await.unwrap();
    assert_eq!(consumed.load(Ordering::SeqCst), 1);
    assert_eq!(report.submitted, 0);
    assert_eq!(report.submission_attempted, 1);
    assert!(report.documents[0].submission_started_offset_ms.is_some());
    assert_eq!(report.rejected, 0);
    assert_eq!(report.not_submitted, 0);
    assert_eq!(report.acceptance_unknown, 1);
    assert_eq!(report.completed, 0);
    assert_eq!(report.accepted_outstanding, 0);
    assert_eq!(report.outstanding, 1);
    assert_eq!(
        report.submission_attempted,
        report.submitted + report.rejected + report.acceptance_unknown
    );
    assert_eq!(
        report.documents[0].idempotency_key.as_deref(),
        Some(keys.lock().await[0].as_str())
    );

    let (base, state, server) =
        start_arrival_server(Duration::ZERO, Duration::ZERO, Vec::new()).await;
    let config = arrival_benchmark_config(&base, 1);
    let report = run(&dataset, Split::Test, &config).await.unwrap();
    server.abort();
    assert_eq!(state.submitted.load(Ordering::SeqCst), 1);
    assert_eq!(report.submitted, 1);
    assert_eq!(report.submission_attempted, 1);
    assert_eq!(report.completed, 1);
    assert_eq!(report.succeeded, 1);
    assert_eq!(
        report.rejected + report.not_submitted + report.acceptance_unknown,
        0
    );
    assert_eq!(report.outstanding, 0);
    assert_eq!(
        report.documents[0].submission_outcome,
        SubmissionOutcome::Accepted
    );
}

#[test]
fn poisson_extreme_seed_cli_finishes_without_unrepresentable_duration() {
    let (directory, dataset) = arrival_dataset();
    let manifest = directory.path().join("manifest.json");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-eval"))
        .args([
            "arrival",
            manifest.to_str().unwrap(),
            "--split",
            "test",
            "--base-url",
            "http://127.0.0.1:9",
            "--clients",
            "1",
            "--worker-concurrency",
            "1",
            "--region-concurrency",
            "1",
            "--arrival-mode",
            "poisson",
            "--rate-per-second",
            "1",
            "--duration-seconds",
            "1",
            "--max-jobs",
            "1",
            "--max-upload-bytes",
            "1024",
            "--max-pending-tasks",
            "1",
            "--seed",
            "7650297886450228676",
        ])
        .env("OCR_API_TOKEN", "b".repeat(32))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["scheduled"], 0);
    assert_eq!(report["offered"], 0);
    assert_eq!(report["outstanding"], 0);
    assert_eq!(report["dataset_name"], dataset.manifest.dataset_name);
}
