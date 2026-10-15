//! Real PostgreSQL checkpoint tests plus synthetic recognition/recovery tests.
//! The HTTP model and detector are fixtures, never real OCR accuracy evidence.
#![cfg(unix)]

use std::{
    io::Cursor,
    os::unix::fs::PermissionsExt,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, http::StatusCode, routing::post};
use image::{ImageFormat, RgbImage};
use ocr_core::{
    domain::{LeasedJob, NewJob},
    store::Store,
};
use ocr_layout::{PageLayout, Region, Task};
use ocr_worker::{
    inference::Recognizer,
    settings::{OcrStrategy, Settings},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
mod support;

struct Database {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
}
impl Database {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("retry_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        let store = Store::from_pool(pool.clone());
        store.migrate().await.unwrap();
        Self {
            admin,
            pool,
            store,
            schema,
        }
    }
    async fn leased(&self) -> LeasedJob {
        let input = png();
        let id = Uuid::new_v4();
        self.store
            .submit(NewJob {
                id,
                filename: "fixture.png".into(),
                media_type: "image/png".into(),
                sha256: format!("{:x}", Sha256::digest(&input)),
                input,
                idempotency_key: None,
                extraction_profile: "none".into(),
            })
            .await
            .unwrap();
        self.store.claim_by_id(id, 60, 3).await.unwrap().unwrap()
    }
    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

struct Model {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    peak: Arc<AtomicUsize>,
    release: Arc<Notify>,
    handle: JoinHandle<()>,
    parser: support::parser::ParserHarness,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.release.notify_waiters();
        self.handle.abort();
    }
}
impl Model {
    async fn new(
        responses: Vec<(StatusCode, String)>,
        delay_ms: u64,
        block_call: Option<usize>,
    ) -> Self {
        let parser = support::parser::ParserHarness::start().await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let observed_peak = peak.clone();
        let release = Arc::new(Notify::new());
        let gate = release.clone();
        let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>|{
            let requests=captured.clone();let active=active.clone();let peak=observed_peak.clone();
            let responses=responses.clone();let release=gate.clone();
            async move {
                let call={let mut requests=requests.lock().unwrap();requests.push(body);requests.len()};
                struct Active(Arc<AtomicUsize>);impl Drop for Active{fn drop(&mut self){self.0.fetch_sub(1,Ordering::SeqCst);}}
                let count=active.fetch_add(1,Ordering::SeqCst)+1;peak.fetch_max(count,Ordering::SeqCst);
                let _active=Active(active);
                if block_call==Some(call){release.notified().await;}
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                let (status,text)=responses.get(call-1).cloned().unwrap_or((StatusCode::OK,"ordinary transcription".into()));
                (status,Json(json!({"choices":[{"message":{"content":text},"finish_reason":"stop"}]})))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            requests,
            peak,
            release,
            handle,
            parser,
        }
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    async fn wait_calls(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.calls() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture did not receive expected model calls");
    }
}

struct Detector {
    directory: tempfile::TempDir,
    binary: std::path::PathBuf,
}
impl Detector {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("fake-layout");
        let fixture = directory.path().join("layout.json");
        std::fs::write(&fixture, serde_json::to_vec(&layout()).unwrap()).unwrap();
        let path = format!("'{}'", fixture.to_str().unwrap().replace('\'', "'\"'\"'"));
        let script = format!(
            r##"#!/bin/sh
printf '%s\n' '{{"type":"ready","version":1,"model_sha256":"{digest}"}}'
while IFS= read -r request; do
 id=$(printf '%s' "$request" | /usr/bin/sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
 printf '{{"type":"layout","id":%s,"layout":' "$id"
 /bin/cat {path}
 printf '}}\n'
done
"##,
            digest = "0".repeat(64)
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { directory, binary }
    }
}
fn layout() -> PageLayout {
    let regions = (0..3)
        .map(|index| {
            let top = index as i32 * 330;
            let bottom = top + 300;
            Region {
                index,
                class_id: 22,
                label: "text".into(),
                task: Task::Text,
                bbox: [0., top as f32 * 0.096, 64., bottom as f32 * 0.096],
                bbox_2d: [0, top, 1000, bottom],
                polygon: vec![[0, top], [1000, top], [1000, bottom], [0, bottom]],
                polygon_source: "fixture".into(),
                score: 0.9,
                reading_order: index,
            }
        })
        .collect();
    PageLayout {
        width: 64,
        height: 96,
        regions,
        discarded: vec![],
    }
}
fn png() -> Vec<u8> {
    let page = RgbImage::from_fn(64, 96, |x, y| image::Rgb([x as u8, y as u8, (x ^ y) as u8]));
    let mut bytes = Cursor::new(Vec::new());
    page.write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}
fn settings(model: &Model, detector: &Detector) -> Settings {
    Settings {
        database_url: "postgres://unused".into(),
        redis_url: "redis://localhost:6379".into(),
        redis_stream: "ocr:test".into(),
        redis_consumer_group: "test".into(),
        redis_timeout_ms: 2000,
        redis_redispatch_seconds: 30,
        vllm_base_url: model.base.clone(),
        vllm_model: "fixture-model".into(),
        vllm_api_key: None,
        worker_concurrency: 4,
        lease_seconds: 60,
        max_attempts: 3,
        poll_interval_ms: 500,
        request_timeout_seconds: 5,
        job_timeout_seconds: 60,
        max_pages: 20,
        max_image_pixels: 1_000_000,
        max_image_edge: 2048,
        table_crop_padding_pixels: 0,
        pdf_render_dpi: 200,
        page_max_edge: 3500,
        strategy: OcrStrategy::CourseLayout,
        layout_binary: detector.binary.clone(),
        layout_model_path: detector.directory.path().join("unused.onnx"),
        layout_model_sha256: "0".repeat(64),
        layout_timeout_seconds: 5,
        layout_pool_size: 1,
        max_regions_per_page: 128,
        max_regions_per_document: 2048,
        region_concurrency: 1,
        inference_revision: "pinned-fixture-deployment-v1".into(),
        inference_deployment: None,
        checkpoint_enabled: false,
        recovery_max_regions: 0,
        recovery_expansion_pixels: 16,
        recognize_image_regions: false,
        pdf_render_timeout_seconds: 30,
        parser_socket_path: model.parser.socket_path(),
        max_output_tokens: 1024,
        retention_days: 7,
    }
}
fn ok(text: &str) -> (StatusCode, String) {
    (StatusCode::OK, text.into())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn recreated_worker_recognizer_reuses_completed_regions_after_retry() {
    let db = Database::new().await;
    let detector = Detector::new();
    let model = Model::new(
        vec![
            ok("first region"),
            (StatusCode::SERVICE_UNAVAILABLE, "temporary failure".into()),
            ok("second region"),
            ok("third region"),
        ],
        0,
        None,
    )
    .await;
    let old = db.leased().await;
    let mut config = settings(&model, &detector);
    config.checkpoint_enabled = true;
    let first = Recognizer::new(config.clone()).unwrap().for_job(
        db.store.clone(),
        old.job.id,
        old.lease_token,
    );
    let error = first
        .recognize(old.input.clone(), "image/png", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.retryable);
    assert_eq!(model.calls(), 2);
    first.shutdown().await;
    drop(first);
    assert!(
        db.store
            .fail(old.job.id, old.lease_token, "fixture retry", true, 3, 0)
            .await
            .unwrap()
    );
    let next = db
        .store
        .claim_by_id(old.job.id, 60, 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.job.attempts, 2);
    let restarted =
        Recognizer::new(config)
            .unwrap()
            .for_job(db.store.clone(), next.job.id, next.lease_token);
    let result = restarted
        .recognize(next.input, "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        model.calls(),
        4,
        "retry must recognize only the two remaining regions"
    );
    assert_eq!(
        result["pages"][0]["raw_regions"][0]["raw_content"],
        "first region"
    );
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(
            requests[1]["messages"][1]["content"][1],
            requests[2]["messages"][1]["content"][1]
        );
        assert_ne!(
            requests[0]["messages"][1]["content"][1],
            requests[2]["messages"][1]["content"][1]
        );
    }
    let cached: i64 = sqlx::query_scalar("SELECT count(*) FROM region_checkpoints WHERE job_id=$1")
        .bind(next.job.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(cached, 3);
    restarted.shutdown().await;
    drop(restarted);
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and CREATE SCHEMA privileges"]
async fn changed_model_revision_pipeline_options_and_job_scope_do_not_reuse_stale_content() {
    let db = Database::new().await;
    let detector = Detector::new();
    let model = Model::new(vec![], 0, None).await;
    let first = db.leased().await;
    let mut config = settings(&model, &detector);
    config.checkpoint_enabled = true;
    let recognizer = Recognizer::new(config.clone()).unwrap().for_job(
        db.store.clone(),
        first.job.id,
        first.lease_token,
    );
    assert_eq!(
        recognizer.transcribe_jpeg(b"fixture-jpeg").await.unwrap(),
        "ordinary transcription"
    );
    recognizer.transcribe_jpeg(b"fixture-jpeg").await.unwrap();
    assert_eq!(model.calls(), 1);
    let telemetry = recognizer.telemetry_snapshot();
    assert_eq!((telemetry.attempts_started, telemetry.succeeded), (1, 1));
    assert_eq!(
        (telemetry.checkpoint_hits, telemetry.checkpoint_misses),
        (1, 1)
    );
    assert_eq!(
        telemetry.usage.missing_responses, 1,
        "cache replay must not replay usage"
    );
    for variant in 0..4 {
        let mut changed = config.clone();
        match variant {
            0 => changed.inference_revision = "new-pinned-deployment".into(),
            1 => changed.vllm_model = "different-model".into(),
            2 => changed.max_output_tokens += 1,
            _ => changed.layout_model_sha256 = "1".repeat(64),
        }
        let changed = Recognizer::new(changed).unwrap().for_job(
            db.store.clone(),
            first.job.id,
            first.lease_token,
        );
        changed.transcribe_jpeg(b"fixture-jpeg").await.unwrap();
        changed.shutdown().await;
    }
    assert_eq!(
        model.calls(),
        5,
        "changed inference/pipeline fingerprints must miss"
    );
    let second = db.leased().await;
    let separate = Recognizer::new(config).unwrap().for_job(
        db.store.clone(),
        second.job.id,
        second.lease_token,
    );
    separate.transcribe_jpeg(b"fixture-jpeg").await.unwrap();
    assert_eq!(model.calls(), 6);
    recognizer.shutdown().await;
    separate.shutdown().await;
    drop(recognizer);
    drop(separate);
    db.cleanup().await;
}

#[tokio::test]
async fn full_page_calls_share_the_same_global_model_concurrency_bound() {
    let detector = Detector::new();
    let model = Model::new(vec![], 50, None).await;
    let mut config = settings(&model, &detector);
    config.strategy = OcrStrategy::FullPage;
    config.region_concurrency = 2;
    let recognizer = Recognizer::new(config).unwrap();
    let unused_store = Store::from_pool(
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap(),
    );
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let recognizer = recognizer.for_job(unused_store.clone(), Uuid::new_v4(), Uuid::new_v4());
        tasks.push(tokio::spawn(async move {
            recognizer
                .recognize(png(), "image/png", CancellationToken::new())
                .await
                .unwrap();
            let trace = recognizer.telemetry_snapshot();
            assert_eq!((trace.attempts_started, trace.succeeded), (1, 1));
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(model.calls(), 6);
    assert_eq!(model.peak.load(Ordering::SeqCst), 2);
    assert_eq!(
        recognizer.telemetry_snapshot().attempts_started,
        0,
        "each job owns its trace while sharing R"
    );
    recognizer.shutdown().await;
}

const OWNER: &str =
    "HỢP ĐỒNG THUÊ NHÀ\nBÊN CHO THUÊ (BÊN A)\nHọ và tên: Lê Mẫu Một\nCCCD số: 012345678901";
const TENANT: &str = "BÊN THUÊ (BÊN B)\nHọ và tên: Đỗ Mẫu Hai\nCCCD số: 023456789012\nNgày sinh: 07/08/1990\nThú cung";
const RECOVERED: &str = "BÊN THUÊ (BÊN B)\nHọ và tên: Đỗ Mẫu Hai\nCCCD số: 023456789012\nNgày sinh: 09/10/1991\nThú cưng";

#[tokio::test]
async fn bounded_recovery_keeps_raw_content_and_conflicting_alternatives_unresolved() {
    let detector = Detector::new();
    let model = Model::new(
        vec![
            ok(OWNER),
            ok(TENANT),
            ok("Giá thuê: 10000000 VND/tháng\nThú cung"),
            ok(RECOVERED),
        ],
        0,
        None,
    )
    .await;
    let mut config = settings(&model, &detector);
    config.recovery_max_regions = 1;
    let recognizer = Recognizer::new(config).unwrap();
    let result = recognizer
        .recognize_with_profile(
            png(),
            "image/png",
            "vi_rental_contract_v1",
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        model.calls(),
        4,
        "two suspect regions must still respect the one-recovery cap"
    );
    assert_eq!(result["recovery"]["attempted_regions"], 1);
    let observations = result["recovery"]["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["content"], RECOVERED);
    assert_eq!(observations[0]["detector_region_index"], 1);
    assert_eq!(observations[0]["automatically_selected"], false);
    assert_eq!(observations[0]["original_unchanged"], true);
    assert_ne!(
        observations[0]["bbox_2d"],
        result["pages"][0]["raw_regions"][1]["bbox_2d"]
    );
    assert_eq!(result["pages"][0]["raw_regions"][1]["raw_content"], TENANT);
    let tenant = result["extraction"]["parties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["role"] == "tenant")
        .expect("tenant role must be explicit");
    let birth = &tenant["fields"]["birth_date"];
    // Both observations describe the same contract: this is ambiguity within
    // that source, rather than a contract-versus-identity-card conflict.
    assert_eq!(birth["state"], "ambiguous");
    assert!(birth["selected"].is_null());
    assert_eq!(birth["candidates"].as_array().unwrap().len(), 2);
    let evidence = serde_json::to_string(birth).unwrap();
    assert!(evidence.contains("ocr_recovery_context"));
    assert_eq!(result["extraction"]["decision"], "needs_review");
    recognizer.shutdown().await;
}

#[tokio::test]
async fn failed_recovery_is_a_visible_observation_without_rewriting_the_first_pass() {
    let detector = Detector::new();
    let model = Model::new(
        vec![
            ok("Thú cung"),
            ok("ordinary"),
            ok("ordinary"),
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable".into()),
        ],
        0,
        None,
    )
    .await;
    let mut config = settings(&model, &detector);
    config.recovery_max_regions = 1;
    let recognizer = Recognizer::new(config).unwrap();
    let result = recognizer
        .recognize(png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(model.calls(), 4);
    assert_eq!(result["recovery"]["observations"][0]["status"], "failed");
    assert_eq!(
        result["pages"][0]["raw_regions"][0]["raw_content"],
        "Thú cung"
    );
    recognizer.shutdown().await;
}

#[tokio::test]
async fn cancelling_recovery_stops_the_job_and_does_not_schedule_more_model_calls() {
    let detector = Detector::new();
    let model = Model::new(
        vec![ok("Thú cung"), ok("Thú cung"), ok("Thú cung")],
        0,
        Some(4),
    )
    .await;
    let mut config = settings(&model, &detector);
    config.recovery_max_regions = 3;
    let recognizer = Recognizer::new(config).unwrap();
    let token = CancellationToken::new();
    let task = {
        let recognizer = recognizer.clone();
        let token = token.clone();
        tokio::spawn(async move { recognizer.recognize(png(), "image/png", token).await })
    };
    model.wait_calls(4).await;
    token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.retryable && error.message.contains("cancelled"));
    assert_eq!(model.calls(), 4);
    model.release.notify_waiters();
    recognizer.shutdown().await;
}

#[tokio::test]
async fn recovery_of_missing_or_adjacent_birth_date_stays_unselected_and_raw_is_unchanged() {
    for missing in [
        TENANT.replace("Ngày sinh: 07/08/1990\n", ""),
        TENANT.replace("07/08/1990", ""),
    ] {
        let detector = Detector::new();
        let model = Model::new(
            vec![
                ok(OWNER),
                ok(&missing),
                ok("Giá thuê: 10000000 VND/tháng"),
                ok(RECOVERED),
            ],
            0,
            None,
        )
        .await;
        let mut config = settings(&model, &detector);
        config.recovery_max_regions = 1;
        let recognizer = Recognizer::new(config).unwrap();
        let result = recognizer
            .recognize_with_profile(
                png(),
                "image/png",
                "vi_rental_contract_v1",
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let tenant = result["extraction"]["parties"]
            .as_array()
            .unwrap()
            .iter()
            .find(|party| party["role"] == "tenant")
            .unwrap();
        let birth = &tenant["fields"]["birth_date"];
        assert_eq!(birth["state"], "ambiguous");
        assert!(birth["selected"].is_null());
        assert_eq!(birth["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(result["pages"][0]["raw_regions"][1]["raw_content"], missing);
        assert_eq!(
            result["recovery"]["observations"][0]["automatically_selected"],
            false
        );
        assert!(
            result["extraction"]["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["code"] == "recovery_only_observation")
        );
        assert_eq!(model.calls(), 4);
        recognizer.shutdown().await;
    }
}
