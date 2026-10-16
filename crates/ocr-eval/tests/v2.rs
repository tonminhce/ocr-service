use ocr_eval::{Split, digest, v2};
use serde_json::{Value, json};
#[path = "support/v2_fixture.rs"]
mod fixture;
use fixture::{fixture, refresh_evidence, report, save};

#[test]
fn independent_ids_match_once_and_report_contains_only_aggregate_metrics() {
    let (mut m, p) = fixture();
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.all_critical_accuracy, Some(1.0));
    assert_eq!(r.metrics.entities_exact, 5);
    assert_eq!(r.metrics.field_grounded_exact, 4);
    assert_eq!(r.metrics.conflict_complete, 1);
    assert_eq!(r.metrics.character_error_rate, Some(0.0));
    assert_eq!(r.metrics.geometry_mean_coverage, Some(1.0));
    assert!(!r.machine_auto_acceptance_enabled);
    let json = serde_json::to_string(&r).unwrap();
    for value in [
        "Nguyễn",
        "1990-02-15",
        "case-1",
        "gold-a",
        "pred-gold-a",
        "source.txt",
    ] {
        assert!(
            !json.contains(value),
            "report leaked private annotation identifier or value"
        );
    }
}
#[test]
fn correct_values_swapped_between_people_do_not_get_rematched_per_field() {
    let (mut m, mut p) = fixture();
    for key in ["visible_text", "value"] {
        let a = p["observations"][0][key].clone();
        p["observations"][0][key] = p["observations"][1][key].clone();
        p["observations"][1][key] = a;
    }
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.entities_exact, 5);
    assert_eq!(r.metrics.field_value_state_matches, 2);
    assert_eq!(r.metrics.field_grounded_exact, 2);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
}
#[test]
fn correct_value_wrong_page_is_not_grounded() {
    let (mut m, mut p) = fixture();
    p["observations"][0]["anchor"]["page_number"] = json!(2);
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.field_value_state_matches, 4);
    assert_eq!(r.metrics.field_grounded_exact, 3);
    assert_eq!(r.metrics.candidates_missing, 1);
    assert_eq!(r.metrics.candidates_extra, 1);
    assert_eq!(r.metrics.cer_unavailable_pairs, 1);
}
#[test]
fn correct_value_wrong_card_or_party_reference_is_rejected() {
    let (mut m, mut p) = fixture();
    p["observations"][3]["card_id"] = json!("pred-gold-card-a");
    assert!(report(&mut m, &p).is_err());
}
#[test]
fn missing_conflict_candidate_and_singleton_recovery_remain_ambiguous() {
    let (mut m, mut p) = fixture();
    p["observations"].as_array_mut().unwrap().remove(3);
    p["fields"][2]["observation_ids"] = json!(["pred-obs-b-contract"]);
    p["fields"][2]["state"] = json!("ambiguous");
    p["fields"][2]["ambiguity_reason"] = json!("recovery_only");
    p["observations"][2]["origin"] = json!("recovery");
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.conflict_complete, 0);
    assert_eq!(r.metrics.candidates_missing, 1);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
}
#[test]
fn gold_singleton_ambiguity_is_valid_but_does_not_become_found() {
    let (mut m, mut p) = fixture();
    m["cases"][0]["observations"]
        .as_array_mut()
        .unwrap()
        .remove(3);
    m["cases"][0]["fields"][2]["target"]["observation_ids"] = json!(["obs-b-contract"]);
    m["cases"][0]["fields"][2]["target"]["state"] = json!("ambiguous");
    m["cases"][0]["fields"][2]["target"]["ambiguity_reason"] = json!("recovery_only");
    p["observations"].as_array_mut().unwrap().remove(3);
    p["fields"][2]["observation_ids"] = json!(["pred-obs-b-contract"]);
    p["fields"][2]["state"] = json!("ambiguous");
    p["fields"][2]["ambiguity_reason"] = json!("recovery_only");
    p["observations"][2]["origin"] = json!("recovery");
    refresh_evidence(&mut p);
    assert_eq!(
        report(&mut m, &p).unwrap().metrics.all_critical_accuracy,
        Some(1.0)
    );
    p["fields"][2]["state"] = json!("found");
    p["fields"][2]["ambiguity_reason"] = Value::Null;
    p["observations"][2]["origin"] = json!("first_pass");
    assert_eq!(
        report(&mut m, &p).unwrap().metrics.all_critical_accuracy,
        Some(0.0)
    );
}
#[test]
fn duplicate_reference_is_rejected_and_duplicate_observation_is_penalized() {
    let (mut m, mut p) = fixture();
    p["fields"][0]["observation_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("pred-obs-a-name"));
    assert!(report(&mut m, &p).is_err());
    p["fields"][0]["observation_ids"] = json!(["pred-obs-a-name", "duplicate-obs"]);
    let mut duplicate = p["observations"][0].clone();
    duplicate["id"] = json!("duplicate-obs");
    p["observations"].as_array_mut().unwrap().push(duplicate);
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.field_grounded_exact, 3);
    assert_eq!(r.metrics.candidates_extra, 2);
    assert_eq!(r.metrics.candidates_missing, 1);
}
#[test]
fn missing_and_source_blank_are_different() {
    let (mut m, mut p) = fixture();
    p["observations"].as_array_mut().unwrap().remove(4);
    p["fields"][3]["observation_ids"] = json!([]);
    p["fields"][3]["state"] = json!("missing");
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.field_state_matches, 3);
    assert_eq!(r.metrics.field_grounded_exact, 3);
}
#[test]
fn forged_hash_quote_or_multibyte_span_cannot_pass_structural_integrity() {
    for mutation in [0, 1, 2] {
        let (mut m, mut p) = fixture();
        match mutation {
            0 => p["observations"][0]["evidence"]["quote_sha256"] = json!("0".repeat(64)),
            1 => p["transcripts"][0]["text"] = json!("forged transcript"),
            _ => p["observations"][0]["evidence"]["byte_end"] = json!(3),
        }
        assert!(report(&mut m, &p).is_err());
    }
}
#[test]
fn structurally_valid_wrong_transcript_is_semantic_error_not_pixel_attestation() {
    let (mut m, mut p) = fixture();
    p["observations"][0]["visible_text"] = json!("Nguyên A");
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.field_value_state_matches, 4);
    assert_eq!(r.metrics.field_grounded_exact, 3);
    assert_eq!(r.metrics.cer_edit_distance, 1);
    assert!(r.metrics.character_error_rate.unwrap() > 0.0);
}
#[test]
fn rotation_and_cropbox_mismatch_disable_source_geometry_matching() {
    for key in ["rotation_degrees", "crop_box_pdf"] {
        let (mut m, mut p) = fixture();
        p["pages"][0][key] = if key == "rotation_degrees" {
            json!(90)
        } else {
            json!([0.0, 0.0, 499.0, 500.0])
        };
        let r = report(&mut m, &p).unwrap();
        assert_eq!(r.metrics.page_transform_mismatches, 1);
        assert_eq!(r.metrics.all_critical_cases_correct, 0);
    }
}
#[test]
fn whole_page_or_clipped_boxes_cannot_receive_precise_field_grounding() {
    for polygon in [
        json!([[0, 0], [1000, 0], [1000, 1000], [0, 1000]]),
        json!([[20, 50], [120, 50], [120, 80], [20, 80]]),
    ] {
        let (mut m, mut p) = fixture();
        p["observations"][0]["anchor"]["polygon"] = polygon;
        refresh_evidence(&mut p);
        let r = report(&mut m, &p).unwrap();
        assert_eq!(r.metrics.field_value_state_matches, 4);
        assert_eq!(r.metrics.field_grounded_exact, 3);
        assert_eq!(r.metrics.all_critical_cases_correct, 0);
    }
}
#[test]
fn entity_roles_extras_and_ambiguous_matching_have_explicit_penalties() {
    let (mut m, mut p) = fixture();
    p["entities"][1]["role"] = json!("tenant");
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.roles_exact, 1);
    assert_eq!(r.metrics.entities_exact, 4);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
    let (mut m, mut p) = fixture();
    let mut extra = p["entities"][4].clone();
    extra["id"] = json!("extra-card");
    extra["anchor"]["polygon"] = json!([[510, 510], [990, 510], [990, 990], [510, 990]]);
    p["entities"].as_array_mut().unwrap().push(extra);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.entities_extra, 1);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
}
#[test]
fn gold_schema_rejects_bad_ids_references_geometry_states_and_annotation_provenance() {
    for mutation in 0..8 {
        let (mut m, p) = fixture();
        let c = &mut m["cases"][0];
        match mutation {
            0 => c["entities"][1]["id"] = c["entities"][0]["id"].clone(),
            1 => c["observations"][0]["document_id"] = json!("dangling"),
            2 => c["observations"][0]["anchor"]["polygon"][0][0] = json!(-1),
            3 => c["observations"][0]["anchor"]["page_number"] = json!(3),
            4 => c["fields"][0]["annotation"]["annotator_ids"] = json!(["labeler-1"]),
            5 => c["fields"][0]["annotation"]["prediction_hidden"] = json!(false),
            6 => c["fields"][3]["target"]["state"] = json!("missing"),
            _ => {
                c["observations"][0]["anchor"]["polygon"] =
                    json!([[20, 50], [200, 80], [200, 50], [20, 80]])
            }
        }
        assert!(report(&mut m, &p).is_err(), "mutation {mutation}");
    }
}
#[test]
fn normalization_keeps_accents_identifier_zeros_currency_period_and_units() {
    use ocr_extraction::FieldValue;
    let a = FieldValue::Text {
        value: "Nguyễn".into(),
    };
    let b = FieldValue::Text {
        value: "Nguyễn".into(),
    };
    assert_eq!(
        v2::normalized_value(&a).unwrap(),
        v2::normalized_value(&b).unwrap()
    );
    assert_ne!(
        v2::normalized_value(&b).unwrap(),
        v2::normalized_value(&FieldValue::Text {
            value: "Nguyen".into()
        })
        .unwrap()
    );
    assert_ne!(
        v2::normalized_value(&FieldValue::Identifier {
            value: "00123".into()
        })
        .unwrap(),
        v2::normalized_value(&FieldValue::Identifier {
            value: "123".into()
        })
        .unwrap()
    );
    let money = |period| FieldValue::Money {
        amount: "100.00".into(),
        currency: "VND".into(),
        period,
    };
    assert_ne!(
        v2::normalized_value(&money(None)).unwrap(),
        v2::normalized_value(&money(Some("month".into()))).unwrap()
    );
    assert!(
        v2::normalized_value(&FieldValue::Area {
            amount: "1".into(),
            unit: "m3".into()
        })
        .is_err()
    );
}
#[test]
fn group_and_unseen_template_leakage_are_rejected_but_representative_family_can_recur() {
    let (mut m, p) = fixture();
    let mut second = m["cases"][0].clone();
    second["id"] = json!("case-2");
    second["split"] = json!("dev");
    second["source_sha256"] = json!(digest(b"second synthetic source"));
    second["source_path"] = json!("source-2.txt");
    m["cases"].as_array_mut().unwrap().push(second);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source-2.txt"), b"second synthetic source").unwrap();
    let path = save(dir.path(), &mut m, &p);
    assert!(v2::Dataset::load(&path).is_err());
    m["cases"][1]["group_id"] = json!("group-2");
    let path = save(dir.path(), &mut m, &p);
    assert!(v2::Dataset::load(&path).is_ok());
    m["cases"][0]["cohort"] = json!("unseen_template");
    let path = save(dir.path(), &mut m, &p);
    assert!(v2::Dataset::load(&path).is_err());
}

#[test]
fn right_value_associated_to_another_valid_card_is_a_candidate_error() {
    let (mut m, mut p) = fixture();
    // Two distinct cards for the same tenant: wrong-card attribution is a valid
    // graph and must be penalized semantically rather than rejected as dangling.
    m["cases"][0]["entities"][3]["party_id"] = json!("gold-b");
    p["entities"][3]["party_id"] = json!("pred-gold-b");
    p["observations"][3]["card_id"] = json!("pred-gold-card-a");
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.field_value_state_matches, 4);
    assert_eq!(r.metrics.field_grounded_exact, 3);
    assert_eq!(r.metrics.candidates_missing, 1);
    assert_eq!(r.metrics.candidates_extra, 1);
}
#[test]
fn cer_long_pairs_and_empty_reference_report_unavailability_without_fake_zero() {
    let (mut m, mut p) = fixture();
    let text = "a".repeat(1025);
    m["cases"][0]["observations"][0]["visible_text"] = json!(text);
    p["observations"][0]["visible_text"] = json!(text);
    refresh_evidence(&mut p);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.cer_unavailable_pairs, 1);
    assert_eq!(r.metrics.cer_pairs, 4);
    assert_eq!(r.metrics.cer_reference_characters, 26);
    assert_eq!(r.metrics.all_critical_cases_correct, 1);
}
#[test]
fn score_v2_cli_runs_explicitly_and_v1_loader_rejects_v2() {
    let (mut m, p) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = save(dir.path(), &mut m, &p);
    assert!(ocr_eval::Dataset::load(&path).is_err());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-eval"))
        .args(["score-v2", path.to_str().unwrap(), "--split", "test"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], "ocr-eval-report/2.0.0");
    assert_eq!(result["metrics"]["all_critical_cases_correct"], 1);
}
#[test]
fn source_and_prediction_hashes_are_verified_again_at_scoring_time() {
    let (mut m, p) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = save(dir.path(), &mut m, &p);
    let data = v2::Dataset::load(&path).unwrap();
    std::fs::write(dir.path().join("prediction.json"), b"{}").unwrap();
    assert!(v2::score(&data, Split::Test).is_err());
    let path = save(dir.path(), &mut m, &p);
    let data = v2::Dataset::load(&path).unwrap();
    std::fs::write(dir.path().join("source.txt"), b"changed source").unwrap();
    assert!(v2::score(&data, Split::Test).is_err());
}

#[test]
fn empty_substring_cannot_attest_a_source_blank_and_unknown_value_properties_are_rejected() {
    let (mut m, mut p) = fixture();
    p["transcripts"][4]["text"] = json!("nonblank");
    assert!(report(&mut m, &p).is_err());
    let (mut m, mut p) = fixture();
    p["observations"][0]["value"]["confidence"] = json!(0.99);
    assert!(report(&mut m, &p).is_err());
}
#[test]
fn malformed_private_enum_does_not_leak_its_value_through_cli_error_chain() {
    let (mut m, mut p) = fixture();
    p["entities"][1]["role"] = json!("sensitive-value-never-print");
    let dir = tempfile::tempdir().unwrap();
    let path = save(dir.path(), &mut m, &p);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-eval"))
        .args(["score-v2", path.to_str().unwrap(), "--split", "test"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("sensitive-value-never-print"));
    assert!(output.stdout.is_empty());
}

#[test]
fn matching_is_order_independent_and_geometry_ties_abstain() {
    let (mut m, mut p) = fixture();
    p["entities"].as_array_mut().unwrap().reverse();
    p["observations"].as_array_mut().unwrap().reverse();
    p["fields"].as_array_mut().unwrap().reverse();
    assert_eq!(
        report(&mut m, &p)
            .unwrap()
            .metrics
            .all_critical_cases_correct,
        1
    );
    let (mut m, mut p) = fixture();
    let mut extra = p["entities"][1].clone();
    extra["id"] = json!("same-anchor-extra-party");
    p["entities"].as_array_mut().unwrap().push(extra);
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.entities_missing, 1);
    assert_eq!(r.metrics.entities_extra, 2);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
}

#[test]
fn ambiguity_reason_is_part_of_semantic_state_and_grounded_exact() {
    let (mut m, mut p) = fixture();
    m["cases"][0]["fields"][2]["target"]["state"] = json!("ambiguous");
    m["cases"][0]["fields"][2]["target"]["ambiguity_reason"] = json!("uncertain_source");
    p["fields"][2]["state"] = json!("ambiguous");
    p["fields"][2]["ambiguity_reason"] = json!("recovery_only");
    p["observations"][2]["origin"] = json!("recovery");
    p["observations"][3]["origin"] = json!("recovery");
    let r = report(&mut m, &p).unwrap();
    assert_eq!(r.metrics.candidates_exact, 5);
    assert_eq!(r.metrics.field_state_matches, 3);
    assert_eq!(r.metrics.field_value_state_matches, 3);
    assert_eq!(r.metrics.field_grounded_exact, 3);
    assert_eq!(r.metrics.all_critical_cases_correct, 0);
}
#[test]
fn multi_candidate_recovery_only_cannot_include_first_pass_observations() {
    let (mut m, mut p) = fixture();
    p["fields"][2]["state"] = json!("ambiguous");
    p["fields"][2]["ambiguity_reason"] = json!("recovery_only");
    assert!(report(&mut m, &p).is_err());
    p["observations"][2]["origin"] = json!("recovery");
    assert!(report(&mut m, &p).is_err());
    p["observations"][3]["origin"] = json!("recovery");
    assert!(report(&mut m, &p).is_ok());
}

#[test]
fn found_predictions_reject_one_or_multiple_recovery_only_observations() {
    for observation_count in [1, 2] {
        let (mut manifest, mut prediction) = fixture();
        let gold_observations = manifest["cases"][0]["observations"].clone();
        let gold_fields = manifest["cases"][0]["fields"].clone();
        prediction["observations"][0]["origin"] = json!("recovery");
        if observation_count == 2 {
            let mut recovery = prediction["observations"][0].clone();
            recovery["id"] = json!("pred-obs-a-name-second");
            recovery["anchor"]["polygon"] = json!([[20, 300], [200, 300], [200, 330], [20, 330]]);
            prediction["observations"]
                .as_array_mut()
                .unwrap()
                .push(recovery);
            prediction["fields"][0]["observation_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!("pred-obs-a-name-second"));
        }
        refresh_evidence(&mut prediction);
        let error = report(&mut manifest, &prediction).unwrap_err();
        assert_eq!(
            error.to_string(),
            "found prediction requires first-pass support"
        );
        assert_eq!(manifest["cases"][0]["observations"], gold_observations);
        assert_eq!(manifest["cases"][0]["fields"], gold_fields);
    }
}

#[test]
fn found_prediction_with_first_pass_and_recovery_support_preserves_grounding() {
    let (mut manifest, mut prediction) = fixture();
    let mut gold = manifest["cases"][0]["observations"][0].clone();
    gold["id"] = json!("obs-a-name-second");
    gold["anchor"]["polygon"] = json!([[20, 300], [200, 300], [200, 330], [20, 330]]);
    manifest["cases"][0]["observations"]
        .as_array_mut()
        .unwrap()
        .push(gold);
    manifest["cases"][0]["fields"][0]["target"]["observation_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("obs-a-name-second"));
    let mut recovery = prediction["observations"][0].clone();
    recovery["id"] = json!("pred-obs-a-name-second");
    recovery["origin"] = json!("recovery");
    recovery["anchor"]["polygon"] = json!([[20, 300], [200, 300], [200, 330], [20, 330]]);
    prediction["observations"]
        .as_array_mut()
        .unwrap()
        .push(recovery);
    prediction["fields"][0]["observation_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("pred-obs-a-name-second"));
    refresh_evidence(&mut prediction);
    let result = report(&mut manifest, &prediction).unwrap();
    assert_eq!(result.metrics.all_critical_accuracy, Some(1.0));
    assert_eq!(result.metrics.field_grounded_exact, 4);
    assert_eq!(result.metrics.candidates_exact, 6);
    assert!(
        manifest["cases"][0]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation["origin"] == "human_gold")
    );
}
