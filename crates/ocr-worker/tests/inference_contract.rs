use std::{
    io::Cursor,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::Json,
    http::{HeaderMap, Response, StatusCode},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ocr_worker::{
    inference::{Recognizer, parse_completion},
    preparation::{normalize_image, parse_pdf_pages},
    settings::Settings,
};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type Captured = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

struct MockServer {
    base: String,
    handle: JoinHandle<()>,
    captured: Captured,
}
impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn server(status: StatusCode, body: Value) -> MockServer {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let requests = captured.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, Json(request): Json<Value>| {
            let requests = requests.clone();
            let body = body.clone();
            async move {
                requests.lock().unwrap().push((headers, request));
                (status, Json(body))
            }
        }),
    );
    serve(app, captured).await
}

async fn serve(app: Router, captured: Captured) -> MockServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockServer {
        base,
        handle,
        captured,
    }
}

fn settings(base: &str) -> Settings {
    Settings {
        database_url: "postgres://unused".into(),
        redis_url: "redis://localhost:6379".into(),
        redis_stream: "ocr:test".into(),
        redis_consumer_group: "ocr-test-workers".into(),
        redis_timeout_ms: 2000,
        redis_redispatch_seconds: 1,
        vllm_base_url: base.into(),
        vllm_model: "test-vision-model".into(),
        vllm_api_key: Some("test-secret".into()),
        worker_concurrency: 2,
        lease_seconds: 60,
        max_attempts: 3,
        poll_interval_ms: 500,
        request_timeout_seconds: 5,
        job_timeout_seconds: 600,
        max_pages: 20,
        max_image_pixels: 40_000_000,
        max_image_edge: 2048,
        table_crop_padding_pixels: 0,
        pdf_render_dpi: 200,
        page_max_edge: 3500,
        strategy: ocr_worker::settings::OcrStrategy::FullPage,
        layout_binary: "unused".into(),
        layout_model_path: "unused".into(),
        layout_model_sha256: String::new(),
        layout_timeout_seconds: 60,
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

fn completion(text: &str, reason: &str) -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": reason}]})
}

fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(width, height, image::Rgba(rgba))
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

#[tokio::test]
async fn records_genuine_usage_crop_dimensions_and_declared_identity_without_changing_text() {
    let mut body = completion("Nội dung nguyên bản 012345678901", "stop");
    body["usage"] = json!({"prompt_tokens":17,"completion_tokens":3,"total_tokens":20});
    let server = server(StatusCode::OK, body).await;
    let mut config = settings(&server.base);
    config.inference_deployment = Some(ocr_worker::telemetry::DeploymentDeclaration::parse(
        r#"{"runtime":"test-runtime","runtime_version":"1.0","max_sequences":1,"max_pixels":1572864}"#
    ).unwrap());
    let recognizer = Recognizer::new(config).unwrap();
    let result = recognizer
        .recognize(png(8, 4, [255; 4]), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["markdown"], "Nội dung nguyên bản 012345678901");
    assert!(
        result.get("execution").is_none(),
        "manifest stays beside raw output at commit time"
    );
    let trace = recognizer.telemetry_snapshot();
    assert_eq!(
        (
            trace.attempts_started,
            trace.succeeded,
            trace.failed,
            trace.cancelled
        ),
        (1, 1, 0, 0)
    );
    assert_eq!(trace.usage.prompt_tokens.sum, 17);
    assert_eq!(trace.usage.completion_tokens.sum, 3);
    assert_eq!(trace.usage.total_tokens.sum, 20);
    assert_eq!(trace.inputs_by_task[0].width_pixels.min, Some(8));
    assert_eq!(trace.inputs_by_task[0].height_pixels.min, Some(4));
    assert_eq!(trace.inputs_by_task[0].pixels.sum, 32);
    assert!(trace.inputs_by_task[0].encoded_jpeg_bytes.sum > 0);
    assert_eq!(trace.ttft_seconds, None);
    let manifest = recognizer.execution_manifest("none");
    assert_eq!(
        manifest["identity"]["runtime_declaration_attestation"],
        "operator_declared_unverified"
    );
    assert_eq!(
        manifest["identity"]["verified_server_configuration"],
        Value::Null
    );
    assert_eq!(
        manifest["identity"]["runtime_declaration"]["max_sequences"],
        1
    );
    assert_eq!(
        manifest["identity"]["prompt_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let serialized = serde_json::to_string(&manifest).unwrap();
    for private in ["test-secret", server.base.as_str(), "Nội dung nguyên bản"] {
        assert!(!serialized.contains(private));
    }
}

#[tokio::test]
async fn absent_invalid_partial_and_overflow_usage_never_become_estimated_tokens() {
    let cases = [
        (None, "missing"),
        (Some(json!(null)), "missing"),
        (Some(json!({"prompt_tokens":7})), "partial"),
        (
            Some(json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})),
            "zero",
        ),
        (Some(json!({"prompt_tokens":-1})), "invalid"),
        (
            Some(json!({"prompt_tokens":10,"total_tokens":5})),
            "invalid",
        ),
        (
            Some(json!({"completion_tokens":9,"total_tokens":2})),
            "invalid",
        ),
        (Some(json!({"completion_tokens":1.5})), "invalid"),
        (Some(json!({"completion_tokens":"4"})), "invalid"),
        (
            Some(json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":9})),
            "invalid",
        ),
        (
            Some(json!({"prompt_tokens":u64::MAX,"completion_tokens":1,"total_tokens":0})),
            "invalid",
        ),
        (
            Some(serde_json::from_str(r#"{"total_tokens":18446744073709551616}"#).unwrap()),
            "invalid",
        ),
    ];
    for (usage, expected) in cases {
        let mut response = completion("unchanged", "stop");
        if let Some(usage) = usage {
            response["usage"] = usage;
        }
        let server = server(StatusCode::OK, response).await;
        let recognizer = Recognizer::new(settings(&server.base)).unwrap();
        assert_eq!(
            recognizer.transcribe_jpeg(b"fixture-jpeg").await.unwrap(),
            "unchanged"
        );
        let trace = recognizer.telemetry_snapshot();
        assert_eq!(trace.inputs_by_task[0].dimensions_unavailable, 1);
        match expected {
            "missing" => assert_eq!(trace.usage.missing_responses, 1),
            "invalid" => assert_eq!(trace.usage.invalid_responses, 1),
            "partial" => {
                assert_eq!(trace.usage.prompt_tokens.sum, 7);
                assert_eq!(trace.usage.total_tokens.observations, 0);
                assert_eq!(trace.usage.completion_tokens.min, None);
            }
            "zero" => {
                assert_eq!(trace.usage.total_tokens.observations, 1);
                assert_eq!(trace.usage.total_tokens.min, Some(0));
            }
            _ => unreachable!(),
        }
        if expected == "missing" || expected == "invalid" {
            assert_eq!(trace.usage.total_tokens.observations, 0);
            assert_eq!(trace.usage.prompt_tokens.min, None);
        }
    }
}

#[tokio::test]
async fn bounded_finish_reasons_include_truncation_and_unknown_but_do_not_accept_them() {
    use ocr_core::metrics::ModelFinish;
    for (reason, index) in [
        ("length", ModelFinish::Length),
        ("private arbitrary reason", ModelFinish::Other),
        ("content_filter", ModelFinish::ContentFilter),
    ] {
        let mut response = completion("partial", reason);
        response["usage"] = json!({"total_tokens":5});
        let server = server(StatusCode::OK, response).await;
        let recognizer = Recognizer::new(settings(&server.base)).unwrap();
        let error = recognizer.transcribe_jpeg(b"jpeg").await.unwrap_err();
        assert!(!error.retryable);
        assert!(!error.message.contains("private arbitrary reason"));
        let trace = recognizer.telemetry_snapshot();
        assert_eq!(
            (trace.attempts_started, trace.failed, trace.cancelled),
            (1, 1, 0)
        );
        assert_eq!(trace.finish_reasons[index as usize], 1);
        assert_eq!(
            trace.usage.total_tokens.sum, 5,
            "genuine usage for truncated output still incurred"
        );
    }
}

#[test]
fn declaration_validation_is_bounded_strict_and_does_not_echo_input() {
    use ocr_worker::telemetry::DeploymentDeclaration;
    for input in [
        r#"{"api_key":"private-secret"}"#,
        r#"{"runtime":"private-secret\n"}"#,
        r#"{"max_sequences":0}"#,
        r#"{"weights_sha256":"bad"}"#,
    ] {
        let error = DeploymentDeclaration::parse(input)
            .err()
            .expect("invalid declaration");
        assert!(!error.to_string().contains("private-secret"));
    }
    assert!(DeploymentDeclaration::parse(&"x".repeat(8193)).is_err());
    assert!(DeploymentDeclaration::parse("{}").is_ok());
}

#[tokio::test]
async fn cancellation_distinguishes_queued_work_from_started_http_and_releases_local_r() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let starts = Arc::new(AtomicUsize::new(0));
    let observed = starts.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let observed = observed.clone();
            async move {
                if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                }
                Json(completion("okay", "stop"))
            }
        }),
    );
    let server = serve(app, Arc::default()).await;
    let mut config = settings(&server.base);
    config.region_concurrency = 1;
    let recognizer = Recognizer::new(config).unwrap();
    let clone = recognizer.clone();
    let first = tokio::spawn(async move { clone.transcribe_jpeg(b"jpeg").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while starts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let clone = recognizer.clone();
    let queued = tokio::spawn(async move { clone.transcribe_jpeg(b"jpeg").await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    assert_eq!(recognizer.telemetry_snapshot().attempts_started, 1);
    assert_eq!(recognizer.telemetry_snapshot().cancelled, 0);
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(recognizer.telemetry_snapshot().cancelled, 1);
    assert_eq!(recognizer.transcribe_jpeg(b"jpeg").await.unwrap(), "okay");
    let trace = recognizer.telemetry_snapshot();
    assert_eq!(
        (
            trace.attempts_started,
            trace.succeeded,
            trace.failed,
            trace.cancelled
        ),
        (2, 1, 0, 1)
    );
}

#[tokio::test]
async fn sends_openai_multimodal_contract_and_preserves_page_result() {
    let server = server(StatusCode::OK, completion("# Receipt\n\nTotal: 42", "stop")).await;
    let recognizer = Recognizer::new(settings(&server.base)).unwrap();
    let result = recognizer
        .recognize(png(8, 4, [255; 4]), "image/png", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["strategy"], "full_page_vlm");
    assert_eq!(result["model"], "test-vision-model");
    assert_eq!(result["pages"][0]["page_number"], 1);
    assert_eq!(result["markdown"], "# Receipt\n\nTotal: 42");
    let captured = server.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let (headers, request) = &captured[0];
    assert_eq!(headers["authorization"], "Bearer test-secret");
    assert_eq!(request["model"], "test-vision-model");
    assert_eq!(request["temperature"], 0);
    assert_eq!(request["max_tokens"], 8192);
    assert_eq!(request["stream"], false);
    assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
    assert!(
        request["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("untrusted data")
    );
    let data = request["messages"][1]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap();
    let jpeg = STANDARD
        .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
        .unwrap();
    assert_eq!(
        image::guess_format(&jpeg).unwrap(),
        image::ImageFormat::Jpeg
    );
    assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 8);
}

#[tokio::test]
async fn classifies_rate_limits_and_server_errors_as_retryable() {
    for (status, retryable) in [
        (429, true),
        (503, true),
        (400, false),
        (401, false),
        (404, false),
    ] {
        let server = server(
            StatusCode::from_u16(status).unwrap(),
            json!({"error":"private upstream message"}),
        )
        .await;
        let error = Recognizer::new(settings(&server.base))
            .unwrap()
            .transcribe_jpeg(b"jpeg")
            .await
            .unwrap_err();
        assert_eq!(error.retryable, retryable, "status {status}");
        assert!(!error.message.contains("private upstream message"));
    }
}

#[tokio::test]
async fn truncated_and_malformed_results_never_become_successes() {
    for response in [
        completion("partial text", "length"),
        json!({"choices":[]}),
        json!({"choices":[{"message":{"content":null},"finish_reason":"stop"}]}),
        json!({"choices":[{"message":{"content":"text"}}]}),
    ] {
        let server = server(StatusCode::OK, response).await;
        let error = Recognizer::new(settings(&server.base))
            .unwrap()
            .transcribe_jpeg(b"jpeg")
            .await
            .unwrap_err();
        assert!(!error.retryable);
    }
    let error = parse_completion(&serde_json::to_vec(&completion("partial", "length")).unwrap())
        .unwrap_err();
    assert!(error.message.contains("incomplete"));
    assert!(parse_completion(b"not JSON").is_err());
}

#[tokio::test]
async fn stopped_empty_page_output_is_preserved_and_labeled_empty() {
    for text in ["", "   \n"] {
        let server = server(StatusCode::OK, completion(text, "stop")).await;
        let result = Recognizer::new(settings(&server.base))
            .unwrap()
            .recognize(png(8, 8, [255; 4]), "image/png", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["pages"][0]["markdown"], text);
        assert_eq!(result["pages"][0]["empty"], true);
        assert_eq!(result["markdown"], text);
    }
}

#[tokio::test]
async fn refuses_redirects_instead_of_forwarding_upstream_credentials() {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let requests = captured.clone();
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async { axum::response::Redirect::temporary("/redirect-target") }),
        )
        .route(
            "/redirect-target",
            post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let requests = requests.clone();
                async move {
                    requests.lock().unwrap().push((headers, request));
                    Json(completion("leak", "stop"))
                }
            }),
        );
    let server = serve(app, captured).await;
    let error = Recognizer::new(settings(&server.base))
        .unwrap()
        .transcribe_jpeg(b"jpeg")
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("307"));
    assert!(server.captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bounds_streamed_responses_without_content_length() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let chunks =
                (0..5).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b' '; 512 * 1024])));
            Response::builder()
                .body(Body::from_stream(futures_util::stream::iter(chunks)))
                .unwrap()
        }),
    );
    let server = serve(app, Arc::new(Mutex::new(Vec::new()))).await;
    let error = Recognizer::new(settings(&server.base))
        .unwrap()
        .transcribe_jpeg(b"jpeg")
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("2 MiB"));
}

#[tokio::test]
async fn inference_timeout_is_retryable_and_unauthenticated_mode_omits_header() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Json(completion("late", "stop"))
        }),
    );
    let server = serve(app, Arc::new(Mutex::new(Vec::new()))).await;
    let mut config = settings(&server.base);
    config.request_timeout_seconds = 1;
    let error = Recognizer::new(config)
        .unwrap()
        .transcribe_jpeg(b"jpeg")
        .await
        .unwrap_err();
    assert!(error.retryable);
    let server = self::server(StatusCode::OK, completion("ok", "stop")).await;
    let mut config = settings(&server.base);
    config.vllm_api_key = None;
    Recognizer::new(config)
        .unwrap()
        .transcribe_jpeg(b"jpeg")
        .await
        .unwrap();
    assert!(
        !server.captured.lock().unwrap()[0]
            .0
            .contains_key("authorization")
    );
}

#[test]
fn validates_dimensions_before_decode_and_resizes_with_white_alpha_background() {
    let input = png(100, 80, [0, 0, 0, 0]);
    let error = normalize_image(&input, 100, 32).unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("pixel limit"));
    let normalized = normalize_image(&input, 8000, 32).unwrap();
    let image = image::load_from_memory(&normalized).unwrap().to_rgb8();
    assert_eq!(image.width(), 32);
    assert!(image.height() <= 32);
    assert!(image.get_pixel(0, 0).0.iter().all(|&v| v > 250));
    assert!(normalize_image(b"corrupt PNG", 8000, 32).is_err());
}

fn exif_jpeg(orientation: u16) -> Vec<u8> {
    // Four distinct quadrants provide expected pixel positions independently of
    // image's orientation implementation: red/green over blue/yellow.
    let image = image::RgbImage::from_fn(80, 40, |x, y| {
        image::Rgb(match (x < 40, y < 20) {
            (true, true) => [255, 0, 0],
            (false, true) => [0, 255, 0],
            (true, false) => [0, 0, 255],
            (false, false) => [255, 255, 0],
        })
    });
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
        .encode_image(&image)
        .unwrap();
    // Minimal little-endian TIFF IFD containing the standard Orientation tag.
    let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    exif.extend_from_slice(&orientation.to_le_bytes());
    exif.extend_from_slice(&[0; 6]); // inline value padding and null next IFD
    let mut tagged = jpeg[..2].to_vec();
    tagged.extend_from_slice(&[0xff, 0xe1]);
    tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    tagged.extend_from_slice(&exif);
    tagged.extend_from_slice(&jpeg[2..]);
    tagged
}

fn assert_quadrants(image: &image::RgbImage, expected: [[u8; 3]; 4]) {
    let centers = [
        (image.width() / 4, image.height() / 4),
        (image.width() * 3 / 4, image.height() / 4),
        (image.width() / 4, image.height() * 3 / 4),
        (image.width() * 3 / 4, image.height() * 3 / 4),
    ];
    for ((x, y), expected) in centers.into_iter().zip(expected) {
        for (&actual, expected) in image.get_pixel(x, y).0.iter().zip(expected) {
            assert!(
                actual.abs_diff(expected) < 20,
                "wrong color at ({x},{y}): {actual}, expected {expected}"
            );
        }
    }
}

#[test]
fn applies_phone_jpeg_rotation_and_mirroring_before_metadata_is_stripped() {
    let rotated = exif_jpeg(6); // 90 degrees clockwise
    let output = normalize_image(&rotated, 3200, 96).unwrap();
    let image = image::load_from_memory(&output).unwrap().to_rgb8();
    assert_eq!((image.width(), image.height()), (40, 80));
    assert_quadrants(
        &image,
        [[0, 0, 255], [255, 0, 0], [255, 255, 0], [0, 255, 0]],
    );
    let resized = image::load_from_memory(&normalize_image(&rotated, 3200, 32).unwrap())
        .unwrap()
        .to_rgb8();
    assert_eq!((resized.width(), resized.height()), (16, 32));
    assert_quadrants(
        &resized,
        [[0, 0, 255], [255, 0, 0], [255, 255, 0], [0, 255, 0]],
    );
    assert!(!normalize_image(&rotated, 3199, 96).unwrap_err().retryable);

    let mirrored = exif_jpeg(2); // horizontal mirror
    let output = normalize_image(&mirrored, 3200, 96).unwrap();
    let image = image::load_from_memory(&output).unwrap().to_rgb8();
    assert_eq!((image.width(), image.height()), (80, 40));
    assert_quadrants(
        &image,
        [[0, 255, 0], [255, 0, 0], [255, 255, 0], [0, 0, 255]],
    );
}

#[tokio::test]
async fn rejects_forged_media_type_before_inference() {
    let server = server(StatusCode::OK, completion("unused", "stop")).await;
    let recognizer = Recognizer::new(settings(&server.base)).unwrap();
    assert!(
        recognizer
            .recognize(png(8, 8, [255; 4]), "image/jpeg", CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        recognizer
            .recognize(
                b"%PDF-fake".to_vec(),
                "text/plain",
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        recognizer
            .recognize(
                b"not a PDF".to_vec(),
                "application/pdf",
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(server.captured.lock().unwrap().is_empty());
}

#[test]
fn pdf_page_count_encryption_and_limits_fail_closed() {
    assert_eq!(
        parse_pdf_pages("Title: sample\nPages: 2\nEncrypted: no\n", 20).unwrap(),
        2
    );
    for metadata in [
        "Pages: 21\n",
        "Pages: 0\n",
        "Pages: bogus\n",
        "Title: no pages\n",
        "Pages: 1\nEncrypted: yes (print:yes)\n",
    ] {
        let error = parse_pdf_pages(metadata, 20).unwrap_err();
        assert!(!error.retryable);
    }
}

#[test]
fn validates_configuration_without_leaking_secrets() {
    let mut config = settings("http://127.0.0.1:8000/v1");
    assert!(config.validate().is_ok());
    config.redis_url = "https://redis-user:redis-secret@example.test".into();
    let error = config.validate().unwrap_err().to_string();
    assert!(!error.contains("redis-secret"));
    assert!(!error.contains("redis-user"));
    config.redis_url = "redis://localhost:6379".into();
    config.redis_timeout_ms = 0;
    assert!(config.validate().is_err());
    config.redis_timeout_ms = 2000;
    config.redis_redispatch_seconds = 0;
    assert!(config.validate().is_err());
    config.redis_redispatch_seconds = 30;
    config.vllm_base_url = "http://user:super-secret@example.test/v1".into();
    let error = config.validate().unwrap_err().to_string();
    assert!(!error.contains("super-secret"));
    config.vllm_base_url = "file:///tmp/server".into();
    assert!(config.validate().is_err());
    config.vllm_base_url = "http://localhost/v1".into();
    config.worker_concurrency = 0;
    assert!(config.validate().is_err());
    config.worker_concurrency = 2;
    config.max_pages = 0;
    assert!(config.validate().is_err());
}

#[tokio::test]
#[ignore = "requires Poppler (pdfinfo and pdftoppm)"]
async fn pdf_pages_render_in_order_and_page_limit_is_checked_before_inference() {
    let input = include_bytes!("fixtures/two_pages.pdf").to_vec();
    let server = server(StatusCode::OK, completion("page text", "stop")).await;
    let mut config = settings(&server.base);
    config.max_image_edge = 128;
    let result = Recognizer::new(config.clone())
        .unwrap()
        .recognize(input.clone(), "application/pdf", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["pages"].as_array().unwrap().len(), 2);
    assert_eq!(result["pages"][0]["page_number"], 1);
    assert_eq!(result["pages"][1]["page_number"], 2);
    assert_eq!(result["markdown"], "page text\n\n---\n\npage text");
    {
        let captured = server.captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        for (_, request) in captured.iter() {
            let data = request["messages"][1]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap();
            let jpeg = STANDARD
                .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
                .unwrap();
            let decoded = image::load_from_memory(&jpeg).unwrap();
            assert!(decoded.width() <= 128 && decoded.height() <= 128);
        }
    }
    config.max_pages = 1;
    let error = Recognizer::new(config)
        .unwrap()
        .recognize(input, "application/pdf", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("2 pages; the limit is 1"));
    assert_eq!(server.captured.lock().unwrap().len(), 2);
}

#[test]
fn validates_warm_pool_checkpoint_revision_and_recovery_budgets() {
    let baseline = settings("http://127.0.0.1:8000/v1");
    assert!(baseline.validate().is_ok());
    for pool_size in [0, baseline.worker_concurrency + 1, 9] {
        let mut config = baseline.clone();
        config.layout_pool_size = pool_size;
        assert!(config.validate().is_err());
    }
    let mut config = baseline.clone();
    config.checkpoint_enabled = true;
    assert!(config.validate().is_err());
    config.inference_revision = "deployment-pinned-revision-01".into();
    assert!(config.validate().is_ok());
    for revision in ["x".repeat(129), "bad\nrevision".into(), "đổi-model".into()] {
        config.inference_revision = revision;
        assert!(config.validate().is_err());
    }
    let mut config = baseline;
    config.recovery_max_regions = 17;
    assert!(config.validate().is_err());
    config.recovery_max_regions = 0;
    config.recovery_expansion_pixels = 0;
    assert!(config.validate().is_err());
    config.recovery_expansion_pixels = 129;
    assert!(config.validate().is_err());
}
