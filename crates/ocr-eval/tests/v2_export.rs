use std::path::Path;

use ocr_eval::{digest, v2, v2_export};
use serde_json::{Value, json};

fn extraction_fixture() -> Value {
    let page_text = "HOP DONG THUE MUA — DU LIEU GIA.\nBen A (cho thue) — Ho ten: NGUOI MAU A.\nCCCD so: 012345678901.\nTien thue: 1.234.567 dong/thang.\n";
    let landlord_line = "Ben A (cho thue) — Ho ten: NGUOI MAU A.";
    let id_line = "CCCD so: 012345678901.";
    let rent_line = "Tien thue: 1.234.567 dong/thang.";
    let span = |line: &str| {
        let start = page_text
            .as_bytes()
            .windows(line.len())
            .position(|window| window == line.as_bytes())
            .unwrap();
        json!({"start": start, "end": start + line.len(), "unit": "utf8_bytes"})
    };
    json!({
        "schema_version": "1.0.0",
        "logical_documents": [{"document_id": "contract", "kind": "rental_contract", "pages": [1]}],
        "roles": [
            {"role_id": "landlord_1", "logical_document_id": "contract", "role_type": "landlord"},
            {"role_id": "tenant_1", "logical_document_id": "contract", "role_type": "tenant"}
        ],
        "source_artifacts": [
            {"artifact_id": "ocr_page_1", "kind": "ocr_text", "page_number": 1,
             "sha256": digest(page_text.as_bytes()), "text": page_text}
        ],
        "fields": {
            "contract.parties.landlord_1.name": {
                "logical_document_id": "contract", "role_id": "landlord_1", "value_status": "found",
                "raw_value": "NGUOI MAU A", "normalized_value": {"type": "text", "value": "NGUOI MAU A"},
                "method": "extracted",
                "evidence": [{"artifact_id": "ocr_page_1", "page_number": 1, "quote": landlord_line, "span": span(landlord_line), "polygon": null}],
                "confidence": null, "candidates": [], "validation_issues": [], "derivation": null},
            "contract.parties.landlord_1.id_number": {
                "logical_document_id": "contract", "role_id": "landlord_1", "value_status": "found",
                "raw_value": "012345678901", "normalized_value": {"type": "identifier", "value": "012345678901"},
                "method": "extracted",
                "evidence": [{"artifact_id": "ocr_page_1", "page_number": 1, "quote": id_line, "span": span(id_line), "polygon": null}],
                "confidence": null, "candidates": [], "validation_issues": [], "derivation": null},
            "contract.monthly_rent": {
                "logical_document_id": "contract", "role_id": null, "value_status": "found",
                "raw_value": "1.234.567", "normalized_value": {"type": "money", "amount": "1234567", "currency": "VND", "period": "month"},
                "method": "extracted",
                "evidence": [{"artifact_id": "ocr_page_1", "page_number": 1, "quote": rent_line, "span": span(rent_line), "polygon": null}],
                "confidence": null, "candidates": [], "validation_issues": [], "derivation": null},
            "contract.parties.tenant_1.phone": {
                "logical_document_id": "contract", "role_id": "tenant_1", "value_status": "missing",
                "raw_value": null, "normalized_value": null, "method": "none",
                "evidence": [], "confidence": null, "candidates": [], "validation_issues": [], "derivation": null}
        },
        "validation_issues": [], "review": null,
        "metadata": {"schema_version": "1.0.0", "business_schema_id": "synthetic_rental_contract_v1"}
    })
}

fn result_fixture() -> Value {
    json!({
        "strategy": "course_layout_vlm", "model": "synthetic/test",
        "pages": [{"page_number": 1, "width": 1000, "height": 600,
                   "coordinate_system": "normalized_0_1000", "empty": false,
                   "markdown": "", "regions": [], "coverage": {},
                   "detections": [], "discarded": [], "raw_regions": []}],
        "layout": [], "markdown": ""
    })
}

fn export_sidecar(base: &Path) -> (Value, String) {
    let extraction_path = base.join("extraction.json");
    let result_path = base.join("result.json");
    std::fs::write(
        &extraction_path,
        serde_json::to_vec(&extraction_fixture()).unwrap(),
    )
    .unwrap();
    std::fs::write(&result_path, serde_json::to_vec(&result_fixture()).unwrap()).unwrap();
    let source_bytes = b"synthetic source, not a real document";
    let source_sha = digest(source_bytes);
    std::fs::write(base.join("source.txt"), source_bytes).unwrap();
    let sidecar_path = base.join("prediction.json");
    let report =
        v2_export::export(&extraction_path, &result_path, &source_sha, &sidecar_path).unwrap();
    assert_eq!(report.pages, 1);
    assert_eq!(report.entities, 3);
    assert_eq!(report.transcripts, 1);
    assert!(report.fields >= 3);
    assert!(report.abstained_pages.is_empty());
    let sidecar: Value = serde_json::from_slice(&std::fs::read(&sidecar_path).unwrap()).unwrap();
    (sidecar, source_sha)
}

fn gold_manifest_for(sidecar: &Value, source_sha: &str) -> Value {
    let mut entities = Vec::new();
    for entity in sidecar["entities"].as_array().unwrap() {
        let mut gold = entity.clone();
        if gold["kind"] == "party" {
            gold["role"] = json!(gold["role"].as_str().unwrap_or("other"));
        }
        entities.push(gold);
    }
    let mut observations = Vec::new();
    for observation in sidecar["observations"].as_array().unwrap() {
        let mut gold = observation.clone();
        gold["origin"] = json!("human_gold");
        gold["evidence"] = Value::Null;
        observations.push(gold);
    }
    let fields: Vec<Value> = sidecar["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| {
            json!({
                "target": field,
                "critical": true,
                "annotation": {
                    "annotator_ids": ["labeler-1", "labeler-2"],
                    "prediction_hidden": true,
                    "pixels_checked": true,
                    "adjudicated": true,
                    "adjudicator_id": "adjudicator-3"
                }
            })
        })
        .collect();
    json!({
        "schema_version": v2::SCHEMA,
        "normalization_policy": v2::NORMALIZATION,
        "dataset_name": "synthetic-v2-export",
        "synthetic": true,
        "cases": [{
            "id": "case-1", "group_id": "group-1", "template_family": "family-1",
            "split": "test", "cohort": "representative", "quality": "scan",
            "source_sha256": source_sha, "source_path": "source.txt",
            "prediction_path": "prediction.json",
            "prediction_sha256": "0".repeat(64),
            "pages": sidecar["pages"],
            "entities": entities,
            "observations": observations,
            "fields": fields
        }]
    })
}

#[test]
fn exported_sidecar_passes_the_v2_loader_and_scores() {
    let base = tempfile::tempdir().unwrap();
    let (sidecar, source_sha) = export_sidecar(base.path());
    let mut manifest = gold_manifest_for(&sidecar, &source_sha);
    let prediction_bytes = std::fs::read(base.path().join("prediction.json")).unwrap();
    manifest["cases"][0]["prediction_sha256"] = json!(digest(&prediction_bytes));
    std::fs::write(
        base.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let dataset = v2::Dataset::load(base.path().join("manifest.json").as_path()).unwrap();
    let report = v2::score(&dataset, ocr_eval::Split::Test).unwrap();
    assert!(report.metrics.fields_gold >= 3);
}

#[test]
fn tampered_sidecar_quote_is_rejected_by_the_loader() {
    let base = tempfile::tempdir().unwrap();
    let (mut sidecar, source_sha) = export_sidecar(base.path());
    sidecar["observations"][0]["visible_text"] = json!("FORGED TEXT");
    std::fs::write(
        base.path().join("prediction.json"),
        serde_json::to_vec(&sidecar).unwrap(),
    )
    .unwrap();
    let mut manifest = gold_manifest_for(&sidecar, &source_sha);
    let prediction_bytes = std::fs::read(base.path().join("prediction.json")).unwrap();
    manifest["cases"][0]["prediction_sha256"] = json!(digest(&prediction_bytes));
    std::fs::write(
        base.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let dataset = v2::Dataset::load(base.path().join("manifest.json").as_path())
        .expect("gold manifest itself must stay valid");
    let error = match v2::score(&dataset, ocr_eval::Split::Test) {
        Ok(_) => panic!("scorer must reject a forged machine observation"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("evidence hash/UTF-8 byte span mismatch"),
        "unexpected rejection: {error}"
    );
}

#[test]
fn export_is_deterministic() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (first_sidecar, _) = export_sidecar(first.path());
    let (second_sidecar, _) = export_sidecar(second.path());
    assert_eq!(first_sidecar, second_sidecar);
}

#[test]
fn page_without_dimensions_abstains_explicitly() {
    let base = tempfile::tempdir().unwrap();
    let extraction_path = base.path().join("extraction.json");
    let result_path = base.path().join("result.json");
    std::fs::write(
        &extraction_path,
        serde_json::to_vec(&extraction_fixture()).unwrap(),
    )
    .unwrap();
    let mut result = result_fixture();
    result["pages"][0].as_object_mut().unwrap().remove("width");
    std::fs::write(&result_path, serde_json::to_vec(&result).unwrap()).unwrap();
    let error = v2_export::export(
        &extraction_path,
        &result_path,
        &"a".repeat(64),
        &base.path().join("prediction.json"),
    )
    .err();
    let error = error.expect("loader must reject a forged machine observation");
    assert!(
        error
            .to_string()
            .contains("no page in the OCR result carries usable dimensions"),
        "unexpected error: {error}"
    );
}
