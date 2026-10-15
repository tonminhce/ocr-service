//! Synthetic detector protocol fixtures exercise the Rust pipeline boundary.
//! These tests do not run a layout model or an OCR model and do not establish
//! real-model accuracy or parity on scanned documents.
#![cfg(unix)]

use std::{
    io::Cursor,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    extract::Json,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{GenericImageView, ImageFormat, Rgb, RgbImage};
use ocr_layout::{PageLayout, Region, Task};
use ocr_worker::{
    inference::Recognizer,
    settings::{OcrStrategy, Settings},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TEXT_PROMPT: &str = "OCR and recognize the text in this image. Maintain the original structure and formatting. Output only the recognized text:";
const TABLE_PROMPT: &str = "Convert this table image into a clean Markdown table format. Ensure all rows and columns are preserved accurately. Only output the markdown table:";
const FORMULA_PROMPT: &str = "Convert this mathematical formula into LaTeX format. Wrap the result in appropriate delimiters. Only output the LaTeX string:";

#[derive(Clone)]
enum MockBehavior {
    Mixed,
    FailedTable,
    TruncatedFormula,
    Constant(String),
}

struct CapturedRequest {
    headers: HeaderMap,
    body: Value,
    task: &'static str,
    dimensions: (u32, u32),
}

struct MockVllm {
    base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    completed: Arc<Mutex<Vec<&'static str>>>,
    handle: JoinHandle<()>,
}

impl Drop for MockVllm {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn mock_vllm(behavior: MockBehavior) -> MockVllm {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let completion_order = completed.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let requests = captured.clone();
            let completed = completion_order.clone();
            let behavior = behavior.clone();
            async move {
                let prompt = body["messages"][1]["content"][0]["text"].as_str().unwrap();
                let task = match prompt {
                    TEXT_PROMPT => "text",
                    TABLE_PROMPT => "table",
                    FORMULA_PROMPT => "formula",
                    other => panic!("unexpected regional OCR task prompt: {other}"),
                };
                let data = body["messages"][1]["content"][1]["image_url"]["url"]
                    .as_str()
                    .unwrap();
                let jpeg = STANDARD
                    .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
                    .unwrap();
                assert_eq!(image::guess_format(&jpeg).unwrap(), ImageFormat::Jpeg);
                let dimensions = image::load_from_memory(&jpeg).unwrap().dimensions();
                requests.lock().unwrap().push(CapturedRequest {
                    headers,
                    body,
                    task,
                    dimensions,
                });

                let (status, content, reason, delay_ms) = match &behavior {
                    MockBehavior::Constant(text) => (StatusCode::OK, text.clone(), "stop", 0),
                    MockBehavior::FailedTable if task == "table" => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "private mock upstream diagnostics".into(),
                        "stop",
                        60,
                    ),
                    MockBehavior::TruncatedFormula if task == "formula" => {
                        (StatusCode::OK, "partial formula".into(), "length", 10)
                    }
                    _ => match task {
                        "text" => (StatusCode::OK, "### Tài liệu".into(), "stop", 180),
                        "table" => (
                            StatusCode::OK,
                            "| A | B |\n|---|---|\n| 1 | 2 |".into(),
                            "stop",
                            60,
                        ),
                        "formula" => (StatusCode::OK, r"\[ α + β \]".into(), "stop", 10),
                        _ => unreachable!(),
                    },
                };
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                completed.lock().unwrap().push(task);
                (
                    status,
                    Json(
                        json!({"choices":[{"message":{"content":content},"finish_reason":reason}]}),
                    ),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    MockVllm {
        base_url,
        requests,
        completed,
        handle,
    }
}

/// Executable stand-in for layout-detect: emits a fixed PageLayout without
/// loading weights. Arguments still pass through the real subprocess runner.
struct FakeDetector {
    directory: TempDir,
    binary: PathBuf,
}

impl FakeDetector {
    fn layout(layout: &PageLayout) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("fake-layout.json");
        std::fs::write(&fixture, serde_json::to_vec(layout).unwrap()).unwrap();
        let script = format!(
            "#!/bin/sh\n# Fake detector protocol fixture; no model inference.\nprintf '%s\\n' '{{\"type\":\"ready\",\"version\":1,\"model_sha256\":\"{}\"}}'\nwhile IFS= read -r request; do\n  id=$(printf '%s' \"$request\" | /usr/bin/sed -n 's/.*\"id\":\\([0-9][0-9]*\\).*/\\1/p')\n  printf '{{\"type\":\"layout\",\"id\":%s,\"layout\":' \"$id\"\n  /bin/cat {}\n  printf '}}\\n'\ndone\n",
            "0".repeat(64),
            shell_quote(&fixture)
        );
        Self::script(directory, &script)
    }

    fn sleeping() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("detector.pid");
        // exec replaces the shell, so cancellation cannot leave a shell child.
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec /bin/sleep 30\n",
            shell_quote(&pid)
        );
        Self::script(directory, &script)
    }

    fn script(directory: TempDir, contents: &str) -> Self {
        let binary = directory.path().join("fake-layout-detect");
        std::fs::write(&binary, contents).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { directory, binary }
    }

    fn pid_path(&self) -> PathBuf {
        self.directory.path().join("detector.pid")
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
}

fn settings(server: &MockVllm, detector: &FakeDetector) -> Settings {
    Settings {
        database_url: "postgres://unused-layout-fixture".into(),
        redis_url: "redis://localhost:6379".into(),
        redis_stream: "ocr:test".into(),
        redis_consumer_group: "ocr-test-workers".into(),
        redis_timeout_ms: 2000,
        redis_redispatch_seconds: 1,
        vllm_base_url: server.base_url.clone(),
        vllm_model: "fake-regional-vlm".into(),
        vllm_api_key: Some("layout-fixture-only-key".into()),
        worker_concurrency: 4,
        lease_seconds: 60,
        max_attempts: 3,
        poll_interval_ms: 500,
        request_timeout_seconds: 5,
        job_timeout_seconds: 600,
        max_pages: 20,
        max_image_pixels: 1_000_000,
        max_image_edge: 2048,
        table_crop_padding_pixels: 0,
        pdf_render_dpi: 200,
        page_max_edge: 3500,
        strategy: OcrStrategy::CourseLayout,
        layout_binary: detector.binary.clone(),
        layout_model_path: detector.directory.path().join("unused-fake-model.onnx"),
        layout_model_sha256: "0".repeat(64),
        layout_timeout_seconds: 5,
        layout_pool_size: 1,
        max_regions_per_page: 128,
        max_regions_per_document: 2048,
        region_concurrency: 4,
        inference_revision: String::new(),
        inference_deployment: None,
        checkpoint_enabled: false,
        recovery_max_regions: 0,
        recovery_expansion_pixels: 16,
        recognize_image_regions: false,
        pdf_render_timeout_seconds: 30,
        max_output_tokens: 8192,
        retention_days: 7,
    }
}

fn image_png() -> Vec<u8> {
    let image = RgbImage::from_fn(64, 96, |x, y| Rgb([x as u8, y as u8, 180]));
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, ImageFormat::Png).unwrap();
    output.into_inner()
}

fn region(index: u32, label: &str, task: Task, bbox_2d: [i32; 4]) -> Region {
    let [left, top, right, bottom] = bbox_2d;
    Region {
        index,
        class_id: match label {
            "doc_title" => 6,
            "table" => 21,
            "display_formula" => 5,
            "image" => 14,
            "header" => 12,
            _ => 22,
        },
        label: label.into(),
        task,
        bbox: [
            left as f32 * 0.064,
            top as f32 * 0.096,
            right as f32 * 0.064,
            bottom as f32 * 0.096,
        ],
        bbox_2d,
        polygon: vec![[left, top], [right, top], [right, bottom], [left, bottom]],
        polygon_source: "fake_detector_fixture".into(),
        score: 0.95,
        reading_order: index,
    }
}

fn mixed_layout() -> PageLayout {
    PageLayout {
        width: 64,
        height: 96,
        // Deliberately unsorted protocol input; learned detector indices govern
        // output order, regardless of HTTP completion order.
        regions: vec![
            region(1, "table", Task::Table, [0, 250, 500, 750]),
            region(3, "image", Task::Skip, [500, 250, 1000, 750]),
            region(2, "display_formula", Task::Formula, [0, 750, 1000, 1000]),
            region(0, "doc_title", Task::Text, [0, 0, 1000, 250]),
        ],
        // The real postprocessor numbers this collection independently.
        discarded: vec![region(0, "header", Task::Abandon, [0, 0, 1000, 100])],
    }
}

fn text_layout(count: u32) -> PageLayout {
    PageLayout {
        width: 64,
        height: 96,
        regions: (0..count)
            .map(|i| region(i, "text", Task::Text, [0, 0, 1000, 1000]))
            .collect(),
        discarded: Vec::new(),
    }
}

#[tokio::test]
async fn fake_detector_regions_use_task_crops_and_preserve_reading_order() {
    let detector = FakeDetector::layout(&mixed_layout());
    let server = mock_vllm(MockBehavior::Mixed).await;
    let result = Recognizer::new(settings(&server, &detector))
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["strategy"], "course_layout_vlm");
    assert_eq!(result["image_ocr_override"], false);
    assert_eq!(result["layout_model"]["onnx_sha256"], "0".repeat(64));
    let page = &result["pages"][0];
    assert_eq!(page["page_number"], 1);
    assert_eq!(page["width"], 64);
    assert_eq!(page["height"], 96);
    assert_eq!(page["coordinate_system"], "normalized_0_1000");
    assert_eq!(page["detections"].as_array().unwrap().len(), 4);
    assert_eq!(page["discarded"][0]["label"], "header");
    assert_eq!(page["discarded"][0]["task"], "abandon");
    assert_eq!(page["regions"], result["layout"][0]);
    let regions = page["regions"].as_array().unwrap();
    let labels: Vec<_> = regions
        .iter()
        .map(|r| r["native_label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["doc_title", "table", "display_formula", "image"]);
    for (index, region) in regions.iter().enumerate() {
        assert_eq!(region["index"], index);
        assert_eq!(region["reading_order"], index);
        assert_eq!(region["polygon_source"], "fake_detector_fixture");
    }
    assert_eq!(regions[0]["raw_content"], "### Tài liệu");
    assert_eq!(regions[0]["content"], "# Tài liệu");
    assert_eq!(regions[3]["content"], Value::Null);
    assert_eq!(regions[3]["task_type"], "skip");
    assert_eq!(page["coverage"]["ocr_attempted_regions"], 3);
    assert_eq!(page["coverage"]["skipped_regions"], 1);
    assert_eq!(page["coverage"]["abandoned_regions"], 1);
    let markdown = "# Tài liệu\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n$$\nα + β\n$$\n\n![](page=0,bbox=[500, 250, 1000, 750])";
    assert_eq!(result["markdown"], markdown);
    assert_eq!(page["markdown"], markdown);
    assert_eq!(
        *server.completed.lock().unwrap(),
        ["formula", "table", "text"]
    );

    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "skip and abandon regions must not call OCR"
    );
    for request in requests.iter() {
        assert_eq!(
            request.headers["authorization"],
            "Bearer layout-fixture-only-key"
        );
        assert_eq!(request.body["model"], "fake-regional-vlm");
        assert_eq!(request.body["temperature"], 0);
        assert_eq!(request.body["top_p"], 0.00001);
        assert_eq!(request.body["top_k"], 1);
        assert_eq!(request.body["repetition_penalty"], 1.1);
        assert_eq!(
            request.body["chat_template_kwargs"]["enable_thinking"],
            false
        );
        let expected = if request.task == "table" {
            (112, 140)
        } else {
            (196, 84)
        };
        assert_eq!(
            request.dimensions, expected,
            "{} regional JPEG crop",
            request.task
        );
        assert_ne!(request.dimensions, (64, 96), "full page must not reach OCR");
    }
}

#[tokio::test]
async fn image_ocr_uses_outer_containers_and_keeps_nested_geometry_without_text() {
    let detector = FakeDetector::layout(&PageLayout {
        width: 64,
        height: 96,
        discarded: vec![],
        regions: vec![
            region(0, "image", Task::Skip, [50, 100, 200, 500]),
            region(1, "image", Task::Skip, [0, 0, 450, 1000]),
            region(2, "image", Task::Skip, [600, 100, 800, 500]),
            region(3, "image", Task::Skip, [550, 0, 1000, 1000]),
        ],
    });
    let server = mock_vllm(MockBehavior::Constant("Visible container text".into())).await;
    let mut config = settings(&server, &detector);
    config.recognize_image_regions = true;
    let result = Recognizer::new(config)
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result["image_ocr_policy"]["name"],
        "outermost_containers_v1"
    );
    let page = &result["pages"][0];
    assert_eq!(page["image_ocr_skipped_nested_regions"], json!([0, 2]));
    assert_eq!(page["coverage"]["ocr_attempted_regions"], 2);
    assert_eq!(page["coverage"]["skipped_regions"], 2);
    assert_eq!(page["regions"].as_array().unwrap().len(), 4);
    for index in [0, 2] {
        assert_eq!(page["regions"][index]["content"], Value::Null);
        assert_eq!(page["regions"][index]["task_type"], "skip");
    }
    for index in [1, 3] {
        assert_eq!(page["regions"][index]["content"], "Visible container text");
    }
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn equal_image_boxes_select_one_owner_instead_of_skipping_both() {
    let detector = FakeDetector::layout(&PageLayout {
        width: 64,
        height: 96,
        discarded: vec![],
        regions: vec![
            region(1, "image", Task::Skip, [0, 0, 1000, 1000]),
            region(0, "image", Task::Skip, [0, 0, 1000, 1000]),
        ],
    });
    let server = mock_vllm(MockBehavior::Constant("Visible text".into())).await;
    let mut config = settings(&server, &detector);
    config.recognize_image_regions = true;
    let result = Recognizer::new(config)
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result["pages"][0]["image_ocr_skipped_nested_regions"],
        json!([1])
    );
    assert_eq!(result["pages"][0]["coverage"]["ocr_attempted_regions"], 1);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn table_margin_changes_only_recognition_frame_and_records_extension() {
    let detector = FakeDetector::layout(&mixed_layout());
    let server = mock_vllm(MockBehavior::Mixed).await;
    let mut config = settings(&server, &detector);
    config.table_crop_padding_pixels = 112;
    let result = Recognizer::new(config)
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result["layout_profile"],
        "course-root-sdk-0.1.4+table-padding"
    );
    assert_eq!(
        result["recognition_preprocessing"]["table_crop_padding_pixels"],
        112
    );
    assert_eq!(
        result["pages"][0]["regions"][1]["bbox_2d"],
        json!([0, 250, 500, 750])
    );
    let requests = server.requests.lock().unwrap();
    for request in requests.iter() {
        assert_eq!(
            request.dimensions,
            if request.task == "table" {
                (336, 364)
            } else {
                (196, 84)
            }
        );
    }
}

#[tokio::test]
async fn image_override_reads_detected_crop_and_marks_enhanced_profile() {
    let image_region = region(0, "image", Task::Skip, [500, 250, 1000, 750]);
    let detector = FakeDetector::layout(&PageLayout {
        width: 64,
        height: 96,
        regions: vec![image_region],
        discarded: vec![region(0, "header", Task::Abandon, [0, 0, 1000, 100])],
    });
    let server = mock_vllm(MockBehavior::Constant("Image text 001234".into())).await;
    let mut config = settings(&server, &detector);
    config.recognize_image_regions = true;
    let result = Recognizer::new(config)
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["layout_profile"], "course-root-sdk-0.1.4+image-text");
    assert_eq!(result["image_ocr_override"], true);
    let page = &result["pages"][0];
    assert_eq!(page["raw_regions"][0]["task_type"], "image_text");
    assert_eq!(page["regions"][0]["content"], "Image text 001234");
    assert_eq!(page["coverage"]["ocr_attempted_regions"], 1);
    assert_eq!(page["coverage"]["skipped_regions"], 0);
    assert_eq!(page["coverage"]["abandoned_regions"], 1);
    assert_eq!(
        result["markdown"],
        "![](page=0,bbox=[500, 250, 1000, 750])\n\nImage text 001234"
    );
    assert_eq!(page["markdown"], result["markdown"]);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].task, "text");
    assert_eq!(requests[0].dimensions, (112, 140));
    assert_ne!(requests[0].dimensions, (64, 96));
}

#[tokio::test]
async fn image_override_keeps_geometry_and_placeholder_for_empty_ocr() {
    for text in ["", " \n\t"] {
        let detector = FakeDetector::layout(&PageLayout {
            width: 64,
            height: 96,
            regions: vec![region(0, "image", Task::Skip, [500, 250, 1000, 750])],
            discarded: vec![],
        });
        let server = mock_vllm(MockBehavior::Constant(text.into())).await;
        let mut config = settings(&server, &detector);
        config.recognize_image_regions = true;
        let result = Recognizer::new(config)
            .unwrap()
            .recognize(image_png(), "image/png", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["layout"][0].as_array().unwrap().len(), 1);
        assert_eq!(result["pages"][0]["raw_regions"][0]["raw_content"], text);
        assert_eq!(result["pages"][0]["coverage"]["empty_model_responses"], 1);
        assert_eq!(result["markdown"], "![](page=0,bbox=[500, 250, 1000, 750])");
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_detector_dimensions_and_geometry_fail_closed_before_ocr() {
    let mut wrong_dimensions = text_layout(1);
    wrong_dimensions.width = 65;
    let mut out_of_range = text_layout(1);
    out_of_range.regions[0].bbox_2d[2] = 1001;
    let inverted_image = PageLayout {
        width: 64,
        height: 96,
        regions: vec![region(0, "image", Task::Skip, [900, 0, 100, 500])],
        discarded: Vec::new(),
    };
    let mut outside_pixels = text_layout(1);
    // Raw model boxes may extend beyond an edge before SDK clipping. A box
    // wholly outside the page, paired with plausible crop coordinates, is
    // invalid detector geometry and must fail at the subprocess boundary.
    outside_pixels.regions[0].bbox = [65.0, 0.0, 66.0, 96.0];
    let zero_area_image = PageLayout {
        width: 64,
        height: 96,
        regions: vec![region(0, "image", Task::Skip, [100, 0, 100, 500])],
        discarded: Vec::new(),
    };
    for (name, layout) in [
        ("dimensions", wrong_dimensions),
        ("coordinate range", out_of_range),
        ("inverted skipped image", inverted_image),
        ("nonintersecting pixel bounds", outside_pixels),
        ("zero area skipped image", zero_area_image),
    ] {
        let detector = FakeDetector::layout(&layout);
        let server = mock_vllm(MockBehavior::Mixed).await;
        let error = Recognizer::new(settings(&server, &detector))
            .unwrap()
            .recognize(image_png(), "image/png", CancellationToken::new())
            .await
            .expect_err(name);
        assert!(!error.retryable, "{name}: {}", error.message);
        assert!(
            server.requests.lock().unwrap().is_empty(),
            "{name} must never fall back to whole-page OCR"
        );
    }
}

#[tokio::test]
async fn any_region_upstream_failure_fails_the_entire_document() {
    for behavior in [MockBehavior::FailedTable, MockBehavior::TruncatedFormula] {
        let detector = FakeDetector::layout(&mixed_layout());
        let is_http_error = matches!(behavior, MockBehavior::FailedTable);
        let server = mock_vllm(behavior).await;
        let error = Recognizer::new(settings(&server, &detector))
            .unwrap()
            .recognize(image_png(), "image/png", CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.retryable, is_http_error);
        assert!(error.message.contains(if is_http_error {
            "page 1 region 1"
        } else {
            "page 1 region 2"
        }));
        assert!(
            error
                .message
                .contains(if is_http_error { "503" } else { "incomplete" })
        );
        assert!(!error.message.contains("private mock upstream diagnostics"));
    }
}

#[tokio::test]
async fn page_and_document_region_budgets_reject_before_any_ocr() {
    for document_limit in [false, true] {
        let detector = FakeDetector::layout(&text_layout(2));
        let server = mock_vllm(MockBehavior::Mixed).await;
        let mut config = settings(&server, &detector);
        if document_limit {
            config.max_regions_per_document = 1;
        } else {
            config.max_regions_per_page = 1;
        }
        let error = Recognizer::new(config)
            .unwrap()
            .recognize(image_png(), "image/png", CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!error.retryable);
        assert!(error.message.contains("region limit"));
        assert!(server.requests.lock().unwrap().is_empty());
    }
}

async fn wait_for_pid(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = contents.parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake detector never started")
}

fn pid_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn detector_cancellation_kills_and_reaps_the_direct_process() {
    let detector = FakeDetector::sleeping();
    let server = mock_vllm(MockBehavior::Mixed).await;
    let recognizer = Recognizer::new(settings(&server, &detector)).unwrap();
    let cancelled = CancellationToken::new();
    let token = cancelled.clone();
    let recognition =
        tokio::spawn(async move { recognizer.recognize(image_png(), "image/png", token).await });
    let pid = wait_for_pid(&detector.pid_path()).await;
    assert!(pid_exists(pid));
    cancelled.cancel();
    let error = tokio::time::timeout(Duration::from_secs(3), recognition)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.retryable);
    assert!(error.message.contains("cancelled"));
    assert!(
        !pid_exists(pid),
        "cancelled detector {pid} must be killed and reaped"
    );
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn detector_deadline_kills_and_reaps_the_direct_process() {
    let detector = FakeDetector::sleeping();
    let server = mock_vllm(MockBehavior::Mixed).await;
    let mut config = settings(&server, &detector);
    config.layout_timeout_seconds = 1;
    let recognizer = Recognizer::new(config).unwrap();
    let recognition = tokio::spawn(async move {
        recognizer
            .recognize(image_png(), "image/png", CancellationToken::new())
            .await
    });
    let pid = wait_for_pid(&detector.pid_path()).await;
    let error = tokio::time::timeout(Duration::from_secs(3), recognition)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.retryable);
    assert!(error.message.contains("timed out"));
    assert!(
        !pid_exists(pid),
        "timed-out detector {pid} must be killed and reaped"
    );
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cumulative_document_text_budget_rejects_valid_individual_completions() {
    let detector = FakeDetector::layout(&text_layout(3));
    let server = mock_vllm(MockBehavior::Constant("x".repeat(1536 * 1024))).await;
    let mut config = settings(&server, &detector);
    config.region_concurrency = 1;
    let error = Recognizer::new(config)
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("4 MiB text budget"));
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn serialized_result_budget_accounts_for_audit_and_markdown_copies() {
    let detector = FakeDetector::layout(&text_layout(3));
    // Under the 4 MiB transcription budget and per-response 2 MiB bound, but
    // content/raw_content/layout/page/Markdown views exceed the storage budget.
    let server = mock_vllm(MockBehavior::Constant("abcdefghij".repeat(105_000))).await;
    let error = Recognizer::new(settings(&server, &detector))
        .unwrap()
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(
        error.message.contains("16 MiB storage budget"),
        "{}",
        error.message
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn abandoned_geometry_counts_toward_the_document_region_budget_before_ocr() {
    let detector = FakeDetector::layout(&PageLayout {
        width: 64,
        height: 96,
        regions: vec![],
        discarded: vec![
            region(0, "header", Task::Abandon, [0, 0, 1000, 100]),
            region(1, "header", Task::Abandon, [0, 800, 1000, 900]),
        ],
    });
    let server = mock_vllm(MockBehavior::Mixed).await;
    let mut config = settings(&server, &detector);
    config.max_regions_per_document = 1;
    let recognizer = Recognizer::new(config).unwrap();
    let error = recognizer
        .recognize(image_png(), "image/png", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("including discarded"));
    assert!(server.requests.lock().unwrap().is_empty());
    recognizer.shutdown().await;
}
