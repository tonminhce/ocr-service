use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::read_bounded;

const SCHEMA: &str = "ocr-eval-prediction/2.0.0";
const NORMALIZATION: &str = "nfc-typed/1.0.0";
const SUPPORTED_FIELDS: [&str; 12] = [
    "name",
    "id_number",
    "birth_date",
    "issue_date",
    "residence_address",
    "contact_address",
    "phone",
    "bank_account",
    "monthly_rent",
    "deposit_amount",
    "property_address",
    "property_area",
];

#[derive(Serialize)]
pub struct ExportReport {
    pub schema_version: &'static str,
    pub sidecar_path: String,
    pub source_sha256: String,
    pub machine_only: bool,
    pub pages: usize,
    pub entities: usize,
    pub transcripts: usize,
    pub observations: usize,
    pub fields: usize,
    pub unsupported_fields: Vec<String>,
    pub abstained_pages: Vec<u32>,
    pub region_bound_observations: usize,
    pub page_anchored_observations: usize,
    pub ambiguous_binds: usize,
    pub limitations: Vec<String>,
}

fn polygon_area(polygon: &[[f64; 2]]) -> f64 {
    let mut area = 0.0;
    for index in 0..polygon.len() {
        let next = (index + 1) % polygon.len();
        area += polygon[index][0] * polygon[next][1] - polygon[next][0] * polygon[index][1];
    }
    (area / 2.0).abs()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn page_rect_anchor(page: &serde_json::Value) -> Option<serde_json::Value> {
    let width = page.get("width")?.as_u64()?;
    let height = page.get("height")?.as_u64()?;
    if width == 0 || height == 0 || width > 32768 || height > 32768 {
        return None;
    }
    Some(serde_json::json!({
        "page_number": page.get("page_number")?.as_u64()?,
        "source_kind": "contract",
        "side": "not_applicable",
        "granularity": "block",
        "polygon": [[0.0, 0.0], [width as f64, 0.0], [width as f64, height as f64], [0.0, height as f64]],
    }))
}

struct PartyTarget {
    role_id: String,
    document_id: String,
}

fn parse_field_path(path: &str) -> Option<(Option<PartyTarget>, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    match parts.as_slice() {
        [document, "parties", role, field] => Some((
            Some(PartyTarget {
                role_id: role.to_string(),
                document_id: document.to_string(),
            }),
            field.to_string(),
        )),
        [document, field] if SUPPORTED_FIELDS.contains(field) => Some((None, field.to_string())),
        _ => None,
    }
}

pub fn export(
    extraction_path: &Path,
    result_path: &Path,
    source_sha256: &str,
    output_path: &Path,
) -> Result<ExportReport> {
    ensure!(
        source_sha256.len() == 64
            && source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "source SHA-256 must be 64 lowercase hex characters"
    );
    let extraction_bytes = read_bounded(extraction_path, 16 * 1024 * 1024)?;
    let result_bytes = read_bounded(result_path, 32 * 1024 * 1024)?;
    let extraction: serde_json::Value = serde_json::from_slice(&extraction_bytes)
        .context("extraction artifact is not valid JSON")?;
    let result: serde_json::Value =
        serde_json::from_slice(&result_bytes).context("OCR result artifact is not valid JSON")?;

    let result_pages = result
        .get("pages")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let roles = extraction
        .get("roles")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let logical_documents = extraction
        .get("logical_documents")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let artifacts = extraction
        .get("source_artifacts")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let fields = extraction
        .get("fields")
        .and_then(|f| f.as_object())
        .cloned()
        .unwrap_or_default();

    let mut pages = Vec::new();
    let mut anchors_by_page: Vec<(u32, serde_json::Value)> = Vec::new();
    let mut abstained_pages = Vec::new();
    for page in &result_pages {
        let number = page
            .get("page_number")
            .and_then(|n| n.as_u64())
            .context("result page lacks a page number")? as u32;
        let Some(anchor) = page_rect_anchor(page) else {
            abstained_pages.push(number);
            continue;
        };
        pages.push(serde_json::json!({
            "number": number,
            "width": page["width"],
            "height": page["height"],
            "rotation_degrees": 0,
            "crop_box_pdf": [0.0, 0.0, page["width"].as_f64().unwrap_or(0.0), page["height"].as_f64().unwrap_or(0.0)],
        }));
        anchors_by_page.push((number, anchor));
    }
    ensure!(
        !pages.is_empty(),
        "no page in the OCR result carries usable dimensions"
    );
    let anchor_for_page = |number: u32| -> Option<&serde_json::Value> {
        anchors_by_page
            .iter()
            .find(|(page_number, _)| *page_number == number)
            .map(|(_, anchor)| anchor)
    };

    struct RegionEntry {
        anchor: serde_json::Value,
        content: String,
        index: u32,
    }
    let mut regions_by_page: Vec<(u32, Vec<RegionEntry>)> = Vec::new();
    for page in &result_pages {
        let number = page
            .get("page_number")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        let width = page.get("width").and_then(|w| w.as_f64()).unwrap_or(0.0);
        let height = page.get("height").and_then(|h| h.as_f64()).unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let mut entries = Vec::new();
        if let Some(regions) = page.get("regions").and_then(|r| r.as_array()) {
            for region in regions {
                let Some(content) = region.get("content").and_then(|c| c.as_str()) else {
                    continue;
                };
                if content.trim().is_empty() {
                    continue;
                }
                let Some(polygon) = region.get("polygon").and_then(|p| p.as_array()) else {
                    continue;
                };
                if polygon.len() < 3 || polygon.len() > 16 {
                    continue;
                }
                let mut scaled: Vec<[f64; 2]> = Vec::with_capacity(polygon.len());
                let mut degenerate = false;
                for vertex in polygon {
                    let Some(pair) = vertex.as_array() else {
                        degenerate = true;
                        break;
                    };
                    if pair.len() != 2 {
                        degenerate = true;
                        break;
                    }
                    let x = pair[0].as_f64().unwrap_or(-1.0) * width / 1000.0;
                    let y = pair[1].as_f64().unwrap_or(-1.0) * height / 1000.0;
                    if !(x.is_finite() && y.is_finite()) {
                        degenerate = true;
                        break;
                    }
                    scaled.push([x.clamp(0.0, width), y.clamp(0.0, height)]);
                }
                if degenerate {
                    continue;
                }
                if polygon_area(&scaled) <= 0.01 {
                    continue;
                }
                let index = region.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                entries.push(RegionEntry {
                    anchor: serde_json::json!({
                        "page_number": number,
                        "source_kind": "contract",
                        "side": "not_applicable",
                        "granularity": "block",
                        "polygon": scaled,
                    }),
                    content: content.to_string(),
                    index,
                });
            }
        }
        regions_by_page.push((number, entries));
    }
    let regions_for_page = |number: u32| -> Option<&Vec<RegionEntry>> {
        regions_by_page
            .iter()
            .find(|(page_number, _)| *page_number == number)
            .map(|(_, entries)| entries)
    };

    let page_number_of_artifact = |artifact_id: &str| -> Option<u32> {
        artifacts
            .iter()
            .find(|a| a.get("artifact_id").and_then(|i| i.as_str()) == Some(artifact_id))
            .and_then(|a| a.get("page_number"))
            .and_then(|p| p.as_u64())
            .map(|p| p as u32)
    };

    let mut entities = Vec::new();
    let mut used_documents: Vec<String> = Vec::new();
    for document in &logical_documents {
        let document_id = document
            .get("document_id")
            .and_then(|i| i.as_str())
            .context("logical document lacks an ID")?;
        let page = document
            .get("pages")
            .and_then(|p| p.as_array())
            .and_then(|p| p.first())
            .and_then(|p| p.as_u64())
            .unwrap_or(1) as u32;
        let Some(anchor) = anchor_for_page(page) else {
            continue;
        };
        entities.push(serde_json::json!({
            "id": format!("doc_{document_id}"),
            "kind": "document",
            "document_id": null,
            "party_id": null,
            "role": null,
            "anchor": anchor,
        }));
        used_documents.push(document_id.to_string());
    }

    let mut transcripts = Vec::new();
    let mut transcript_ids: Vec<String> = Vec::new();
    for artifact in &artifacts {
        let artifact_id = artifact
            .get("artifact_id")
            .and_then(|i| i.as_str())
            .context("source artifact lacks an ID")?;
        let page = artifact
            .get("page_number")
            .and_then(|p| p.as_u64())
            .context("source artifact lacks a page number")? as u32;
        let Some(anchor) = anchor_for_page(page) else {
            continue;
        };
        let text = artifact
            .get("text")
            .and_then(|t| t.as_str())
            .context("source artifact lacks text")?;
        transcripts.push(serde_json::json!({
            "id": artifact_id,
            "text": text,
            "anchor": anchor,
        }));
        transcript_ids.push(artifact_id.to_string());
    }
    ensure!(
        !transcripts.is_empty(),
        "no source artifact maps to a page with dimensions"
    );

    for role in &roles {
        let role_id = role
            .get("role_id")
            .and_then(|i| i.as_str())
            .context("role lacks an ID")?;
        let document_id = role
            .get("logical_document_id")
            .and_then(|i| i.as_str())
            .context("role lacks a logical document")?;
        ensure!(
            used_documents.contains(&document_id.to_string()),
            "role references an unexported logical document"
        );
        let role_type = role
            .get("role_type")
            .and_then(|t| t.as_str())
            .unwrap_or("other");
        let page = logical_documents
            .iter()
            .find(|d| d.get("document_id").and_then(|i| i.as_str()) == Some(document_id))
            .and_then(|d| d.get("pages"))
            .and_then(|p| p.as_array())
            .and_then(|p| p.first())
            .and_then(|p| p.as_u64())
            .unwrap_or(1) as u32;
        let Some(anchor) = anchor_for_page(page) else {
            continue;
        };
        entities.push(serde_json::json!({
            "id": format!("party_{document_id}_{role_id}"),
            "kind": "party",
            "document_id": format!("doc_{document_id}"),
            "party_id": null,
            "role": role_type,
            "anchor": anchor,
        }));
    }
    ensure!(
        entities.len() >= 2,
        "export requires at least a document and one party entity"
    );

    let mut observations = Vec::new();
    let mut exported_fields = Vec::new();
    struct RegionBind {
        transcript_id: String,
        anchor: serde_json::Value,
        content: String,
        byte_start: usize,
        byte_end: usize,
    }
    let bind_region = |page: u32, quote: &str| -> Result<Option<RegionBind>, usize> {
        let Some(entries) = regions_for_page(page) else {
            return Ok(None);
        };
        let mut matches = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .content
                    .as_bytes()
                    .windows(quote.len().max(1))
                    .position(|window| window == quote.as_bytes())
                    .map(|start| (entry, start))
            })
            .collect::<Vec<_>>();
        match matches.len() {
            1 => {
                let (entry, start) = matches.remove(0);
                Ok(Some(RegionBind {
                    transcript_id: format!("{}_r{}", entry.anchor["page_number"], entry.index),
                    anchor: entry.anchor.clone(),
                    content: entry.content.clone(),
                    byte_start: start,
                    byte_end: start + quote.len(),
                }))
            }
            0 => Ok(None),
            other => Err(other),
        }
    };
    let mut unsupported = Vec::new();
    let mut region_bound_observations = 0usize;
    let mut page_anchored_observations = 0usize;
    let mut ambiguous_binds = 0usize;
    let mut observation_sequence = 0usize;
    for (path, field) in &fields {
        let Some((party, field_key)) = parse_field_path(path) else {
            unsupported.push(path.clone());
            continue;
        };
        ensure!(
            SUPPORTED_FIELDS.contains(&field_key.as_str()),
            "field key outside the supported v2 enum: {field_key}"
        );
        let document_id = party
            .as_ref()
            .map(|p| p.document_id.clone())
            .unwrap_or_else(|| "contract".to_string());
        let party_reference = party
            .as_ref()
            .map(|p| format!("party_{document_id}_{}", p.role_id));
        if let Some(p) = &party {
            ensure!(
                roles
                    .iter()
                    .any(|r| r.get("role_id").and_then(|i| i.as_str()) == Some(p.role_id.as_str())),
                "field references an unknown role"
            );
        }
        let status = field
            .get("value_status")
            .and_then(|s| s.as_str())
            .context("field lacks a value status")?;
        let evidence_list = field
            .get("evidence")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        let candidates = field
            .get("candidates")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        let mut field_observation_ids: Vec<String> = Vec::new();
        let mut emit_observation = |visible_text: &str,
                                    value: Option<&serde_json::Value>,
                                    state: &str,
                                    origin: &str,
                                    anchor: &serde_json::Value,
                                    transcript_id: &str,
                                    byte_start: usize,
                                    byte_end: usize,
                                    observations: &mut Vec<serde_json::Value>,
                                    ids_out: &mut Vec<String>| {
            observation_sequence += 1;
            let id = format!("obs_{observation_sequence}");
            observations.push(serde_json::json!({
                "id": id,
                "document_id": format!("doc_{document_id}"),
                "party_id": party_reference,
                "card_id": null,
                "field": field_key,
                "state": state,
                "origin": origin,
                "visible_text": visible_text,
                "value": value,
                "anchor": anchor,
                "evidence": {
                    "transcript_id": transcript_id,
                    "byte_start": byte_start,
                    "byte_end": byte_end,
                    "quote_sha256": sha256_hex(visible_text.as_bytes()),
                },
            }));
            ids_out.push(id);
        };

        match status {
            "found" => {
                let value = field.get("normalized_value");
                ensure!(value.is_some(), "found field lacks a normalized value");
                for evidence in &evidence_list {
                    let quote = evidence
                        .get("quote")
                        .and_then(|q| q.as_str())
                        .context("found field evidence lacks a quote")?;
                    let artifact_id = evidence
                        .get("artifact_id")
                        .and_then(|i| i.as_str())
                        .context("found field evidence lacks an artifact reference")?;
                    ensure!(
                        transcript_ids.contains(&artifact_id.to_string()),
                        "evidence references an unexported artifact"
                    );
                    let page = page_number_of_artifact(artifact_id)
                        .context("evidence artifact lacks a page")?;
                    let span = evidence.get("span").context("evidence lacks a span")?;
                    let binding = bind_region(page, quote);
                    if let Err(matches) = binding {
                        let _ = matches;
                        ambiguous_binds += 1;
                    } else if let Some(bind) = binding.unwrap() {
                        region_bound_observations += 1;
                        if !transcript_ids.contains(&bind.transcript_id) {
                            transcripts.push(serde_json::json!({
                                "id": bind.transcript_id,
                                "text": bind.content,
                                "anchor": bind.anchor,
                            }));
                            transcript_ids.push(bind.transcript_id.clone());
                        }
                        emit_observation(
                            quote,
                            value,
                            "present",
                            "first_pass",
                            &bind.anchor,
                            &bind.transcript_id,
                            bind.byte_start,
                            bind.byte_end,
                            &mut observations,
                            &mut field_observation_ids,
                        );
                        continue;
                    }
                    let Some(anchor) = anchor_for_page(page) else {
                        continue;
                    };
                    page_anchored_observations += 1;
                    emit_observation(
                        quote,
                        value,
                        "present",
                        "first_pass",
                        anchor,
                        artifact_id,
                        span.get("start").and_then(|s| s.as_u64()).unwrap_or(0) as usize,
                        span.get("end").and_then(|e| e.as_u64()).unwrap_or(0) as usize,
                        &mut observations,
                        &mut field_observation_ids,
                    );
                }
                ensure!(
                    !field_observation_ids.is_empty(),
                    "found field lacks exportable evidence"
                );
                exported_fields.push(serde_json::json!({
                    "document_id": format!("doc_{document_id}"),
                    "party_id": party_reference,
                    "field": field_key,
                    "state": "found",
                    "observation_ids": field_observation_ids,
                    "ambiguity_reason": null,
                }));
            }
            "missing" => {
                exported_fields.push(serde_json::json!({
                    "document_id": format!("doc_{document_id}"),
                    "party_id": party_reference,
                    "field": field_key,
                    "state": "missing",
                    "observation_ids": [],
                    "ambiguity_reason": null,
                }));
            }
            "unreadable" | "placeholder" => {
                let state = if status == "unreadable" {
                    "unreadable"
                } else {
                    "placeholder"
                };
                let Some(evidence) = evidence_list.first() else {
                    unsupported.push(path.clone());
                    continue;
                };
                let quote = evidence
                    .get("quote")
                    .and_then(|q| q.as_str())
                    .context("state field evidence lacks a quote")?;
                let artifact_id = evidence
                    .get("artifact_id")
                    .and_then(|i| i.as_str())
                    .context("state field evidence lacks an artifact reference")?;
                let page = page_number_of_artifact(artifact_id)
                    .context("evidence artifact lacks a page")?;
                let Some(anchor) = anchor_for_page(page) else {
                    continue;
                };
                let span = evidence.get("span").context("evidence lacks a span")?;
                emit_observation(
                    quote,
                    None,
                    state,
                    "first_pass",
                    anchor,
                    artifact_id,
                    span.get("start").and_then(|s| s.as_u64()).unwrap_or(0) as usize,
                    span.get("end").and_then(|e| e.as_u64()).unwrap_or(0) as usize,
                    &mut observations,
                    &mut field_observation_ids,
                );
                exported_fields.push(serde_json::json!({
                    "document_id": format!("doc_{document_id}"),
                    "party_id": party_reference,
                    "field": field_key,
                    "state": state,
                    "observation_ids": field_observation_ids,
                    "ambiguity_reason": null,
                }));
            }
            "ambiguous" | "conflict" => {
                let mut emitted_any = false;
                for candidate in &candidates {
                    let Some(value) = candidate.get("normalized_value") else {
                        continue;
                    };
                    let Some(evidence) = candidate
                        .get("evidence")
                        .and_then(|e| e.as_array())
                        .and_then(|e| e.first())
                    else {
                        continue;
                    };
                    let quote = evidence
                        .get("quote")
                        .and_then(|q| q.as_str())
                        .context("candidate evidence lacks a quote")?;
                    let artifact_id = evidence
                        .get("artifact_id")
                        .and_then(|i| i.as_str())
                        .context("candidate evidence lacks an artifact reference")?;
                    let page = page_number_of_artifact(artifact_id)
                        .context("evidence artifact lacks a page")?;
                    let span = evidence
                        .get("span")
                        .context("candidate evidence lacks a span")?;
                    let origin = candidate
                        .get("origin")
                        .and_then(|o| o.as_str())
                        .unwrap_or("first_pass");
                    let origin = if origin == "recovery" {
                        "recovery"
                    } else {
                        "first_pass"
                    };
                    let binding = bind_region(page, quote);
                    if let Err(matches) = binding {
                        let _ = matches;
                        ambiguous_binds += 1;
                    } else if let Some(bind) = binding.unwrap() {
                        region_bound_observations += 1;
                        if !transcript_ids.contains(&bind.transcript_id) {
                            transcripts.push(serde_json::json!({
                                "id": bind.transcript_id,
                                "text": bind.content,
                                "anchor": bind.anchor,
                            }));
                            transcript_ids.push(bind.transcript_id.clone());
                        }
                        emit_observation(
                            quote,
                            Some(value),
                            "present",
                            origin,
                            &bind.anchor,
                            &bind.transcript_id,
                            bind.byte_start,
                            bind.byte_end,
                            &mut observations,
                            &mut field_observation_ids,
                        );
                        emitted_any = true;
                        continue;
                    }
                    let Some(anchor) = anchor_for_page(page) else {
                        continue;
                    };
                    page_anchored_observations += 1;
                    emit_observation(
                        quote,
                        Some(value),
                        "present",
                        origin,
                        anchor,
                        artifact_id,
                        span.get("start").and_then(|s| s.as_u64()).unwrap_or(0) as usize,
                        span.get("end").and_then(|e| e.as_u64()).unwrap_or(0) as usize,
                        &mut observations,
                        &mut field_observation_ids,
                    );
                    emitted_any = true;
                }
                if !emitted_any {
                    unsupported.push(path.clone());
                    continue;
                }
                let (state, reason) = if status == "conflict" {
                    ("conflict", None)
                } else {
                    ("ambiguous", Some("uncertain_source"))
                };
                exported_fields.push(serde_json::json!({
                    "document_id": format!("doc_{document_id}"),
                    "party_id": party_reference,
                    "field": field_key,
                    "state": state,
                    "observation_ids": field_observation_ids,
                    "ambiguity_reason": reason,
                }));
            }
            other => {
                unsupported.push(path.clone());
                let _ = other;
            }
        }
    }
    ensure!(
        !exported_fields.is_empty(),
        "no supported field could be exported"
    );

    let sidecar = serde_json::json!({
        "schema_version": SCHEMA,
        "normalization_policy": NORMALIZATION,
        "source_sha256": source_sha256,
        "machine_only": true,
        "pages": pages,
        "entities": entities,
        "transcripts": transcripts,
        "observations": observations,
        "fields": exported_fields,
    });
    let sidecar_bytes = serde_json::to_vec_pretty(&sidecar)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &sidecar_bytes)?;
    Ok(ExportReport {
        schema_version: SCHEMA,
        sidecar_path: output_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        machine_only: true,
        pages: pages.len(),
        entities: entities.len(),
        transcripts: transcripts.len(),
        observations: observations.len(),
        fields: exported_fields.len(),
        unsupported_fields: unsupported,
        abstained_pages,
        region_bound_observations,
        page_anchored_observations,
        ambiguous_binds,
        limitations: vec![
            "Observations bind to real region polygons where a quote uniquely matches one stored layout region; page-rectangle fallback is used otherwise.".into(),
            "Card entities are outside the current supported scope and are not exported.".into(),
            "This machine sidecar does not prove OCR accuracy; independent gold remains the quality gate.".into(),
        ],
    })
}
