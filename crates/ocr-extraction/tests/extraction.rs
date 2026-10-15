use ocr_extraction::{Decision, FieldState, FieldValue, PartyRole, SourceBlock, extract};
fn block(id: &str, page: u32, text: &str) -> SourceBlock {
    SourceBlock {
        id: id.into(),
        page_number: page,
        region_index: Some(7),
        source: "ocr".into(),
        text: text.into(),
        bbox_2d: Some([10, 20, 900, 300]),
        detector_score: Some(0.9),
    }
}
fn contract() -> Vec<SourceBlock> {
    vec![
        block("title", 1, "HỢP ĐỒNG THUÊ NHÀ"),
        block(
            "owner",
            1,
            "## BÊN CHO THUÊ (BÊN A)\nHọ và tên: **Lê Mẫu Một**\nCCCD/CMND số: 012345678901\nNgày sinh: 02/03/1980\nNgày cấp / Nơi cấp: 05/06/2020 Cơ quan mẫu\nĐịa chỉ thường trú: 10 Đường Mẫu, Phường Ví Dụ\nĐiện thoại / Email: 0901234567 / sample@example.invalid\nTài khoản nhận tiền: 0012345678",
        ),
        block(
            "tenant",
            1,
            "## BÊN THUÊ (BÊN B)\nHọ và tên: Đỗ Mẫu Hai\nSố giấy tờ: 023456789012\nNgày sinh / Giới tính: 07/08/1990 / Nam\nNgày cấp / Nơi cấp: 09/10/2021 Nơi mẫu\nĐịa chỉ thường trú: 20 Phố Giả, Thành phố Mẫu",
        ),
        block(
            "terms",
            2,
            "## ĐIỀU 1. THANH TOÁN\nGiá thuê: 10.000.000 VND/tháng\nTiền cọc: 20000000 VND\nĐịa chỉ Nhà thuê: 42 Phố Mẫu\nDiện tích sử dụng: 60 m²",
        ),
    ]
}
fn value(v: &FieldValue) -> &str {
    match v {
        FieldValue::Text { value }
        | FieldValue::Identifier { value }
        | FieldValue::Date { value } => value,
        _ => panic!("unexpected type"),
    }
}
#[test]
fn extracts_scoped_fields_without_fake_confidence() {
    let blocks = contract();
    let r = extract(&blocks);
    assert!(r.supported_template);
    assert_eq!(r.decision, Decision::NeedsReview);
    assert_eq!(r.parties.len(), 2);
    let landlord = r
        .parties
        .iter()
        .find(|p| p.role == PartyRole::Landlord)
        .unwrap();
    assert_eq!(landlord.designator, "A");
    assert_eq!(
        value(landlord.fields["id_number"].selected.as_ref().unwrap()),
        "012345678901"
    );
    assert_eq!(
        value(landlord.fields["bank_account"].selected.as_ref().unwrap()),
        "0012345678"
    );
    assert_eq!(
        value(landlord.fields["birth_date"].selected.as_ref().unwrap()),
        "1980-03-02"
    );
    assert_eq!(
        r.fields["monthly_rent"].selected,
        Some(FieldValue::Money {
            amount: "10000000".into(),
            currency: "VND".into(),
            period: Some("month".into())
        })
    );
    assert_eq!(
        r.fields["deposit_amount"].selected,
        Some(FieldValue::Money {
            amount: "20000000".into(),
            currency: "VND".into(),
            period: None
        })
    );
    assert_eq!(
        r.fields["property_area"].selected,
        Some(FieldValue::Area {
            amount: "60".into(),
            unit: "m2".into()
        })
    );
    for p in &r.parties {
        for f in p.fields.values() {
            assert!(f.confidence.is_none());
            for c in &f.candidates {
                assert!(c.confidence.is_none());
                for e in &c.evidence {
                    let b = blocks.iter().find(|b| b.id == e.block_id).unwrap();
                    assert_eq!(&b.text[e.byte_start..e.byte_end], e.quote);
                    assert_eq!(e.geometry_scope, "source_block");
                }
            }
        }
    }
}
#[test]
fn role_is_explicit_and_can_be_reversed() {
    let mut blocks = contract();
    blocks[1].text = blocks[1]
        .text
        .replace("BÊN CHO THUÊ (BÊN A)", "BÊN CHO THUÊ (BÊN B)");
    blocks[2].text = blocks[2]
        .text
        .replace("BÊN THUÊ (BÊN B)", "BÊN THUÊ (BÊN A)");
    let r = extract(&blocks);
    assert!(r.supported_template);
    assert_eq!(
        r.parties
            .iter()
            .find(|p| p.role == PartyRole::Landlord)
            .unwrap()
            .designator,
        "B"
    );
}
#[test]
fn blank_birthdate_is_unreadable_not_clipped_or_fabricated() {
    let mut blocks = contract();
    blocks[1].text = blocks[1]
        .text
        .replace("Ngày sinh: 02/03/1980", "Ngày sinh:");
    blocks[1].detector_score = Some(0.577);
    let r = extract(&blocks);
    let p = r.parties.iter().find(|p| p.designator == "A").unwrap();
    assert_eq!(
        p.contract_fields["birth_date"].state,
        FieldState::Unreadable
    );
    assert!(p.contract_fields["birth_date"].selected.is_none());
    assert_eq!(
        p.contract_fields["birth_date"].observations[0].quote,
        "Ngày sinh:"
    );
    assert!(
        r.issues
            .iter()
            .any(|i| i.code == "low_layout_detection_score")
    );
    assert!(!serde_json::to_string(&r).unwrap().contains("clipped_date"));
}
#[test]
fn hash_symbols_and_suspect_phrases_are_alerts_not_corrections() {
    let mut blocks = contract();
    blocks.push(block("defects",2,"## CẢN CỨ PHÁP LÝ\nĐặc điểm Nhà thuê: diện tích sử dụng m#; kết cấu: #\nThú cung; bán giao; tạm dùng"));
    let r = extract(&blocks);
    assert_eq!(
        r.issues
            .iter()
            .filter(|i| i.code == "ambiguous_hash_symbol")
            .count(),
        2
    );
    assert_eq!(
        r.issues
            .iter()
            .filter(|i| i.code == "suspect_vietnamese_phrase")
            .count(),
        4
    );
    assert_eq!(r.fields["property_area"].candidates.len(), 1); // existing explicit 60 m²; no # inference
    assert!(
        r.issues
            .iter()
            .any(|i| i.suggested_correction.as_deref() == Some("thú cưng"))
    );
}
fn front(id: &str, number: &str, name: &str, date: &str, address: &str) -> SourceBlock {
    block(
        id,
        4,
        &format!(
            "CĂN CƯỚC CÔNG DÂN\nCitizen Identity Card\nSố / No.: {number}\nHọ và tên / Full name: {name}\nNgày sinh / Date of birth: {date}\nNơi thường trú / Place of residence: {address}"
        ),
    )
}
#[test]
fn cross_source_conflicts_keep_both_observations_without_selecting() {
    let mut blocks = contract();
    blocks.push(front(
        "front",
        "023456789012",
        "Đỗ Mẫu Hai",
        "08/08/1990",
        "99 Phố Khác",
    ));
    blocks.push(block("back",4,"Đặc điểm nhân dạng / Personal identification:\nNgày, tháng, năm / Date, month, year: 10/10/2021\nIDVNM4567890126023456789012<<8"));
    let r = extract(&blocks);
    let p = r.parties.iter().find(|p| p.designator == "B").unwrap();
    assert_eq!(p.linked_identity_documents.len(), 2);
    for key in ["birth_date", "issue_date", "residence_address"] {
        assert_eq!(p.fields[key].state, FieldState::Conflict, "{key}");
        assert!(p.fields[key].selected.is_none());
        assert_eq!(p.fields[key].candidates.len(), 2);
        assert!(p.contract_fields[key].selected.is_some());
        assert!(p.identity_fields[key].selected.is_some());
    }
    assert_eq!(
        r.issues
            .iter()
            .filter(|i| i.code == "cross_source_field_conflict")
            .count(),
        3
    );
}
#[test]
fn names_alone_do_not_link_documents() {
    let mut blocks = contract();
    blocks.push(front(
        "unmatched",
        "034567890123",
        "Đỗ Mẫu Hai",
        "07/08/1990",
        "20 Phố Giả",
    ));
    let r = extract(&blocks);
    assert_eq!(r.identity_documents[0].linkage.state, "unmatched");
    assert!(
        r.parties
            .iter()
            .all(|p| p.linked_identity_documents.is_empty())
    );
}
#[test]
fn repeated_identifier_across_parties_is_ambiguous() {
    let mut blocks = contract();
    blocks[1].text = blocks[1].text.replace("012345678901", "023456789012");
    blocks.push(front(
        "front",
        "023456789012",
        "Đỗ Mẫu Hai",
        "07/08/1990",
        "20 Phố Giả",
    ));
    let r = extract(&blocks);
    assert_eq!(r.identity_documents[0].linkage.state, "ambiguous");
    assert_eq!(r.identity_documents[0].linkage.party_ids.len(), 2);
    assert!(
        r.parties
            .iter()
            .all(|p| p.linked_identity_documents.is_empty())
    );
}
#[test]
fn invalid_calendar_date_and_implicit_currency_or_period_are_rejected() {
    let mut blocks = contract();
    blocks[1].text = blocks[1].text.replace("02/03/1980", "31/02/1980");
    blocks[3].text = "ĐIỀU 1\nGiá thuê: 10000000\nTiền cọc: 20000000\nDiện tích: m#".into();
    let r = extract(&blocks);
    assert_eq!(
        r.parties[0].contract_fields["birth_date"].state,
        FieldState::Unreadable
    );
    assert_eq!(r.fields["monthly_rent"].state, FieldState::Unreadable);
    assert_eq!(r.fields["deposit_amount"].state, FieldState::Unreadable);
    blocks[3].text = "Giá thuê: 10000000 VND".into();
    assert_eq!(
        extract(&blocks).fields["monthly_rent"].state,
        FieldState::Unreadable
    );
}
#[test]
fn unknown_document_does_not_gain_party_fields_from_unscoped_anchors() {
    let r = extract(&[block(
        "random",
        1,
        "Họ và tên: Ai Đó\nSố giấy tờ: 012345678901",
    )]);
    assert!(!r.supported_template);
    assert!(r.parties.is_empty());
    assert!(
        r.issues
            .iter()
            .any(|i| i.code == "unsupported_or_incomplete_template")
    );
}
#[test]
fn duplicated_source_ids_and_zero_pages_fail_closed() {
    let mut a = contract();
    a[1].id = a[0].id.clone();
    let r = extract(&a);
    assert!(r.parties.is_empty());
    assert!(r.issues.iter().any(|i| i.code == "invalid_source_blocks"));
    a = contract();
    a[0].page_number = 0;
    assert!(extract(&a).parties.is_empty());
}
#[test]
fn native_and_ocr_streams_have_independent_role_scope() {
    let mut blocks = contract();
    let mut native = blocks[1].clone();
    native.id = "native-owner".into();
    native.source = "native_pdf".into();
    native.text = native.text.replace("Lê Mẫu Một", "Lê Mẫu Mốt");
    blocks.push(native);
    let r = extract(&blocks);
    let owner = r.parties.iter().find(|p| p.designator == "A").unwrap();
    let tenant = r.parties.iter().find(|p| p.designator == "B").unwrap();
    assert_eq!(owner.fields["name"].state, FieldState::Ambiguous);
    assert_eq!(tenant.fields["name"].state, FieldState::Found);
}

#[test]
fn signature_designators_do_not_erase_roles_or_reenter_detail_scope() {
    let mut blocks = contract();
    blocks.push(block(
        "signature",
        3,
        "BÊN A                      BÊN B\nHọ và tên: Người ký khác\nNgày sinh: 01/01/1970",
    ));
    let r = extract(&blocks);
    assert!(r.supported_template);
    let owner = r.parties.iter().find(|p| p.designator == "A").unwrap();
    assert_eq!(owner.role, PartyRole::Landlord);
    assert_eq!(owner.fields["name"].candidates.len(), 1);
    assert!(!r.issues.iter().any(|i| i.code == "conflicting_party_role"));
}
#[test]
fn incompatible_explicit_roles_fail_closed() {
    let mut blocks = contract();
    blocks.push(block("bad-role", 3, "BÊN THUÊ (BÊN A)"));
    let r = extract(&blocks);
    assert!(!r.supported_template);
    assert_eq!(
        r.parties.iter().find(|p| p.designator == "A").unwrap().role,
        PartyRole::Unknown
    );
    assert!(r.issues.iter().any(|i| i.code == "conflicting_party_role"));
}
#[test]
fn explicit_new_administrative_property_address_anchor_is_supported() {
    let mut blocks = contract();
    blocks.push(block(
        "address",
        1,
        "Địa chỉ Nhà (theo địa giới mới): 42 Phố Mẫu",
    ));
    let r = extract(&blocks);
    assert_eq!(r.fields["property_address"].state, FieldState::Found);
    assert_eq!(
        value(r.fields["property_address"].selected.as_ref().unwrap()),
        "42 Phố Mẫu"
    );
}
#[test]
fn embedded_known_id_in_long_or_malformed_digit_run_is_not_linked() {
    for mrz in [
        "IDVNM000000000002345678901200000<<8",
        "IDVNM4567890127023456789012<<8",
        "IDVNM4567890126023456789012<<88",
        "IDVNM9999999999023456789012<<8",
    ] {
        let mut blocks = contract();
        blocks.push(block("bad-back",4,&format!("Đặc điểm nhân dạng / Personal identification:\nNgày, tháng, năm / Date, month, year: 10/10/2021\n{mrz}")));
        let r = extract(&blocks);
        assert_eq!(r.identity_documents[0].linkage.state, "unmatched", "{mrz}");
        assert!(
            r.parties
                .iter()
                .all(|p| p.linked_identity_documents.is_empty())
        );
    }
}
#[test]
fn decimal_money_preserves_cents_and_rejects_malformed_groups() {
    let mut blocks = contract();
    blocks[3].text = "Giá thuê: 1.234.567,50 VND/tháng\nTiền cọc: 1,000.25 VND".into();
    let r = extract(&blocks);
    assert_eq!(
        r.fields["monthly_rent"].selected,
        Some(FieldValue::Money {
            amount: "1234567.5".into(),
            currency: "VND".into(),
            period: Some("month".into())
        })
    );
    assert!(
        r.fields["monthly_rent"].candidates[0].evidence[0]
            .quote
            .ends_with("/tháng")
    );
    assert_eq!(
        r.fields["deposit_amount"].selected,
        Some(FieldValue::Money {
            amount: "1000.25".into(),
            currency: "VND".into(),
            period: None
        })
    );
    blocks[3].text = "Giá thuê: 10.00.000 VND/tháng".into();
    assert_eq!(
        extract(&blocks).fields["monthly_rent"].state,
        FieldState::Unreadable
    );
}
#[test]
fn invalid_geometry_and_nonfinite_detection_score_fail_closed() {
    let mut blocks = contract();
    blocks[1].bbox_2d = Some([100, 20, 10, 30]);
    assert!(extract(&blocks).parties.is_empty());
    blocks = contract();
    blocks[1].detector_score = Some(f64::NAN);
    assert!(extract(&blocks).parties.is_empty());
}

#[test]
fn ambiguous_contract_identifier_remains_a_possible_link_competitor() {
    let mut blocks = contract();
    blocks[1].text.push_str("\nSố giấy tờ: 023456789012");
    blocks.push(front(
        "front",
        "023456789012",
        "Đỗ Mẫu Hai",
        "07/08/1990",
        "20 Phố Giả",
    ));
    let r = extract(&blocks);
    assert_eq!(r.identity_documents[0].linkage.state, "ambiguous");
    assert_eq!(r.identity_documents[0].linkage.party_ids.len(), 2);
    assert!(
        r.parties
            .iter()
            .all(|p| p.linked_identity_documents.is_empty())
    );
}

#[test]
fn oversized_extraction_inputs_are_fail_closed_before_manifest_or_issues_expand() {
    let cases = [
        vec![block("hashes", 1, &"#".repeat(5000))],
        vec![block("long-block", 1, &"x".repeat(128 * 1024 + 1))],
        (0..2049)
            .map(|i| block(&format!("block-{i}"), 1, "x"))
            .collect(),
        (0..9)
            .map(|i| block(&format!("big-{i}"), 1, &"x".repeat(128 * 1024)))
            .collect(),
        vec![block("many-lines", 1, &"x\n".repeat(10_001))],
        vec![block(
            "many-candidates",
            1,
            &"Họ và tên: Ví Dụ\n".repeat(65),
        )],
        vec![block("many-alerts", 1, &"tạm dùng ".repeat(500))],
        vec![block(
            "many-inline-areas",
            1,
            &"Đặc điểm: diện tích sử dụng 60 m²\n".repeat(65),
        )],
        vec![block(
            "many-back-dates",
            1,
            &"Ngày, tháng, năm / Date, month, year: 01/01/2020\n".repeat(65),
        )],
    ];
    for blocks in cases {
        let result = extract(&blocks);
        assert_eq!(result.decision, Decision::NeedsReview);
        assert!(!result.supported_template);
        assert!(result.source_blocks.is_empty());
        assert!(result.fields.is_empty());
        assert!(result.parties.is_empty());
        assert!(result.identity_documents.is_empty());
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].code, "extraction_budget_exceeded");
        assert!(serde_json::to_vec(&result).unwrap().len() < 3000);
    }
}
#[test]
fn year_zero_is_unreadable_in_iso_calendar_contract() {
    let mut blocks = contract();
    blocks[1].text = blocks[1].text.replace("02/03/1980", "02/03/0000");
    let result = extract(&blocks);
    assert_eq!(
        result.parties[0].contract_fields["birth_date"].state,
        FieldState::Unreadable
    );
}

#[test]
fn property_address_from_explicit_rental_sentence_is_extracted() {
    let mut blocks = contract();
    blocks[3].text = blocks[3].text.replace("Địa chỉ Nhà thuê: 42 Phố Mẫu\n", "");
    blocks.push(block(
        "narrative-address",
        1,
        "Bên A đồng ý cho Bên B thuê toàn bộ nhà ở tại địa chỉ: 42 Phố Mẫu",
    ));
    let result = extract(&blocks);
    assert_eq!(result.fields["property_address"].state, FieldState::Found);
    assert_eq!(
        value(result.fields["property_address"].selected.as_ref().unwrap()),
        "42 Phố Mẫu"
    );
    assert_eq!(result.fields["property_address"].candidates.len(), 1);
    assert_eq!(
        result.fields["property_address"].candidates[0].evidence[0].block_id,
        "narrative-address"
    );
}

#[test]
fn recovery_only_value_is_retained_but_never_selected_without_first_pass_support() {
    let mut blocks = contract();
    blocks[2].text = blocks[2].text.replace("07/08/1990 / Nam", "");
    let mut recovery = blocks[2].clone();
    recovery.id = "expanded_tenant".into();
    recovery.source = "ocr_recovery_context".into();
    recovery.text = recovery.text.replace(
        "Ngày sinh / Giới tính:",
        "Ngày sinh / Giới tính: 09/10/1991",
    );
    blocks.push(recovery);
    let result = extract(&blocks);
    let tenant = result
        .parties
        .iter()
        .find(|p| p.role == PartyRole::Tenant)
        .unwrap();
    let birth = &tenant.fields["birth_date"];
    assert_eq!(birth.state, FieldState::Ambiguous);
    assert!(birth.selected.is_none());
    assert_eq!(birth.candidates.len(), 1);
    assert_eq!(
        birth.candidates[0].evidence[0].source,
        "ocr_recovery_context"
    );
    assert!(
        result
            .issues
            .iter()
            .any(|issue| issue.code == "recovery_only_observation"
                && issue.field_path.as_deref()
                    == Some("parties.contract_party_b.fields.birth_date"))
    );
}

#[test]
fn expanded_crop_cannot_promote_adjacent_field_into_a_missing_party_value() {
    let mut blocks = contract();
    blocks[2].text = blocks[2]
        .text
        .replace("Ngày sinh / Giới tính: 07/08/1990 / Nam\n", "");
    // Expanded pixels include a neighboring date that the first-pass party never observed.
    let mut recovery = block(
        "expanded_adjacent",
        1,
        "BÊN THUÊ (BÊN B)\nNgày sinh: 02/03/1980",
    );
    recovery.source = "ocr_recovery_context".into();
    blocks.push(recovery);
    let result = extract(&blocks);
    let tenant = result
        .parties
        .iter()
        .find(|p| p.role == PartyRole::Tenant)
        .unwrap();
    assert_eq!(tenant.fields["birth_date"].state, FieldState::Ambiguous);
    assert!(tenant.fields["birth_date"].selected.is_none());
    assert_eq!(
        value(&tenant.fields["birth_date"].candidates[0].value),
        "1980-03-02"
    );
}

#[test]
fn repeated_recovery_context_is_not_independent_identity_corroboration() {
    let mut blocks = contract();
    let mut recovery = block(
        "expanded_identity",
        4,
        "CĂN CƯỚC CÔNG DÂN / CITIZEN IDENTITY CARD\nSố / No.: 023456789012\nHọ và tên / Full name: Đỗ Mẫu Hai\nNgày sinh / Date of birth: 09/10/1991",
    );
    recovery.source = "ocr_recovery_context".into();
    blocks.push(recovery);
    let result = extract(&blocks);
    let tenant = result
        .parties
        .iter()
        .find(|p| p.role == PartyRole::Tenant)
        .unwrap();
    assert_eq!(tenant.fields["birth_date"].state, FieldState::Found);
    assert_eq!(
        value(tenant.fields["birth_date"].selected.as_ref().unwrap()),
        "1990-08-07"
    );
    assert_eq!(
        result.identity_documents.last().unwrap().linkage.state,
        "ambiguous"
    );
    assert!(
        result.identity_documents.last().unwrap().fields["id_number"]
            .selected
            .is_none()
    );
}
