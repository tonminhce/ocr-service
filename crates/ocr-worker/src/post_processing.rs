//! Evidence-preserving post-processing. Native PDF text is a separate,
//! explicitly unverified observation; raw layout and OCR are never rewritten.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, LazyLock},
};

use ocr_core::metrics::{Activity, Stage, process_metrics};
use ocr_extraction::SourceBlock;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::ProcessingError;

const MAX_BLOCKS: usize = 10_000;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const NATIVE_POLICY: &str = "parallel-native-ocr-evidence-v2";
static PROCESSING_SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));
const POPPLER_DOCTYPE: &str = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";

/// A requested profile is snapshotted in the PostgreSQL job at submission.
/// Extraction quality never changes the execution status into business approval.
pub async fn apply(
    result: &mut Value,
    media_type: &str,
    profile: &str,
    parser: Option<Arc<crate::parser_client::ParserSession>>,
    source_sha256: String,
    cancelled: CancellationToken,
) -> Result<(), ProcessingError> {
    if profile == "none" {
        return Ok(());
    }
    if profile != "vi_rental_contract_v1" {
        return Err(ProcessingError::permanent("unsupported extraction profile"));
    }
    let _timer = process_metrics().timer(Stage::Postprocess);
    let _active = process_metrics().active(Activity::Postprocess);
    let ocr = ocr_blocks(result)?;
    let recovery = recovery_context_blocks(result)?;
    let mut issues = Vec::new();
    let native = if media_type == "application/pdf" {
        let _timer = process_metrics().timer(Stage::NativeText);
        match match parser {
            Some(parser) => parser.native_text(cancelled.clone()).await,
            None => Err(ProcessingError::transient("isolated parser is unavailable")),
        } {
            Ok(blocks) => blocks,
            Err(error) => {
                if cancelled.is_cancelled() {
                    return Err(error);
                }
                if error.retryable {
                    return Err(error);
                }
                // A PDF text layer is optional. Its absence/failure must remain
                // visible rather than making incomplete extraction look accepted.
                issues.push(json!({"code":"native_pdf_unavailable","severity":"warning",
                    "message":"Native PDF text could not be inspected; OCR-only evidence remains unverified."}));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let permit = tokio::select! {
        _=cancelled.cancelled()=>return Err(ProcessingError::transient("job processing cancelled")),
        permit=PROCESSING_SLOTS.clone().acquire_owned()=>permit.map_err(|_|ProcessingError::transient("post-processing is stopping"))?,
    };
    let stopped = cancelled.clone();
    let profile = profile.to_owned();
    let mut complete = std::mem::take(result);
    let complete = tokio::task::spawn_blocking(move || {
        // A cancelled waiter retains its permit until bounded CPU work exits.
        let _permit = permit;
        let (post_processing, extraction) = make_output(
            SourceInputs {
                ocr,
                native,
                recovery,
            },
            &profile,
            source_sha256,
            issues,
            &stopped,
        )?;
        complete["post_processing"] = post_processing;
        complete["extraction"] = extraction;
        if serde_json::to_vec(&complete)
            .map_err(|_| ProcessingError::permanent("cannot serialize extraction result"))?
            .len()
            > 16 * 1024 * 1024
        {
            return Err(ProcessingError::permanent(
                "extraction result exceeds the 16 MiB storage budget",
            ));
        }
        Ok(complete)
    })
    .await
    .map_err(|_| ProcessingError::transient("post-processing task failed"))??;
    if cancelled.is_cancelled() {
        return Err(ProcessingError::transient("job processing cancelled"));
    }
    *result = complete;
    Ok(())
}

struct SourceInputs {
    ocr: Vec<SourceBlock>,
    native: Vec<SourceBlock>,
    recovery: Vec<SourceBlock>,
}

fn make_output(
    sources: SourceInputs,
    profile: &str,
    source_sha256: String,
    issues: Vec<Value>,
    cancelled: &CancellationToken,
) -> Result<(Value, Value), ProcessingError> {
    let SourceInputs {
        ocr,
        native,
        recovery,
    } = sources;
    if cancelled.is_cancelled() {
        return Err(ProcessingError::transient("job processing cancelled"));
    }
    let (mut blocks, pages) = select_sources(&ocr, &native);
    blocks.extend(recovery);
    let hashes: BTreeMap<_, _> = blocks
        .iter()
        .map(|block| {
            (
                block.id.clone(),
                format!("{:x}", Sha256::digest(block.text.as_bytes())),
            )
        })
        .collect();
    // The reviewer projection may use native text, but extraction sees both
    // streams so mixed pages cannot silently lose an OCR-only image or field.
    let native_pages: HashSet<_> = blocks
        .iter()
        .filter(|b| b.source == "native_pdf")
        .map(|b| b.page_number)
        .collect();
    let markdown = blocks
        .iter()
        .filter(|b| {
            b.source == "native_pdf"
                || (b.source == "ocr" && !native_pages.contains(&b.page_number))
        })
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let extraction = {
        let _timer = process_metrics().timer(Stage::Extraction);
        ocr_extraction::extract(&blocks)
    };
    {
        let _timer = process_metrics().timer(Stage::Validation);
        crate::extraction_validation::validate(&extraction, &blocks)?;
    }
    if cancelled.is_cancelled() {
        return Err(ProcessingError::transient("job processing cancelled"));
    }
    let raw_ocr_issues = ocr_extraction::extract(&ocr).issues;
    let native_comparison = compare_sources(&ocr, &native);
    let post_processing = json!({
        "version":"evidence-postprocess-v1", "profile":profile,
        "original_sha256":source_sha256,
        "native_pdf":{"selection_policy":NATIVE_POLICY,
            "minimum_bidirectional_token_agreement":0.65,
            "minimum_native_token_count":10,
            "minimum_line_height_normalized":3,
            "pages":pages,
            "limitations":["Token agreement is a screening heuristic, not pixel verification.",
                "Visible signature text is not cryptographic signature verification.",
                "Native text and OCR remain uncalibrated; human review is required."]},
        "coordinate_system":"normalized_0_1000_top_left_cropbox",
        "geometry_scope":"source_block_not_individual_field",
        "blocks":blocks, "source_text_sha256":hashes, "markdown":markdown,
        "markdown_policy":"native_projection_where_screened; extraction_keeps_parallel_ocr_evidence",
        "raw_ocr_quality":{"source_blocks":ocr,"issues":raw_ocr_issues},
        "native_comparison":native_comparison,
        "issues":issues
    });
    let extraction = serde_json::to_value(extraction)
        .map_err(|_| ProcessingError::permanent("cannot serialize extraction"))?;
    Ok((post_processing, extraction))
}

fn ocr_blocks(result: &Value) -> Result<Vec<SourceBlock>, ProcessingError> {
    let mut blocks = Vec::new();
    let mut bytes = 0_usize;
    let pages = result["pages"]
        .as_array()
        .ok_or_else(|| ProcessingError::permanent("OCR result has no pages"))?;
    for page in pages {
        let page_number = page["page_number"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| ProcessingError::permanent("OCR page numbering is invalid"))?;
        if let Some(regions) = page["regions"].as_array() {
            for region in regions {
                let Some(text) = region["content"].as_str().filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let index = region["index"]
                    .as_u64()
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| ProcessingError::permanent("OCR region index is invalid"))?;
                let bbox: Option<[i32; 4]> = serde_json::from_value(region["bbox_2d"].clone()).ok();
                blocks.push(SourceBlock {
                    id: format!("ocr:p{page_number}:r{index}"),
                    page_number,
                    region_index: Some(index),
                    source: "ocr".into(),
                    text: text.into(),
                    bbox_2d: bbox,
                    detector_score: region["score"].as_f64(),
                });
                bytes = bytes.saturating_add(text.len());
            }
        } else if let Some(text) = page["markdown"].as_str().filter(|t| !t.trim().is_empty()) {
            blocks.push(SourceBlock {
                id: format!("ocr:p{page_number}"),
                page_number,
                region_index: None,
                source: "ocr".into(),
                text: text.into(),
                bbox_2d: None,
                detector_score: None,
            });
            bytes = bytes.saturating_add(text.len());
        }
        if blocks.len() > MAX_BLOCKS || bytes > MAX_TEXT_BYTES {
            return Err(ProcessingError::permanent(
                "post-processing source budget exceeded",
            ));
        }
    }
    Ok(blocks)
}

/// Recovery gets a complete, independently scoped raw-OCR stream so a local
/// crop cannot inherit a party heading from an unrelated source or page. The
/// first-pass formatted OCR remains present as a separate stream.
fn recovery_context_blocks(result: &Value) -> Result<Vec<SourceBlock>, ProcessingError> {
    let observations: BTreeMap<(u64, u64), &Value> = result["recovery"]["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["status"] == "observed")
        .filter_map(|v| {
            Some((
                (
                    v["page_number"].as_u64()?,
                    v["detector_region_index"].as_u64()?,
                ),
                v,
            ))
        })
        .collect();
    if observations.is_empty() {
        return Ok(Vec::new());
    }
    if observations.len() > 16 {
        return Err(ProcessingError::permanent(
            "recovery observation budget exceeded",
        ));
    }
    let mut blocks = Vec::new();
    let mut bytes = 0usize;
    for page in result["pages"].as_array().into_iter().flatten() {
        let page_number = page["page_number"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| ProcessingError::permanent("invalid recovery page"))?;
        let mut raw: Vec<&Value> = page["raw_regions"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        raw.sort_by_key(|v| {
            (
                v["reading_order"].as_u64().unwrap_or(u64::MAX),
                v["index"].as_u64().unwrap_or(u64::MAX),
            )
        });
        for region in raw {
            let index = region["index"]
                .as_u64()
                .and_then(|p| u32::try_from(p).ok())
                .ok_or_else(|| ProcessingError::permanent("invalid recovery region"))?;
            let observation = observations.get(&(u64::from(page_number), u64::from(index)));
            let selected = observation.copied().unwrap_or(region);
            let text = if observation.is_some() {
                selected["content"].as_str()
            } else {
                region["raw_content"]
                    .as_str()
                    .or_else(|| region["content"].as_str())
            };
            let Some(text) = text.filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            let bbox: [i32; 4] = serde_json::from_value(selected["bbox_2d"].clone())
                .map_err(|_| ProcessingError::permanent("invalid recovery coordinates"))?;
            bytes = bytes.saturating_add(text.len());
            if blocks.len() >= MAX_BLOCKS || bytes > MAX_TEXT_BYTES {
                return Err(ProcessingError::permanent(
                    "recovery context source budget exceeded",
                ));
            }
            blocks.push(SourceBlock {
                id: format!("recovery:p{page_number}:r{index}"),
                page_number,
                region_index: Some(index),
                source: "ocr_recovery_context".into(),
                text: text.into(),
                bbox_2d: Some(bbox),
                // Expanded crops were not measured by the layout detector.
                detector_score: if observation.is_some() {
                    None
                } else {
                    region["score"].as_f64()
                },
            });
        }
    }
    Ok(blocks)
}

#[derive(Clone)]
struct Line {
    text: String,
    bbox: [i32; 4],
}

pub(crate) fn parse_native_xml(xml: &str) -> Result<Vec<SourceBlock>, ProcessingError> {
    // Poppler's fixed external XHTML declaration has no internal subset. Drop
    // that exact declaration; all other DTD/entity declarations stay rejected.
    let xml = xml.strip_prefix(POPPLER_DOCTYPE).unwrap_or(xml);
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            ..Default::default()
        },
    )
    .map_err(|_| ProcessingError::permanent("invalid native PDF geometry XML"))?;
    let mut blocks = Vec::new();
    let mut bytes = 0_usize;
    for (page_index, page) in doc
        .descendants()
        .filter(|n| n.has_tag_name("page"))
        .enumerate()
    {
        let width = number(page, "width")?;
        let height = number(page, "height")?;
        if width <= 0.0 || height <= 0.0 {
            return Err(ProcessingError::permanent(
                "invalid native PDF page dimensions",
            ));
        }
        let mut lines = Vec::new();
        for line in page.descendants().filter(|n| n.has_tag_name("line")) {
            let coords = [
                number(line, "xMin")? / width,
                number(line, "yMin")? / height,
                number(line, "xMax")? / width,
                number(line, "yMax")? / height,
            ];
            if coords.iter().any(|c| !(0.0..=1.0).contains(c))
                || coords[0] >= coords[2]
                || coords[1] >= coords[3]
            {
                continue;
            }
            let bbox = coords.map(|c| (c * 1000.0).round() as i32);
            // Tiny invisible signature anchors were observed in real input.
            // This cannot detect all hidden text; review remains mandatory.
            if bbox[3] - bbox[1] < 3 {
                continue;
            }
            let text = line
                .children()
                .filter(|n| n.has_tag_name("word"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ");
            if text.trim().is_empty() || text.chars().any(|c| c == '\u{fffd}' || c.is_control()) {
                continue;
            }
            lines.push(Line { text, bbox });
        }
        lines.sort_by_key(|l| (l.bbox[1], l.bbox[0]));
        let mut rows: Vec<Vec<Line>> = Vec::new();
        for line in lines {
            if let Some(row) = rows.last_mut() {
                let anchor = &row[0];
                let overlap =
                    (anchor.bbox[3].min(line.bbox[3]) - anchor.bbox[1].max(line.bbox[1])).max(0);
                let min_height = (anchor.bbox[3] - anchor.bbox[1]).min(line.bbox[3] - line.bbox[1]);
                if overlap * 4 >= min_height * 3 {
                    row.push(line);
                    continue;
                }
            }
            rows.push(vec![line]);
        }
        for (row_index, mut row) in rows.into_iter().enumerate() {
            row.sort_by_key(|l| l.bbox[0]);
            let text = row
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let bbox = row.iter().fold([1000, 1000, 0, 0], |b, l| {
                [
                    b[0].min(l.bbox[0]),
                    b[1].min(l.bbox[1]),
                    b[2].max(l.bbox[2]),
                    b[3].max(l.bbox[3]),
                ]
            });
            bytes = bytes.saturating_add(text.len());
            blocks.push(SourceBlock {
                id: format!("native:p{}:row{row_index}", page_index + 1),
                page_number: (page_index + 1) as u32,
                region_index: None,
                source: "native_pdf".into(),
                text,
                bbox_2d: Some(bbox),
                detector_score: None,
            });
            if blocks.len() > MAX_BLOCKS || bytes > MAX_TEXT_BYTES {
                return Err(ProcessingError::permanent(
                    "native PDF text budget exceeded",
                ));
            }
        }
    }
    Ok(blocks)
}

fn number(node: roxmltree::Node<'_, '_>, name: &str) -> Result<f64, ProcessingError> {
    node.attribute(name)
        .and_then(|n| n.parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .ok_or_else(|| ProcessingError::permanent("invalid native PDF geometry number"))
}

fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// These are text-layer alternatives, not verified corrections. Their source
/// references let a reviewer compare them with unchanged image OCR.
fn compare_sources(ocr: &[SourceBlock], native: &[SourceBlock]) -> Value {
    // A dense mixed page can geometrically overlap every OCR/native pair.
    // Bound comparison before candidate-reference arrays are materialized.
    if ocr.len().saturating_mul(native.len()) > 100_000 {
        return json!({"comparison_policy":"normalized_geometry_and_token_sets_v1",
            "comparison_budget_exceeded":true,"review_required":true,
            "alternatives":[],"uncovered_native_blocks":[],"native_blocks":native,
            "automatic_raw_text_rewrites":0});
    }
    let mut alternatives = Vec::new();
    let mut uncovered = Vec::new();
    for row in native {
        let Some(n) = row.bbox_2d else { continue };
        let area = (n[2] - n[0]) * (n[3] - n[1]);
        let covered = ocr
            .iter()
            .filter(|b| b.page_number == row.page_number)
            .filter_map(|b| b.bbox_2d)
            .any(|o| {
                let overlap = (n[2].min(o[2]) - n[0].max(o[0])).max(0)
                    * (n[3].min(o[3]) - n[1].max(o[1])).max(0);
                overlap * 2 >= area
            });
        if !covered {
            uncovered.push(json!({"block_id":row.id,"page_number":row.page_number,
            "text":row.text,"bbox_2d":n,"pixel_verified":false,"review_required":true}));
        }
    }
    for block in ocr {
        let Some(o) = block.bbox_2d else { continue };
        let rows: Vec<_> = native
            .iter()
            .filter(|b| b.page_number == block.page_number)
            .filter(|b| {
                b.bbox_2d.is_some_and(|n| {
                    let x = (n[0] + n[2]) / 2;
                    let y = (n[1] + n[3]) / 2;
                    (o[0] - 2..=o[2] + 2).contains(&x) && (o[1] - 2..=o[3] + 2).contains(&y)
                })
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        let text = rows
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if tokens(&text) != tokens(&block.text) {
            alternatives.push(json!({"ocr_block_id":block.id,"native_block_ids":rows.iter().map(|b|&b.id).collect::<Vec<_>>(),
                "page_number":block.page_number,"pixel_verified":false,"review_required":true,
                "reason":"geometry_matched_text_layer_differs; formatting, OCR, or native-layer errors remain possible"}));
        }
    }
    json!({"comparison_policy":"normalized_geometry_and_token_sets_v1","comparison_budget_exceeded":false,
        "alternatives":alternatives,"uncovered_native_blocks":uncovered,
        "native_blocks":native,"automatic_raw_text_rewrites":0})
}

fn select_sources(ocr: &[SourceBlock], native: &[SourceBlock]) -> (Vec<SourceBlock>, Vec<Value>) {
    let pages: std::collections::BTreeSet<_> =
        ocr.iter().chain(native).map(|b| b.page_number).collect();
    let mut blocks = Vec::new();
    let mut assessments = Vec::new();
    for page in pages {
        let o: Vec<_> = ocr.iter().filter(|b| b.page_number == page).collect();
        let n: Vec<_> = native.iter().filter(|b| b.page_number == page).collect();
        let ot = tokens(
            &o.iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
        let nt = tokens(
            &n.iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
        let common = ot.intersection(&nt).count();
        let native_agreement = common as f64 / nt.len().max(1) as f64;
        let ocr_agreement = common as f64 / ot.len().max(1) as f64;
        let selected = nt.len() >= 10 && native_agreement >= 0.65 && ocr_agreement >= 0.65;
        assessments.push(
            json!({"page_number":page,"selected_source":if selected {"native_pdf_and_ocr"}else{"ocr"},
            "native_unique_tokens":nt.len(),"ocr_unique_tokens":ot.len(),
            "native_token_agreement":native_agreement,"ocr_token_agreement":ocr_agreement,
            "confidence":null,"pixel_verified":false,
            "review_required":true}),
        );
        if selected {
            for block in n {
                blocks.push(block.clone());
            }
        }
        for block in o {
            blocks.push(block.clone());
        }
    }
    (blocks, assessments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_joins_label_and_value_but_filters_tiny_anchors() {
        let blocks=parse_native_xml(r#"<doc><page width="100" height="100"><line xMin="2" yMin="10" xMax="20" yMax="12"><word>Ngày sinh:</word></line><line xMin="30" yMin="10" xMax="70" yMax="12"><word>01/01/2000</word></line><line xMin="2" yMin="80" xMax="20" yMax="80.1"><word>SIGN_A</word></line></page></doc>"#).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "Ngày sinh: 01/01/2000");
        assert_eq!(blocks[0].bbox_2d, Some([20, 100, 700, 120]));
        assert_eq!(blocks[0].page_number, 1);
        assert_eq!(blocks[0].region_index, None);
    }

    #[test]
    fn parser_rejects_entities_and_invalid_numbers_without_fetching() {
        assert!(
            parse_native_xml(
                "<!DOCTYPE doc [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><doc>&x;</doc>"
            )
            .is_err()
        );
        assert!(parse_native_xml("<doc><page width='NaN' height='20'/></doc>").is_err());
        assert!(parse_native_xml("<doc><page width='0' height='20'/></doc>").is_err());
    }

    #[test]
    fn sparse_text_layer_cannot_override_image_card_ocr() {
        fn block(text: &str, source: &str) -> SourceBlock {
            SourceBlock {
                id: source.into(),
                page_number: 1,
                region_index: None,
                source: source.into(),
                text: text.into(),
                bbox_2d: None,
                detector_score: None,
            }
        }
        let ocr = vec![block(
            "CĂN CƯỚC CÔNG DÂN citizen identity card full name birth date place of residence issue date valid until",
            "ocr",
        )];
        let native = vec![block("CĂN CƯỚC CÔNG DÂN", "native_pdf")];
        let (selected, assessments) = select_sources(&ocr, &native);
        assert_eq!(selected[0].source, "ocr");
        assert_eq!(assessments[0]["selected_source"], "ocr");
    }

    #[test]
    fn mixed_pages_keep_unique_ocr_fields_and_both_disagreeing_observations() {
        let common =
            "hợp đồng thuê nhà điều khoản thanh toán bàn giao quyền nghĩa vụ trách nhiệm thời hạn";
        let ocr = vec![SourceBlock {
            id: "ocr".into(),
            page_number: 1,
            region_index: Some(0),
            source: "ocr".into(),
            text: format!("{common}\nNgày sinh: 01/01/2000\nUniqueEmbeddedImageValue"),
            bbox_2d: None,
            detector_score: None,
        }];
        let native = vec![SourceBlock {
            id: "native".into(),
            page_number: 1,
            region_index: None,
            source: "native_pdf".into(),
            text: format!("{common}\nNgày sinh: 02/01/2000"),
            bbox_2d: None,
            detector_score: None,
        }];
        let (blocks, assessment) = select_sources(&ocr, &native);
        assert_eq!(assessment[0]["selected_source"], "native_pdf_and_ocr");
        assert_eq!(blocks.len(), 2);
        assert!(
            blocks
                .iter()
                .any(|b| b.text.contains("UniqueEmbeddedImageValue"))
        );
        assert!(blocks.iter().any(|b| b.text.contains("02/01/2000")));
    }

    #[test]
    fn dense_geometry_comparison_fails_closed_before_reference_expansion() {
        let block = SourceBlock {
            id: "b".into(),
            page_number: 1,
            region_index: None,
            source: "ocr".into(),
            text: "test".into(),
            bbox_2d: Some([0, 0, 1000, 1000]),
            detector_score: None,
        };
        let result = compare_sources(&vec![block.clone(); 400], &vec![block; 400]);
        assert_eq!(result["comparison_budget_exceeded"], true);
        assert_eq!(result["automatic_raw_text_rewrites"], 0);
        assert_eq!(result["alternatives"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn raw_layout_is_immutable_and_unsupported_profiles_fail() {
        let mut result = json!({"pages":[{"page_number":1,"regions":[{"index":0,"content":"Ngày sinh: #","bbox_2d":[1,2,3,4],"score":0.57}]}],"layout":[[{"content":"Ngày sinh: #"}]],"markdown":"Ngày sinh: #"});
        let original = result.clone();
        apply(
            &mut result,
            "image/png",
            "vi_rental_contract_v1",
            None,
            "a".repeat(64),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["layout"], original["layout"]);
        assert_eq!(result["markdown"], original["markdown"]);
        assert!(
            result["post_processing"]["blocks"][0]["text"]
                .as_str()
                .unwrap()
                .contains('#')
        );
        assert!(
            apply(
                &mut result,
                "image/png",
                "invented",
                None,
                "a".repeat(64),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_pdf_parser_is_retryable_not_a_success_warning() {
        let mut result = json!({"pages": []});
        let error = apply(
            &mut result,
            "application/pdf",
            "vi_rental_contract_v1",
            None,
            "a".repeat(64),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.retryable);
        assert_eq!(error.message, "isolated parser is unavailable");
    }
}
