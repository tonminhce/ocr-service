//! TEST_DATABASE_URL=postgres://... cargo test -p ocr-core --test review -- --ignored
use std::{collections::BTreeMap, str::FromStr};

use ocr_core::{
    domain::NewJob,
    review::{ReviewAction, ReviewError, ReviewRequest},
    store::Store,
};
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

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
        let schema = format!("ocr_review_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(6)
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
    async fn job(&self) -> (Uuid, Value) {
        let id = Uuid::new_v4();
        self.store
            .submit(NewJob {
                id,
                filename: "private.pdf".into(),
                media_type: "application/pdf".into(),
                sha256: "test".into(),
                input: b"%PDF-original".to_vec(),
                idempotency_key: None,
                extraction_profile: "vi_rental_contract_v1".into(),
            })
            .await
            .unwrap();
        let lease = self.store.claim_by_id(id, 120, 3).await.unwrap().unwrap();
        let result =
            json!({"extraction":ocr_extraction::extract(&[]),"markdown":"original","layout":[]});
        assert!(
            self.store
                .complete_retaining_input(id, lease.lease_token, result.clone())
                .await
                .unwrap()
        );
        (id, result)
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
fn request(revision: i64, action: ReviewAction) -> ReviewRequest {
    ReviewRequest {
        request_id: Uuid::new_v4(),
        expected_revision: revision,
        action,
        reason: "Reviewed source document".into(),
        corrections: BTreeMap::new(),
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn decisions_are_revisioned_idempotent_immutable_and_retained() {
    let db = Database::new().await;
    let (id, original) = db.job().await;
    let request = request(0, ReviewAction::Reject);
    let first = db
        .store
        .append_review(id, "trusted-review-principal", request.clone())
        .await
        .unwrap();
    let repeat = db
        .store
        .append_review(id, "trusted-review-principal", request.clone())
        .await
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(repeat.revision, 1);
    assert_eq!(repeat.status, "rejected");
    assert!(matches!(
        db.store
            .append_review(id, "another-principal", request)
            .await,
        Err(ReviewError::Conflict)
    ));
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().result.unwrap(),
        original
    );
    let status = db.store.get_status(id).await.unwrap().unwrap();
    assert_eq!(status.review_revision, 1);
    assert_eq!(status.review_status, "rejected");
    let history = db.store.review_history(id, 0).await.unwrap();
    assert_eq!(history.events.len(), 1);
    assert_eq!(history.events[0].principal, "trusted-review-principal");
    assert!(
        sqlx::query("UPDATE job_review_events SET reason='changed' WHERE job_id=$1")
            .bind(id)
            .execute(&db.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM job_review_events WHERE job_id=$1")
            .bind(id)
            .execute(&db.pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE jobs SET updated_at=clock_timestamp()-INTERVAL '90 days' WHERE id=$1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.store.prune(7).await.is_err());
    assert_eq!(
        db.store
            .get_status(id)
            .await
            .unwrap()
            .unwrap()
            .review_revision,
        1
    );
    assert_eq!(
        db.store.review_history(id, 0).await.unwrap().events.len(),
        1
    );
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().result.unwrap(),
        original
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn concurrent_decisions_allow_exactly_one_expected_revision() {
    let db = Database::new().await;
    let (id, _) = db.job().await;
    let a = db
        .store
        .append_review(id, "principal", request(0, ReviewAction::Reject));
    let b = db
        .store
        .append_review(id, "principal", request(0, ReviewAction::Reopen));
    let (a, b) = tokio::join!(a, b);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(ReviewError::Conflict)) || matches!(b, Err(ReviewError::Conflict)));
    assert_eq!(
        db.store.review_history(id, 0).await.unwrap().events.len(),
        1
    );
    assert!(
        db.store
            .append_review(id, "principal", request(1, ReviewAction::Approve))
            .await
            .is_err()
    );
    db.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn compact_status_does_not_deserialize_malformed_legacy_result() {
    let db = Database::new().await;
    let (id, _) = db.job().await;
    sqlx::query("UPDATE jobs SET result=$2 WHERE id=$1")
        .bind(id)
        .bind(json!({"extraction":"malformed"}))
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        db.store.get_status(id).await.unwrap().unwrap().status,
        "succeeded"
    );
    assert!(matches!(
        db.store.get_business(id).await,
        Err(ReviewError::InvalidStoredResult)
    ));
    assert!(
        db.store
            .get_artifact(id, "unknown")
            .await
            .unwrap()
            .is_none()
    );
    assert!(db.store.get_artifact(id, "source").await.unwrap().is_some());
    db.cleanup().await;
}

fn reviewable_extraction() -> ocr_extraction::ExtractionResult {
    use ocr_extraction::{
        Candidate, Evidence, FieldResult, FieldState, FieldValue, Party, PartyRole,
    };
    let mut extraction = ocr_extraction::extract(&[]);
    extraction.supported_template = true;
    extraction.issues.clear();
    extraction.fields.clear();
    let evidence:Evidence=serde_json::from_value(json!({"block_id":"b1","page_number":1,"region_index":0,"source":"ocr","byte_start":0,"byte_end":11,"quote":"source text","bbox_2d":[1,1,100,20],"geometry_scope":"source_block","detector_score":0.5,"detector_score_kind":"layout_detection_not_ocr_confidence","confidence":null})).unwrap();
    let field = |value: FieldValue| FieldResult {
        state: FieldState::Found,
        selected: Some(value.clone()),
        candidates: vec![Candidate {
            value,
            raw_value: "source text".into(),
            origin: "ocr".into(),
            method: "test_fixture".into(),
            confidence: None,
            evidence: vec![evidence.clone()],
        }],
        ..FieldResult::default()
    };
    for (id, role) in [("a", PartyRole::Landlord), ("b", PartyRole::Tenant)] {
        let mut fields = BTreeMap::new();
        for name in [
            "name",
            "id_number",
            "birth_date",
            "issue_date",
            "residence_address",
        ] {
            let value = match name {
                "id_number" => FieldValue::Identifier {
                    value: "001204001171".into(),
                },
                "birth_date" | "issue_date" => FieldValue::Date {
                    value: "2004-01-04".into(),
                },
                _ => FieldValue::Text {
                    value: "source text".into(),
                },
            };
            fields.insert(name.into(), field(value));
        }
        if id == "b" {
            let f = fields.get_mut("birth_date").unwrap();
            f.state = FieldState::Conflict;
            f.selected = None;
            let mut other = f.candidates[0].clone();
            other.value = FieldValue::Date {
                value: "2001-06-13".into(),
            };
            f.candidates.push(other);
        }
        extraction.parties.push(Party {
            id: id.into(),
            designator: id.into(),
            role,
            role_evidence: vec![evidence.clone()],
            contract_fields: fields.clone(),
            identity_fields: BTreeMap::new(),
            fields,
            linked_identity_documents: vec![],
        });
    }
    extraction.fields.insert(
        "monthly_rent".into(),
        field(FieldValue::Money {
            amount: "10000000".into(),
            currency: "VND".into(),
            period: Some("month".into()),
        }),
    );
    extraction.fields.insert(
        "property_address".into(),
        field(FieldValue::Text {
            value: "source text".into(),
        }),
    );
    extraction
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn evidence_correction_requires_separate_human_approval_and_reopen_revokes_it() {
    use ocr_core::review::Correction;
    use ocr_extraction::FieldValue;
    let db = Database::new().await;
    let (id, _) = db.job().await;
    let original =
        json!({"extraction":reviewable_extraction(),"markdown":"immutable raw","layout":[]});
    sqlx::query("UPDATE jobs SET result=$2 WHERE id=$1")
        .bind(id)
        .bind(&original)
        .execute(&db.pool)
        .await
        .unwrap();
    let business = db.store.get_business(id).await.unwrap().unwrap();
    assert!(
        db.store
            .append_review(id, "principal", request(0, ReviewAction::Approve))
            .await
            .is_err()
    );
    let mut correction = request(0, ReviewAction::Correct);
    correction.corrections.insert(
        "tenant.birth_date".into(),
        Correction {
            value: FieldValue::Date {
                value: "2004-01-04".into(),
            },
            evidence_refs: business.fields["tenant.birth_date"].evidence_refs.clone(),
        },
    );
    let corrected = db
        .store
        .append_review(id, "principal", correction.clone())
        .await
        .unwrap();
    assert_eq!(corrected.status, "needs_review");
    let business = db.store.get_business(id).await.unwrap().unwrap();
    assert_eq!(
        business.fields["tenant.birth_date"].approval,
        "human_corrected_requires_review"
    );
    assert_eq!(business.fields["tenant.birth_date"].candidates.len(), 2);
    assert_eq!(
        db.store
            .append_review(id, "principal", request(1, ReviewAction::Approve))
            .await
            .unwrap()
            .status,
        "approved"
    );
    let approved = db.store.get_business(id).await.unwrap().unwrap();
    assert_eq!(
        approved.fields["tenant.birth_date"].approval,
        "human_approved"
    );
    assert_eq!(
        db.store
            .append_review(id, "principal", correction)
            .await
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        db.store
            .append_review(id, "principal", request(2, ReviewAction::Reopen))
            .await
            .unwrap()
            .status,
        "needs_review"
    );
    assert_eq!(
        db.store.get(id).await.unwrap().unwrap().result.unwrap(),
        original
    );
    db.cleanup().await;
}
