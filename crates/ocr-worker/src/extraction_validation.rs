//! Validate references independently of rule generation before storing results.
use std::collections::BTreeMap;

use ocr_extraction::{ExtractionResult, SourceBlock};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProcessingError;

pub(crate) fn validate(
    result: &ExtractionResult,
    blocks: &[SourceBlock],
) -> Result<(), ProcessingError> {
    let fail = || ProcessingError::permanent("structured extraction failed evidence validation");
    if result
        .issues
        .iter()
        .any(|i| i.code == "extraction_budget_exceeded")
    {
        if result.source_blocks.is_empty()
            && result.parties.is_empty()
            && result.identity_documents.is_empty()
            && result.fields.values().all(|f| {
                f.selected.is_none() && f.candidates.is_empty() && f.observations.is_empty()
            })
        {
            return Ok(());
        }
        return Err(fail());
    }
    let mut sources = BTreeMap::new();
    for block in blocks {
        if block.id.is_empty()
            || block.page_number == 0
            || sources.insert(block.id.as_str(), block).is_some()
            || block
                .detector_score
                .is_some_and(|s| !s.is_finite() || !(0.0..=1.0).contains(&s))
            || block.bbox_2d.is_some_and(|b| {
                b.iter().any(|n| !(0..=1000).contains(n)) || b[0] >= b[2] || b[1] >= b[3]
            })
        {
            return Err(fail());
        }
    }
    if result.source_blocks.len() != blocks.len() {
        return Err(fail());
    }
    let mut references = std::collections::BTreeSet::new();
    for reference in &result.source_blocks {
        let block = sources.get(reference.block_id.as_str()).ok_or_else(fail)?;
        if !references.insert(&reference.block_id)
            || reference.page_number != block.page_number
            || reference.region_index != block.region_index
            || reference.source != block.source
            || reference.text_bytes != block.text.len()
            || reference.text_sha256 != format!("{:x}", Sha256::digest(block.text.as_bytes()))
        {
            return Err(fail());
        }
    }
    let value = serde_json::to_value(result).map_err(|_| fail())?;
    walk(&value, &sources)?;
    Ok(())
}

fn walk(value: &Value, sources: &BTreeMap<&str, &SourceBlock>) -> Result<(), ProcessingError> {
    let fail = || ProcessingError::permanent("structured extraction failed evidence validation");
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, sources)?;
            }
        }
        Value::Object(map) => {
            if map.contains_key("byte_start") {
                let id = map["block_id"].as_str().ok_or_else(fail)?;
                let block = sources.get(id).ok_or_else(fail)?;
                let start = map["byte_start"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(fail)?;
                let end = map["byte_end"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(fail)?;
                if start >= end
                    || block.text.get(start..end) != map["quote"].as_str()
                    || map["page_number"].as_u64() != Some(u64::from(block.page_number))
                    || map["region_index"] != serde_json::json!(block.region_index)
                    || map["source"].as_str() != Some(block.source.as_str())
                    || map["bbox_2d"] != serde_json::json!(block.bbox_2d)
                    || map["geometry_scope"] != "source_block"
                    || !map["confidence"].is_null()
                {
                    return Err(fail());
                }
            }
            if map.contains_key("state") && map.contains_key("selected") {
                let candidates = map["candidates"].as_array().ok_or_else(fail)?;
                let selected = &map["selected"];
                let valid = match map["state"].as_str() {
                    Some("found") => {
                        !selected.is_null()
                            && !candidates.is_empty()
                            && candidates
                                .iter()
                                .all(|c| c["evidence"].as_array().is_some_and(|e| !e.is_empty()))
                            && candidates.iter().any(|c| {
                                &c["value"] == selected
                                    && c["evidence"].as_array().is_some_and(|evidence| {
                                        evidence
                                            .iter()
                                            .any(|ev| ev["source"] != "ocr_recovery_context")
                                    })
                            })
                    }
                    Some("missing" | "unreadable") => selected.is_null() && candidates.is_empty(),
                    Some("conflict" | "ambiguous") => {
                        let distinct = candidates.len() >= 2
                            && candidates
                                .iter()
                                .any(|c| c["value"] != candidates[0]["value"]);
                        let recovery_only = map["state"] == "ambiguous"
                            && !candidates.is_empty()
                            && candidates.iter().all(|candidate| {
                                candidate["evidence"].as_array().is_some_and(|evidence| {
                                    !evidence.is_empty()
                                        && evidence
                                            .iter()
                                            .all(|ev| ev["source"] == "ocr_recovery_context")
                                })
                            });
                        selected.is_null() && (distinct || recovery_only)
                    }
                    _ => false,
                };
                if !valid || !map["confidence"].is_null() {
                    return Err(fail());
                }
            }
            for item in map.values() {
                walk(item, sources)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_tampered_hashes_quotes_and_multibyte_offsets() {
        let block = SourceBlock {
            id: "b1".into(),
            page_number: 1,
            region_index: Some(0),
            source: "ocr".into(),
            text: "HỢP ĐỒNG THUÊ NHÀ\nBÊN CHO THUÊ (BÊN A)\nHọ và tên: Người Thử".into(),
            bbox_2d: Some([1, 2, 300, 400]),
            detector_score: Some(0.9),
        };
        let blocks = vec![block];
        let result = ocr_extraction::extract(&blocks);
        validate(&result, &blocks).unwrap();
        let mut changed = result.clone();
        changed.source_blocks[0].text_sha256 = "0".repeat(64);
        assert!(validate(&changed, &blocks).is_err());
        let mut changed = result.clone();
        changed.parties[0].role_evidence[0].quote = "fabricated".into();
        assert!(validate(&changed, &blocks).is_err());
        let mut changed = result;
        changed.parties[0].role_evidence[0].byte_start += 1;
        assert!(validate(&changed, &blocks).is_err());
    }
    #[test]
    fn rejects_forged_found_state_when_only_recovery_evidence_supports_value() {
        let blocks = vec![SourceBlock {
            id: "recovery".into(),
            page_number: 1,
            region_index: Some(0),
            source: "ocr_recovery_context".into(),
            text: "HỢP ĐỒNG THUÊ NHÀ\nBÊN CHO THUÊ (BÊN A)\nHọ và tên: Người Thử".into(),
            bbox_2d: Some([1, 2, 300, 400]),
            detector_score: None,
        }];
        let result = ocr_extraction::extract(&blocks);
        validate(&result, &blocks).unwrap();
        let mut changed = result;
        let field = changed.parties[0].fields.get_mut("name").unwrap();
        assert_eq!(field.state, ocr_extraction::FieldState::Ambiguous);
        field.state = ocr_extraction::FieldState::Found;
        field.selected = Some(field.candidates[0].value.clone());
        assert!(validate(&changed, &blocks).is_err());
    }
}
