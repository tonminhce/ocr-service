//! Storage limits guard SQLx allocations before HTTP's independent wire writer.
use ocr_core::{
    domain::NewJob,
    projection::Artifact,
    review::{ReviewError, ReviewRequest},
    store::{Store, StoreError},
};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn guarded_projection_boundaries_source_bytes_and_review_preserve_raw_result() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let store = Store::from_pool(pool.clone());
    store.migrate().await.unwrap();
    let id = Uuid::new_v4();
    store
        .submit(NewJob {
            id,
            filename: "bounds.pdf".into(),
            media_type: "application/pdf".into(),
            sha256: "synthetic".into(),
            input: b"%PDF-original".to_vec(),
            idempotency_key: None,
            extraction_profile: "vi_rental_contract_v1".into(),
        })
        .await
        .unwrap();
    let lease = store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
    let extraction = json!({"schema_version":"rental-extraction/1.0.0","profile_version":"vi_rental_contract_v1","normalizer_version":"test","policy_version":"review_all_uncalibrated/1.0.0","supported_template":false,"decision":"needs_review","source_blocks":[],"parties":[],"fields":{},"identity_documents":[],"issues":[],"limitations":[]});
    let original = json!({"extraction":extraction,"markdown":"abc\nTiếng Việt","layout":[[]]});
    store
        .complete_retaining_input(id, lease.lease_token, original.clone())
        .await
        .unwrap();
    assert!(matches!(
        store.get_artifact_bounded(id, "source", 12).await,
        Err(StoreError::ResponseTooLarge)
    ));
    assert!(
        matches!(store.get_artifact_bounded(id, "source", 13).await.unwrap(),Some(Artifact::Source {bytes,..}) if bytes==b"%PDF-original")
    );
    let size: i32 =
        sqlx::query_scalar("SELECT octet_length((result->'layout')::text) FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(matches!(
        store
            .get_artifact_bounded(id, "layout", size as usize - 1)
            .await,
        Err(StoreError::ResponseTooLarge)
    ));
    assert!(
        matches!(store.get_artifact_bounded(id, "layout", size as usize).await.unwrap(),Some(Artifact::Json(value)) if value==json!([[]]))
    );
    assert!(
        store
            .get_artifact_bounded(id, "diagnostics", 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.get_business_bounded(id, 1).await,
        Err(ReviewError::ResponseTooLarge)
    ));
    let expected = store.get_business(id).await.unwrap().unwrap();
    let actual = store.get_business_bounded(id, 4096).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let request:ReviewRequest = serde_json::from_value(json!({"request_id":Uuid::new_v4(),"expected_revision":0,"action":"reject","reason":"synthetic bounded review"})).unwrap();
    assert!(matches!(
        store
            .append_review_bounded(id, "test-review-service", request.clone(), 1)
            .await,
        Err(ReviewError::ResponseTooLarge)
    ));
    assert_eq!(store.review_history(id, 0).await.unwrap().events.len(), 0);
    let snapshot = store
        .append_review_bounded(id, "test-review-service", request.clone(), 4096)
        .await
        .unwrap();
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.status, "rejected");
    // Idempotent review replay does not need extraction; preserve its original
    // outcome even when a later caller configures a smaller extraction budget.
    let replay = store
        .append_review_bounded(id, "test-review-service", request.clone(), 1)
        .await
        .unwrap();
    assert_eq!(replay.revision, 1);
    let mut conflict = request;
    conflict.reason = "a different request under the same request_id".into();
    assert!(matches!(
        store
            .append_review_bounded(id, "test-review-service", conflict, 1)
            .await,
        Err(ReviewError::Conflict)
    ));
    assert_eq!(store.get(id).await.unwrap().unwrap().result, Some(original));
    let raw_snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM job_review_events WHERE job_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(raw_snapshot["status"], "rejected");
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
    assert!(
        store
            .get_artifact_bounded(Uuid::new_v4(), "source", 1)
            .await
            .unwrap()
            .is_none()
    );
}
