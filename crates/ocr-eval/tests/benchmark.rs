use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use ocr_eval::{
    Dataset, Document, Manifest, Split,
    benchmark::{BenchmarkConfig, run},
    digest,
};
use std::{
    collections::BTreeMap,
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
}
