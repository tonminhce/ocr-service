mod support;
use ocr_core::review::Correction;
use ocr_eval::{Dataset, Split, load_business, read_bounded, score};

#[test]
fn per_field_metrics_separate_state_values_conflicts_roles_and_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = support::fixture_files(directory.path());
    let dataset = Dataset::load(&support::save(directory.path(), &manifest)).unwrap();
    let report = score(&dataset, Split::Test).unwrap();
    assert!(report.synthetic && !report.machine_auto_acceptance_enabled);
    assert_eq!(report.documents, 1);
    assert_eq!(report.labelled_fields, 4);
    assert_eq!(report.fields["landlord.birth_date"].precision, Some(1.0));
    assert_eq!(report.fields["monthly_rent"].state_matches, 1);
    assert_eq!(report.fields["monthly_rent"].precision, Some(0.0));
    assert_eq!(report.fields["monthly_rent"].recall, Some(0.0));
    assert_eq!(
        report.fields["tenant.birth_date"].conflict_state_recall,
        Some(1.0)
    );
    assert_eq!(
        report.fields["tenant.birth_date"].conflict_exact_recall,
        Some(1.0)
    );
    assert_eq!(
        report.fields["tenant.birth_date"].manual_evidence_exact_matches,
        1
    );
    assert_eq!(report.fields["landlord.birth_date"].evidence_available, 1);
    assert_eq!(
        report.fields["landlord.birth_date"].manual_evidence_labels,
        0
    );
    assert_eq!(
        report.fields["deposit_amount"].absent_fields_with_candidates,
        1
    );
    assert_eq!(
        report.fields["deposit_amount"].unexpected_candidate_values,
        1
    );
    assert_eq!((report.role_labels, report.role_matches), (1, 1));
    let report = serde_json::to_string(&report).unwrap();
    assert!(!report.contains("1980-01-01") && !report.contains("1200000"));
}
#[test]
fn human_correction_does_not_inflate_machine_score() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = support::fixture_files(directory.path());
    let mut business = support::business();
    let field = business.fields.get_mut("monthly_rent").unwrap();
    field.value = Some(support::money("1000000", Some("month")));
    field.correction = Some(Correction {
        value: field.value.clone().unwrap(),
        evidence_refs: field.evidence_refs.clone(),
    });
    field.approval = "human_approved".into();
    business.review.status = "approved".into();
    business.review.revision = 1;
    business.review.principal = Some("fixture-reviewer".into());
    std::fs::write(
        directory.path().join("business.json"),
        serde_json::to_vec(&business).unwrap(),
    )
    .unwrap();
    let dataset = Dataset::load(&support::save(directory.path(), &manifest)).unwrap();
    let report = score(&dataset, Split::Test).unwrap();
    let rent = &report.fields["monthly_rent"];
    assert_eq!(rent.precision, Some(0.0));
    assert_eq!(rent.human_approval_coverage, Some(1.0));
    assert_eq!(rent.human_approved_error_rate, Some(0.0));
}
#[test]
fn wrong_conflict_candidates_do_not_count_as_exact_conflict_recall() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = support::fixture_files(directory.path());
    let mut business = support::business();
    business
        .fields
        .get_mut("tenant.birth_date")
        .unwrap()
        .candidates[1]
        .value = support::date("2002-04-04");
    std::fs::write(
        directory.path().join("business.json"),
        serde_json::to_vec(&business).unwrap(),
    )
    .unwrap();
    let report = score(
        &Dataset::load(&support::save(directory.path(), &manifest)).unwrap(),
        Split::Test,
    )
    .unwrap();
    let conflict = &report.fields["tenant.birth_date"];
    assert_eq!(conflict.conflict_state_recall, Some(1.0));
    assert_eq!(conflict.conflict_exact_recall, Some(0.0));
    assert_eq!(conflict.unexpected_candidate_values, 1);
}
#[test]
fn duplicate_hashes_and_group_split_leakage_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[1].source_sha256 = manifest.documents[0].source_sha256.clone();
    assert!(
        Dataset::load(&support::save(directory.path(), &manifest))
            .unwrap_err_message()
            .contains("duplicate source hash")
    );
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[1].group_id = manifest.documents[0].group_id.clone();
    assert!(
        Dataset::load(&support::save(directory.path(), &manifest))
            .unwrap_err_message()
            .contains("group crosses")
    );
}
#[test]
fn traversal_hash_mismatch_empty_and_invalid_split_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[0].result_path = Some("../outside.json".into());
    assert!(Dataset::load(&support::save(directory.path(), &manifest)).is_err());
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[0].source_sha256 = "0".repeat(64);
    assert!(Dataset::load(&support::save(directory.path(), &manifest)).is_err());
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents.clear();
    assert!(Dataset::load(&support::save(directory.path(), &manifest)).is_err());
    assert!(Split::parse("training").is_err());
    let path = directory.path().join("bad.json");
    std::fs::write(&path, br#"{"schema_version":"ocr-eval-manifest/1.0.0","dataset_name":"bad","synthetic":true,"documents":[{"split":"train"}]}"#).unwrap();
    assert!(Dataset::load(&path).is_err());
}
#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("source"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path().join("source"), directory.path().join("link"))
        .unwrap();
    assert!(ocr_eval::safe_resolve(directory.path(), "link").is_err());
}
#[cfg(unix)]
#[test]
fn fifo_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fifo");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a live, NUL-terminated local path; mode is permission bits.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_bounded(&path, 1024).is_err());
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("FIFO input must not block waiting for a writer")
    );
}
#[test]
fn malformed_evidence_and_uncalibrated_numeric_confidence_are_rejected() {
    let mut business = support::business();
    business
        .fields
        .get_mut("monthly_rent")
        .unwrap()
        .evidence_refs
        .push("unknown".into());
    assert!(load_business(&serde_json::to_vec(&business).unwrap()).is_err());
    let mut business = support::business();
    business.fields.get_mut("monthly_rent").unwrap().confidence = Some(0.99);
    assert!(load_business(&serde_json::to_vec(&business).unwrap()).is_err());
    let mut business = support::business();
    business.evidence.values_mut().next().unwrap().byte_end += 1;
    assert!(load_business(&serde_json::to_vec(&business).unwrap()).is_err());
}
#[test]
fn file_budget_and_inconsistent_labels_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[2]
        .labels
        .get_mut("tenant.birth_date")
        .unwrap()
        .values
        .clear();
    assert!(Dataset::load(&support::save(directory.path(), &manifest)).is_err());
    let path = directory.path().join("bytes");
    std::fs::write(&path, b"123456789").unwrap();
    assert!(read_bounded(&path, 8).is_err());
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents.retain(|d| d.split == Split::Dev);
    let dataset = Dataset::load(&support::save(directory.path(), &manifest)).unwrap();
    assert!(score(&dataset, Split::Test).is_err());
}
#[test]
fn prediction_artifact_and_embedded_source_hashes_are_verified() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = support::fixture_files(directory.path());
    let path = support::save(directory.path(), &manifest);
    std::fs::write(directory.path().join("business.json"), b"{}").unwrap();
    let dataset = Dataset::load(&path).unwrap();
    assert!(
        score(&dataset, Split::Test)
            .unwrap_err_message()
            .contains("artifact hash")
    );

    let mut business = support::business();
    business.source_sha256 = Some("0".repeat(64));
    std::fs::write(
        directory.path().join("business.json"),
        serde_json::to_vec(&business).unwrap(),
    )
    .unwrap();
    let dataset = Dataset::load(&support::save(directory.path(), &manifest)).unwrap();
    assert!(
        score(&dataset, Split::Test)
            .unwrap_err_message()
            .contains("source hash")
    );
}
#[test]
fn evidence_structure_does_not_imply_manual_correctness_and_empty_metrics_stay_null() {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = support::fixture_files(directory.path());
    manifest.documents[2]
        .labels
        .get_mut("tenant.birth_date")
        .unwrap()
        .evidence_gold
        .as_mut()
        .unwrap()[0]
        .quote = "a different independently labelled source quote".into();
    let mut business = support::business();
    business.fields.remove("deposit_amount");
    business
        .fields
        .get_mut("landlord.birth_date")
        .unwrap()
        .value = Some(support::date("1980-01-01"));
    std::fs::write(
        directory.path().join("business.json"),
        serde_json::to_vec(&business).unwrap(),
    )
    .unwrap();
    let report = score(
        &Dataset::load(&support::save(directory.path(), &manifest)).unwrap(),
        Split::Test,
    )
    .unwrap();
    assert_eq!(report.fields["tenant.birth_date"].evidence_available, 1);
    assert_eq!(
        report.fields["tenant.birth_date"].manual_evidence_exact_matches,
        0
    );
    assert_eq!(report.fields["deposit_amount"].exact_matches, 1);
    assert_eq!(report.fields["deposit_amount"].precision, None);
    assert_eq!(report.fields["deposit_amount"].recall, None);
}

#[test]
fn cli_manifest_decoding_errors_do_not_expose_private_values_or_field_names() {
    let sentinel = "synthetic-private-value-never-log";
    for mutation in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let manifest = support::fixture_files(directory.path());
        let mut value = serde_json::to_value(&manifest).unwrap();
        match mutation {
            0 => value["documents"][0]["split"] = serde_json::json!(sentinel),
            1 => value[sentinel] = serde_json::json!(true),
            _ => {
                value["documents"][2]["labels"]["monthly_rent"]["state"] =
                    serde_json::json!(sentinel);
            }
        }
        let path = directory.path().join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-eval"))
            .args(["score", path.to_str().unwrap(), "--split", "test"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(stderr.contains("manifest JSON is invalid"));
        assert!(!stderr.contains(sentinel));
        assert!(!stderr.contains("Caused by:"));
    }
}

#[test]
fn cli_and_library_result_decoding_errors_discard_private_serde_sources() {
    let sentinel = "synthetic-private-value-never-log";
    let mut business = serde_json::to_value(support::business()).unwrap();
    business["fields"]["monthly_rent"]["state"] = serde_json::json!(sentinel);
    let extraction = serde_json::json!({
        "schema_version": ocr_extraction::SCHEMA_VERSION,
        "fields": {"monthly_rent": {"state": sentinel}}
    });
    let extraction_error = "expected extraction, OCR result, API job or business result";
    let payloads = [
        (
            format!(r#"{{"{sentinel}":"\q"}}"#).into_bytes(),
            "result JSON is invalid",
        ),
        (
            serde_json::to_vec(&business).unwrap(),
            "business result is malformed",
        ),
        (serde_json::to_vec(&extraction).unwrap(), extraction_error),
        (
            serde_json::to_vec(&serde_json::json!({"extraction": extraction})).unwrap(),
            extraction_error,
        ),
        (
            serde_json::to_vec(&serde_json::json!({"result": {"extraction": extraction}})).unwrap(),
            extraction_error,
        ),
    ];
    for (payload, expected_message) in payloads {
        let error = load_business(&payload).unwrap_err();
        assert_eq!(error.to_string(), expected_message);
        assert_eq!(error.chain().count(), 1);
        assert!(!format!("{error:?}").contains(sentinel));
        let directory = tempfile::tempdir().unwrap();
        let manifest = support::fixture_files(directory.path());
        std::fs::write(directory.path().join("business.json"), &payload).unwrap();
        let path = support::save(directory.path(), &manifest);
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-eval"))
            .args(["score", path.to_str().unwrap(), "--split", "test"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(stderr.contains(expected_message));
        assert!(!stderr.contains(sentinel));
        assert!(!stderr.contains("Caused by:"));
    }
}

trait ErrorMessage {
    fn unwrap_err_message(self) -> String;
}
impl<T> ErrorMessage for anyhow::Result<T> {
    fn unwrap_err_message(self) -> String {
        match self {
            Ok(_) => panic!("expected failure"),
            Err(error) => error.to_string(),
        }
    }
}
