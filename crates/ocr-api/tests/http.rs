use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use ocr_api::{AppState, config::Settings, router};
use ocr_core::store::Store;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

const TOKEN: &str = "test-service-token-123456";

fn app() -> axum::Router {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@localhost:1/unused")
        .unwrap();
    router(AppState::new(
        Store::from_pool(pool),
        Arc::new(Settings {
            database_url: "unused".into(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            api_token: TOKEN.into(),
            review_token: None,
            review_principal: None,
            max_upload_bytes: 32,
            upload_timeout_seconds: 1,
            upload_concurrency: 2,
            max_pending_jobs: 1000,
            enqueue_deadlines: Default::default(),
            responses: Default::default(),
            transport: Default::default(),
        }),
    ))
}

fn upload(parts: &[(&str, &[u8])], auth: bool) -> Request<Body> {
    let mut body = Vec::new();
    for (name, bytes) in parts {
        body.extend_from_slice(format!("--ocr-boundary\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"test.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--ocr-boundary--\r\n");
    let mut request = Request::post("/v1/ocr/jobs")
        .header("content-type", "multipart/form-data; boundary=ocr-boundary");
    if auth {
        request = request.header("authorization", format!("Bearer {TOKEN}"));
    }
    request.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn untrusted_upload_is_rejected_before_parsing_or_storage() {
    let response = app()
        .oneshot(upload(&[("file", b"invalid")], false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().contains_key("x-request-id"));
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn rejects_invalid_uploads_without_touching_storage() {
    for (parts, expected) in [
        (vec![], StatusCode::BAD_REQUEST),
        (vec![("file", b"".as_slice())], StatusCode::BAD_REQUEST),
        (vec![("other", b"data".as_slice())], StatusCode::BAD_REQUEST),
        (
            vec![("file", b"not an image".as_slice())],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            vec![
                ("file", b"%PDF-1.7".as_slice()),
                ("file", b"%PDF-1.7".as_slice()),
            ],
            StatusCode::BAD_REQUEST,
        ),
        (vec![("file", &[0; 33])], StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let response = app().oneshot(upload(&parts, true)).await.unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["content-type"], "application/json");
    }
}

#[tokio::test]
async fn accepts_probe_without_credentials_and_rejects_malformed_job_ids() {
    let response = app()
        .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app()
        .oneshot(
            Request::get("/v1/ocr/jobs/not-a-uuid")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn submission_replay_and_conflict_follow_http_contract() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let store = Store::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let app = router(AppState::new(
        store,
        Arc::new(Settings {
            database_url: url,
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            api_token: TOKEN.into(),
            review_token: None,
            review_principal: None,
            max_upload_bytes: 32,
            upload_timeout_seconds: 1,
            upload_concurrency: 2,
            max_pending_jobs: 1000,
            enqueue_deadlines: Default::default(),
            responses: Default::default(),
            transport: Default::default(),
        }),
    ));
    let key = uuid::Uuid::new_v4().to_string();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut request = upload(&[("file", b"%PDF-1.7")], true);
        request
            .headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        ids.push(body["id"].as_str().unwrap().to_string());
        assert_eq!(body["extraction_profile"], "none");
    }
    assert_eq!(ids[0], ids[1]);
    let mut request = upload(&[("file", b"%PDF-1.6")], true);
    request
        .headers_mut()
        .insert("idempotency-key", key.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let response = app
        .oneshot(
            Request::get(format!("/v1/ocr/jobs/{}", ids[0]))
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn profile_submissions_are_persisted_and_replays_require_matching_settings() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let store = Store::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let app = router(AppState::new(
        store,
        Arc::new(Settings {
            database_url: url,
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            api_token: TOKEN.into(),
            review_token: None,
            review_principal: None,
            max_upload_bytes: 32,
            upload_timeout_seconds: 1,
            upload_concurrency: 2,
            max_pending_jobs: 1000,
            enqueue_deadlines: Default::default(),
            responses: Default::default(),
            transport: Default::default(),
        }),
    ));
    let key = uuid::Uuid::new_v4().to_string();
    let mut ids = Vec::new();
    for parts in [
        vec![
            ("file", b"%PDF-1.7".as_slice()),
            ("extraction_profile", b"vi_rental_contract_v1".as_slice()),
        ],
        vec![
            ("extraction_profile", b"vi_rental_contract_v1".as_slice()),
            ("file", b"%PDF-1.7".as_slice()),
        ],
    ] {
        let mut request = upload(&parts, true);
        request
            .headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["extraction_profile"], "vi_rental_contract_v1");
        ids.push(body["id"].as_str().unwrap().to_owned());
    }
    assert_eq!(ids[0], ids[1]);
    let mut changed_profile = upload(
        &[("file", b"%PDF-1.7"), ("extraction_profile", b"none")],
        true,
    );
    changed_profile
        .headers_mut()
        .insert("idempotency-key", key.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(changed_profile).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let mut omitted_profile = upload(&[("file", b"%PDF-1.7")], true);
    omitted_profile
        .headers_mut()
        .insert("idempotency-key", key.parse().unwrap());
    assert_eq!(
        app.oneshot(omitted_profile).await.unwrap().status(),
        StatusCode::CONFLICT
    );
}

const REVIEW_TOKEN: &str = "privileged-test-review-token-at-least-32-characters";
fn review_app_with_store(store: Store) -> axum::Router {
    router(AppState::new(
        store,
        Arc::new(Settings {
            database_url: "unused".into(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            api_token: TOKEN.into(),
            review_token: Some(REVIEW_TOKEN.into()),
            review_principal: Some("configured-review-service".into()),
            max_upload_bytes: 32,
            upload_timeout_seconds: 1,
            upload_concurrency: 2,
            max_pending_jobs: 1000,
            enqueue_deadlines: Default::default(),
            responses: Default::default(),
            transport: Default::default(),
        }),
    ))
}
fn review_app() -> axum::Router {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@localhost:1/unused")
        .unwrap();
    review_app_with_store(Store::from_pool(pool))
}
fn review_request(token: &str, body: Value) -> Request<Body> {
    Request::post(format!("/v1/ocr/jobs/{}/review", uuid::Uuid::nil()))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn review_is_disabled_by_default_and_requires_distinct_privileged_auth() {
    let body = serde_json::json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"action":"reject","reason":"checked source"});
    assert_eq!(
        app()
            .oneshot(review_request(TOKEN, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        review_app()
            .oneshot(review_request(TOKEN, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        review_app()
            .oneshot(review_request("", body))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let forged_actor = serde_json::json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"action":"reject","reason":"checked source","actor":"claimed-user"});
    assert_eq!(
        review_app()
            .oneshot(review_request(REVIEW_TOKEN, forged_actor))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn additive_views_validate_paths_and_review_body_bounds_before_storage() {
    for path in [
        "/v1/ocr/jobs/invalid/status",
        "/v1/ocr/jobs/invalid/business",
        "/v1/ocr/jobs/invalid/artifacts/source",
    ] {
        assert_eq!(
            app()
                .oneshot(
                    Request::get(path)
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        app()
            .oneshot(
                Request::get(format!(
                    "/v1/ocr/jobs/{}/artifacts/arbitrary",
                    uuid::Uuid::nil()
                ))
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let oversized = serde_json::json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"action":"reject","reason":"x".repeat(70*1024)});
    assert_eq!(
        review_app()
            .oneshot(review_request(REVIEW_TOKEN, oversized))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn compact_views_artifacts_and_privileged_review_roundtrip() {
    use ocr_core::domain::NewJob;
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let store = Store::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let id = uuid::Uuid::new_v4();
    store
        .submit(NewJob {
            id,
            filename: "unsafe\"\r\nname.pdf".into(),
            media_type: "application/pdf".into(),
            sha256: "test".into(),
            input: b"%PDF-original".to_vec(),
            idempotency_key: None,
            extraction_profile: "vi_rental_contract_v1".into(),
        })
        .await
        .unwrap();
    let lease = store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    let extraction = serde_json::json!({"schema_version":"rental-extraction/1.0.0","profile_version":"vi_rental_contract_v1","normalizer_version":"test","policy_version":"review_all_uncalibrated/1.0.0","supported_template":false,"decision":"needs_review","source_blocks":[],"parties":[],"fields":{},"identity_documents":[],"issues":[],"limitations":[]});
    let original = serde_json::json!({"extraction":extraction,"markdown":"original **markdown**","layout":[[]]});
    store
        .complete_retaining_input(id, lease.lease_token, original.clone())
        .await
        .unwrap();
    let app = review_app_with_store(store.clone());
    for kind in [
        "status",
        "business",
        "artifacts/source",
        "artifacts/markdown",
        "artifacts/layout",
        "artifacts/evidence",
        "artifacts/result",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/v1/ocr/jobs/{id}/{kind}"))
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{kind}");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        if kind == "status" {
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert!(body.get("result").is_none());
            assert_eq!(body["review_revision"], 0);
        } else if kind == "artifacts/source" {
            assert_eq!(
                response.headers()["content-disposition"],
                "attachment; filename=\"document.pdf\""
            );
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                b"%PDF-original".as_slice()
            );
        }
    }
    let request_id = uuid::Uuid::new_v4();
    let payload = serde_json::json!({"request_id":request_id,"expected_revision":0,"action":"reject","reason":"Source incomplete"});
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/ocr/jobs/{id}/review"))
                    .header("authorization", format!("Bearer {REVIEW_TOKEN}"))
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["revision"], 1);
        assert_eq!(body["principal"], "configured-review-service");
    }
    let stale = serde_json::json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"action":"reopen","reason":"try stale"});
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/ocr/jobs/{id}/review"))
                .header("authorization", format!("Bearer {REVIEW_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(stale.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        store.get(id).await.unwrap().unwrap().result.unwrap(),
        original
    );
    let response = app
        .oneshot(
            Request::get(format!("/v1/ocr/jobs/{id}/review"))
                .header("authorization", format!("Bearer {REVIEW_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let history: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(history["events"].as_array().unwrap().len(), 1);
}

fn bounded_settings(url: String) -> Settings {
    Settings {
        database_url: url,
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        api_token: TOKEN.into(),
        review_token: Some(REVIEW_TOKEN.into()),
        review_principal: Some("configured-review-service".into()),
        max_upload_bytes: 32,
        upload_timeout_seconds: 1,
        upload_concurrency: 2,
        max_pending_jobs: 1000,
        enqueue_deadlines: Default::default(),
        responses: ocr_api::config::ResponseSettings {
            max_json_bytes: 4096,
            max_source_bytes: 32,
            large_reserved_bytes: 8192,
            ..Default::default()
        },
        transport: Default::default(),
    }
}
fn authenticated_get(id: uuid::Uuid, suffix: &str) -> Request<Body> {
    Request::get(format!("/v1/ocr/jobs/{id}{suffix}"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn bounded_legacy_replay_artifact_and_review_contracts_are_preserved() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let store = Store::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let app = router(AppState::new(
        store.clone(),
        Arc::new(bounded_settings(url)),
    ));
    let extraction = serde_json::json!({"schema_version":"rental-extraction/1.0.0","profile_version":"vi_rental_contract_v1","normalizer_version":"test","policy_version":"review_all_uncalibrated/1.0.0","supported_template":false,"decision":"needs_review","source_blocks":[],"parties":[],"fields":{},"identity_documents":[],"issues":[],"limitations":[]});
    for (case, result) in [
        (
            "normal",
            serde_json::json!({"extraction":extraction,"markdown":"Tiếng Việt","layout":[[]]}),
        ),
        (
            "stored",
            serde_json::json!({"extraction": {"limitations":["x".repeat(8192)]}, "markdown":"x".repeat(8192),"layout":[{"content":"x".repeat(8192)}]}),
        ),
        // Stored result fits, but the enclosing legacy Job pushes wire size past4096.
        ("wire", serde_json::json!({"blob":"x".repeat(3800)})),
    ] {
        let key = uuid::Uuid::new_v4().to_string();
        let mut request = upload(&[("file", b"%PDF-1.7")], true);
        request
            .headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        let accepted = app.clone().oneshot(request).await.unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert_eq!(accepted.headers()["cache-control"], "private, no-store");
        let location = accepted.headers()["location"].clone();
        let created = json_body(accepted).await;
        let id = uuid::Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
        let fetched = app
            .clone()
            .oneshot(authenticated_get(id, ""))
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
        assert_eq!(json_body(fetched).await, created);
        let lease = store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
        store
            .complete_retaining_input(id, lease.lease_token, result.clone())
            .await
            .unwrap();
        let expected = serde_json::to_value(store.get(id).await.unwrap().unwrap()).unwrap();
        let fetched = app
            .clone()
            .oneshot(authenticated_get(id, ""))
            .await
            .unwrap();
        let mut replay = upload(&[("file", b"%PDF-1.7")], true);
        replay
            .headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        let replay = app.clone().oneshot(replay).await.unwrap();
        if case == "normal" {
            assert_eq!(fetched.status(), StatusCode::OK);
            assert_eq!(json_body(fetched).await, expected);
            assert_eq!(replay.status(), StatusCode::ACCEPTED);
            assert_eq!(replay.headers()["location"], location);
            assert_eq!(json_body(replay).await, expected);
        } else {
            for response in [fetched, replay] {
                assert_eq!(
                    response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{case}"
                );
                assert_eq!(
                    json_body(response).await["error"]["code"],
                    "response_limit_exceeded"
                );
            }
        }
        let status = app
            .clone()
            .oneshot(authenticated_get(id, "/status"))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        assert!(json_body(status).await.get("result").is_none());
        let source = app
            .clone()
            .oneshot(authenticated_get(id, "/artifacts/source"))
            .await
            .unwrap();
        assert_eq!(source.status(), StatusCode::OK);
        assert_eq!(
            source.into_body().collect().await.unwrap().to_bytes(),
            b"%PDF-1.7".as_slice()
        );
        if case == "stored" {
            for suffix in [
                "/business",
                "/artifacts/result",
                "/artifacts/markdown",
                "/artifacts/layout",
                "/artifacts/evidence",
            ] {
                let response = app
                    .clone()
                    .oneshot(authenticated_get(id, suffix))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{suffix}"
                );
                assert_eq!(
                    json_body(response).await["error"]["code"],
                    "response_limit_exceeded"
                );
            }
            let mut review = review_request(
                REVIEW_TOKEN,
                serde_json::json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"action":"reject","reason":"bounded fixture"}),
            );
            *review.uri_mut() = format!("/v1/ocr/jobs/{id}/review").parse().unwrap();
            let response = app.clone().oneshot(review).await.unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(
                json_body(response).await["error"]["code"],
                "response_limit_exceeded"
            );
            assert_eq!(store.review_history(id, 0).await.unwrap().events.len(), 0);
            let mut conflicting = upload(&[("file", b"%PDF-1.6")], true);
            conflicting
                .headers_mut()
                .insert("idempotency-key", key.parse().unwrap());
            assert_eq!(
                app.clone().oneshot(conflicting).await.unwrap().status(),
                StatusCode::CONFLICT
            );
        }
        assert_eq!(
            store.get(id).await.unwrap().unwrap().result.unwrap(),
            result
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn enqueue_http_deadline_is_transient_replayable_and_pool_recovers() {
    use ocr_core::store::SubmitDeadlines;
    use std::time::{Duration, Instant};
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .unwrap();
    let store = Store::from_pool(pool.clone());
    store.migrate().await.unwrap();
    let mut settings = bounded_settings(url);
    settings.enqueue_deadlines = SubmitDeadlines::new(
        Duration::from_millis(800),
        Duration::from_millis(100),
        Duration::from_millis(400),
    )
    .unwrap();
    let app = router(AppState::new(store, Arc::new(settings)));
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1,$2)")
        .bind(0x4f43_5253_i32)
        .bind(0x4a4f_4251_i32)
        .execute(&mut *lock)
        .await
        .unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let make_request = || {
        let mut request = upload(&[("file", b"%PDF-1.7")], true);
        request
            .headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        request
    };
    let started = Instant::now();
    let response = app.clone().oneshot(make_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    assert_eq!(
        json_body(response).await["error"]["code"],
        "enqueue_timeout"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    lock.rollback().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            app.clone().oneshot(make_request()).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
    }
    let rows: (i64, i64) = sqlx::query_as("SELECT count(*), count(o.job_id) FROM jobs j LEFT JOIN job_outbox o ON o.job_id=j.id WHERE j.idempotency_key=$1").bind(key).fetch_one(&pool).await.unwrap();
    assert_eq!(rows, (1, 1));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn read_deadline_includes_pool_acquire_and_releases_response_slots() {
    use std::time::{Duration, Instant};
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let store = Store::from_pool(pool.clone());
    store.migrate().await.unwrap();
    let mut settings = bounded_settings(url);
    settings.responses.read_timeout = Duration::from_millis(60);
    settings.responses.large_concurrency = 1;
    let app = router(AppState::new(store, Arc::new(settings)));
    let held = pool.acquire().await.unwrap();
    let id = uuid::Uuid::new_v4();
    let started = Instant::now();
    for suffix in ["", "/status", "/business", "/artifacts/result"] {
        let response = app
            .clone()
            .oneshot(authenticated_get(id, suffix))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json_body(response).await["error"]["code"],
            "operation_timeout"
        );
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(held);
    // SQLx returns a dropped connection asynchronously. A fresh 60ms request
    // can legitimately time out during recovery on a busy host. Verify bounded
    // recovery with the same admission slots, rather than assuming immediate IO.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = app
                .clone()
                .oneshot(authenticated_get(id, ""))
                .await
                .unwrap();
            if response.status() == StatusCode::NOT_FOUND {
                break;
            }
            // Leaked response admission/byte reservations yield 429 and fail.
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                json_body(response).await["error"]["code"],
                "operation_timeout"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pool and response slots must recover within two seconds");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SHOW statement_timeout")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "0"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SHOW lock_timeout")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "0"
    );
}
