#![allow(dead_code)]
use ocr_core::{
    projection::{BusinessCandidate, BusinessField, BusinessParty, BusinessResult},
    review::ReviewSnapshot,
};
use ocr_eval::{Document, GoldEvidence, Label, Manifest, Split, digest};
use ocr_extraction::{Evidence, FieldState, FieldValue};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

pub fn date(value: &str) -> FieldValue {
    FieldValue::Date {
        value: value.into(),
    }
}
pub fn money(amount: &str, period: Option<&str>) -> FieldValue {
    FieldValue::Money {
        amount: amount.into(),
        currency: "VND".into(),
        period: period.map(str::to_owned),
    }
}
pub fn business() -> BusinessResult {
    let evidence = Evidence {
        block_id: "fixture-block".into(),
        page_number: 1,
        region_index: Some(0),
        source: "region_ocr".into(),
        byte_start: 0,
        byte_end: 10,
        quote: "02/02/2000".into(),
        bbox_2d: Some([10, 10, 500, 50]),
        geometry_scope: "source_block".into(),
        detector_score: Some(0.8),
        detector_score_kind: "layout_detection".into(),
        confidence: None,
    };
    let reference = format!("ev_{}", digest(&serde_json::to_vec(&evidence).unwrap()));
    let make = |state, value, values: Vec<FieldValue>| BusinessField {
        state,
        value,
        confidence: None,
        confidence_kind: "uncalibrated_extraction".into(),
        reason_codes: vec!["confidence_uncalibrated".into()],
        candidates: values
            .into_iter()
            .map(|value| BusinessCandidate {
                value,
                origins: vec!["fixture".into()],
                methods: vec!["synthetic_fixture".into()],
                evidence_refs: vec![reference.clone()],
            })
            .collect(),
        evidence_refs: vec![reference.clone()],
        approval: "requires_review".into(),
        correction: None,
    };
    BusinessResult {
        schema_version: "ocr-business/1.0.0".into(),
        source_sha256: None,
        job_id: Uuid::nil(),
        extraction_profile: "vi_rental_contract_v1".into(),
        supported_template: true,
        machine_decision: "needs_review".into(),
        review: ReviewSnapshot::default(),
        parties: vec![BusinessParty {
            id: "party-b".into(),
            role: "tenant".into(),
            field_prefix: "tenant".into(),
            role_evidence_refs: vec![reference.clone()],
        }],
        fields: BTreeMap::from([
            (
                "landlord.birth_date".into(),
                make(
                    FieldState::Found,
                    Some(date("1980-01-01")),
                    vec![date("1980-01-01")],
                ),
            ),
            (
                "tenant.birth_date".into(),
                make(
                    FieldState::Conflict,
                    None,
                    vec![date("2000-02-02"), date("2001-03-03")],
                ),
            ),
            (
                "monthly_rent".into(),
                make(
                    FieldState::Found,
                    Some(money("1200000", Some("month"))),
                    vec![money("1200000", Some("month"))],
                ),
            ),
            (
                "deposit_amount".into(),
                make(
                    FieldState::Found,
                    Some(money("500000", None)),
                    vec![money("500000", None)],
                ),
            ),
        ]),
        evidence: BTreeMap::from([(reference, evidence)]),
        issue_codes: vec!["synthetic_fixture_only".into()],
    }
}
pub fn fixture_files(base: &Path) -> Manifest {
    std::fs::write(
        base.join("business.json"),
        serde_json::to_vec_pretty(&business()).unwrap(),
    )
    .unwrap();
    let documents = [("dev", Split::Dev), ("calibration", Split::Calibration), ("test", Split::Test)].into_iter().map(|(name, split)| {
        let source = format!("Synthetic evaluator mechanics fixture: {name}\nLandlord date: 01/01/1980\nTenant contract date: 02/02/2000\nTenant attachment date: 03/03/2001\nRent: 1000000 VND/month\nDeposit: not present\n");
        let source_path = format!("source-{name}.txt");
        std::fs::write(base.join(&source_path), &source).unwrap();
        Document {
            id: format!("fixture-{name}"), split, group_id: format!("fixture-group-{name}"), source_sha256: digest(source.as_bytes()), source_path, result_path: Some("business.json".into()), result_sha256: Some(digest(&serde_json::to_vec_pretty(&business()).unwrap())),
            labels: BTreeMap::from([
                ("landlord.birth_date".into(), Label { state: FieldState::Found, values: vec![date("1980-01-01")], evidence_gold: None }),
                ("tenant.birth_date".into(), Label { state: FieldState::Conflict, values: vec![date("2000-02-02"), date("2001-03-03")], evidence_gold: Some(vec![GoldEvidence { page_number: 1, source: "region_ocr".into(), quote: "02/02/2000".into() }]) }),
                ("monthly_rent".into(), Label { state: FieldState::Found, values: vec![money("1000000", Some("month"))], evidence_gold: None }),
                ("deposit_amount".into(), Label { state: FieldState::Missing, values: vec![], evidence_gold: None }),
            ]),
            roles: BTreeMap::from([("party-b".into(), "tenant".into())]),
        }
    }).collect();
    Manifest {
        schema_version: "ocr-eval-manifest/1.0.0".into(),
        dataset_name: "synthetic-evaluator-mechanics".into(),
        synthetic: true,
        documents,
    }
}
pub fn save(base: &Path, manifest: &Manifest) -> std::path::PathBuf {
    let path = base.join("manifest.json");
    let mut manifest = manifest.clone();
    for document in &mut manifest.documents {
        if let Some(path) = &document.result_path
            && let Ok(bytes) = std::fs::read(base.join(path))
        {
            document.result_sha256 = Some(digest(&bytes));
        }
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    path
}
