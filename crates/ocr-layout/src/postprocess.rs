//! PP-DocLayoutV3 output decoding and the production-ocr-course layout policy.
//!
//! Derived from the Apache-2.0 PaddlePaddle/Hugging Face V3 image processor
//! and GLM-OCR SDK 0.1.4 `layout_postprocess_utils.py`. The learned pairwise
//! reading order is part of V3; replacing it with page-coordinate sorting
//! changes the recognition pipeline. See THIRD_PARTY_NOTICES.md.

use anyhow::{Context, Result, ensure};

use crate::{CLASSES, PageLayout, QUERIES, Region, Task, polygon};

const THRESHOLD: f32 = 0.3;
const MASK_SIDE: usize = 200;
const MASK_PIXELS: usize = MASK_SIDE * MASK_SIDE;
// The course page-loader's max_pixels. Bounds also prevent oversized masks
// from allocating unbounded contour working storage.
const MAX_PAGE_PIXELS: u64 = 71_372_800;
pub const LABELS: [&str; CLASSES] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

#[derive(Clone, Debug)]
struct Detection {
    query: usize,
    class: usize,
    score: f32,
    bbox: [f32; 4],
    rank: u32,
}

pub fn postprocess(
    width: u32,
    height: u32,
    logits: &[f32],
    boxes: &[f32],
    order_logits: &[f32],
    masks: &[f32],
) -> Result<PageLayout> {
    ensure!(
        width > 0 && height > 0,
        "layout page dimensions must be positive"
    );
    ensure!(
        width <= 32_768
            && height <= 32_768
            && u64::from(width) * u64::from(height) <= MAX_PAGE_PIXELS,
        "layout page dimensions exceed the course image bounds"
    );
    validate_tensor("logits", logits, QUERIES * CLASSES)?;
    validate_tensor("boxes", boxes, QUERIES * 4)?;
    validate_tensor("order_logits", order_logits, QUERIES * QUERIES)?;
    validate_tensor("masks", masks, QUERIES * MASK_PIXELS)?;
    ensure!(
        boxes.iter().all(|value| (0.0..=1.0).contains(value)),
        "layout predicted boxes must contain normalized values in [0, 1]"
    );

    let ranks = reading_order(order_logits);
    // The SDK masks all class logits for boxes no larger than a mask pixel
    // before Transformers chooses its top 300 query/class pairs.
    let mut pairs = Vec::with_capacity(QUERIES * CLASSES);
    for query in 0..QUERIES {
        let valid = boxes[query * 4 + 2] > 1.0 / MASK_SIDE as f32
            && boxes[query * 4 + 3] > 1.0 / MASK_SIDE as f32;
        for class in 0..CLASSES {
            let value = if valid {
                logits[query * CLASSES + class]
            } else {
                -100.0
            };
            pairs.push((sigmoid(value), query, class));
        }
    }
    pairs.sort_unstable_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    pairs.truncate(QUERIES);
    let mut detections: Vec<_> = pairs
        .into_iter()
        .filter(|pair| pair.0 >= THRESHOLD)
        .map(|(score, query, class)| {
            let b = &boxes[query * 4..query * 4 + 4];
            Detection {
                query,
                class,
                score,
                rank: ranks[query],
                bbox: [
                    (b[0] - 0.5 * b[2]) * width as f32,
                    (b[1] - 0.5 * b[3]) * height as f32,
                    (b[0] + 0.5 * b[2]) * width as f32,
                    (b[1] + 0.5 * b[3]) * height as f32,
                ],
            }
        })
        .collect();

    // Transformers first sorts selected query/class pairs by learned order.
    // NMS then sorts them by confidence as the SDK does.
    detections.sort_by_key(|detection| detection.rank);
    let original_detections = detections.clone();
    detections = nms(detections);
    filter_large_image(&mut detections, width, height);
    detections = merge_course_boxes(detections);
    detections.sort_by_key(|detection| detection.rank);

    let mut regions = Vec::new();
    let mut discarded = Vec::new();
    for detection in detections {
        let clamped = [
            detection.bbox[0].clamp(0.0, width as f32),
            detection.bbox[1].clamp(0.0, height as f32),
            detection.bbox[2].clamp(0.0, width as f32),
            detection.bbox[3].clamp(0.0, height as f32),
        ];
        if clamped[0] >= clamped[2] || clamped[1] >= clamped[3] {
            continue;
        }
        // GLM-OCR truncates clamped pixel coordinates to int before bbox
        // normalization, but retains floating mask vertices until normalization.
        let bbox_2d = [
            normalize(clamped[0] as i32 as f64, width),
            normalize(clamped[1] as i32 as f64, height),
            normalize(clamped[2] as i32 as f64, width),
            normalize(clamped[3] as i32 as f64, height),
        ];
        // The SDK restores polygons by matching coordinates to the first
        // original detection, with numpy.allclose(atol=1, rtol=1e-5).
        let polygon_detection = original_detections
            .iter()
            .find(|original| boxes_allclose(original.bbox, detection.bbox))
            .context("layout polygon lost its original query association")?;
        let mask = &masks
            [polygon_detection.query * MASK_PIXELS..(polygon_detection.query + 1) * MASK_PIXELS];
        let (mut points, source) =
            polygon::extract_polygon(mask, polygon_detection.bbox, width, height, THRESHOLD)?;
        if source == "empty_resized_mask_rectangle" {
            points = vec![
                [clamped[0], clamped[1]],
                [clamped[2], clamped[1]],
                [clamped[2], clamped[3]],
                [clamped[0], clamped[3]],
            ];
        }
        let polygon = points
            .into_iter()
            .map(|point| {
                [
                    normalize(point[0].clamp(0.0, width as f32) as f64, width),
                    normalize(point[1].clamp(0.0, height as f32) as f64, height),
                ]
            })
            .collect();
        let task = task_for_class(detection.class);
        let index = if task == Task::Abandon {
            discarded.len()
        } else {
            regions.len()
        } as u32;
        let region = Region {
            index,
            class_id: detection.class as u32,
            label: LABELS[detection.class].to_owned(),
            task,
            bbox: detection.bbox,
            bbox_2d,
            score: detection.score,
            reading_order: detection.rank,
            polygon,
            polygon_source: source.to_owned(),
        };
        if task == Task::Abandon {
            discarded.push(region);
        } else {
            regions.push(region);
        }
    }
    Ok(PageLayout {
        width,
        height,
        regions,
        discarded,
    })
}

fn validate_tensor(name: &str, values: &[f32], expected: usize) -> Result<()> {
    ensure!(
        values.len() == expected,
        "layout {name} tensor has invalid length: expected {expected}, received {}",
        values.len()
    );
    ensure!(
        values.iter().all(|value| value.is_finite()),
        "layout {name} tensor contains a non-finite value"
    );
    Ok(())
}

pub(crate) fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

fn normalize(pixel: f64, dimension: u32) -> i32 {
    (pixel / f64::from(dimension) * 1000.0) as i32
}

pub fn task_for_class(class: usize) -> Task {
    match class {
        5 | 15 => Task::Formula,
        21 => Task::Table,
        3 | 14 => Task::Skip,
        2 | 8 | 9 | 10 | 12 | 13 | 16 | 18 => Task::Abandon,
        _ => Task::Text,
    }
}

fn reading_order(logits: &[f32]) -> [u32; QUERIES] {
    let mut votes = [0.0_f32; QUERIES];
    for i in 0..QUERIES {
        // The upstream expression sums triu(scores) down rows and sums
        // tril(1 - scores.transpose()) down rows, using only upper triangles.
        let before: f32 = (0..i).map(|j| sigmoid(logits[j * QUERIES + i])).sum();
        let after: f32 = (i + 1..QUERIES)
            .map(|j| 1.0 - sigmoid(logits[i * QUERIES + j]))
            .sum();
        votes[i] = before + after;
    }
    let mut pointers: Vec<usize> = (0..QUERIES).collect();
    pointers.sort_unstable_by(|&a, &b| votes[a].total_cmp(&votes[b]).then_with(|| a.cmp(&b)));
    let mut ranks = [0; QUERIES];
    for (rank, query) in pointers.into_iter().enumerate() {
        ranks[query] = rank as u32;
    }
    ranks
}

fn inclusive_iou(a: [f32; 4], b: [f32; 4]) -> f64 {
    let a = a.map(f64::from);
    let b = b.map(f64::from);
    let overlap = (a[2].min(b[2]) - a[0].max(b[0]) + 1.0).max(0.0)
        * (a[3].min(b[3]) - a[1].max(b[1]) + 1.0).max(0.0);
    let area_a = (a[2] - a[0] + 1.0) * (a[3] - a[1] + 1.0);
    let area_b = (b[2] - b[0] + 1.0) * (b[3] - b[1] + 1.0);
    overlap / (area_a + area_b - overlap)
}

fn nms(mut detections: Vec<Detection>) -> Vec<Detection> {
    detections.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut selected: Vec<Detection> = Vec::with_capacity(detections.len());
    for detection in detections {
        if selected.iter().all(|existing| {
            let threshold = if existing.class == detection.class {
                0.6
            } else {
                0.98
            };
            inclusive_iou(existing.bbox, detection.bbox) < threshold
        }) {
            selected.push(detection);
        }
    }
    selected
}

fn filter_large_image(detections: &mut Vec<Detection>, width: u32, height: u32) {
    if detections.len() <= 1 {
        return;
    }
    let threshold = if width > height { 0.82 } else { 0.93 };
    let page_area = f64::from(width) * f64::from(height);
    let retain = |detection: &Detection| {
        if detection.class != 14 {
            return true;
        }
        let b = detection.bbox.map(f64::from);
        let area = (b[2].min(f64::from(width)) - b[0].max(0.0))
            * (b[3].min(f64::from(height)) - b[1].max(0.0));
        area <= threshold * page_area
    };
    // The SDK deliberately retains the input if this filter would empty it.
    if detections.iter().any(retain) {
        detections.retain(retain);
    }
}

fn contained(a: &Detection, b: &Detection) -> bool {
    let a = a.bbox.map(f64::from);
    let b = b.bbox.map(f64::from);
    let area = (a[2] - a[0]) * (a[3] - a[1]);
    let overlap =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    area > 0.0 && overlap / area >= 0.8
}

fn boxes_allclose(a: [f32; 4], b: [f32; 4]) -> bool {
    a.into_iter()
        .zip(b)
        .all(|(a, b)| (f64::from(a) - f64::from(b)).abs() <= 1.0 + 1e-5 * f64::from(b).abs())
}

fn merge_course_boxes(detections: Vec<Detection>) -> Vec<Detection> {
    let mut keep = vec![true; detections.len()];
    // Match the per-class dictionary's independent checks against the whole
    // NMS output, combining its masks only after every category was evaluated.
    for category in 0..CLASSES {
        let mut contains_other = vec![false; detections.len()];
        let mut contained_by_other = vec![false; detections.len()];
        for (i, a) in detections.iter().enumerate() {
            if matches!(a.class, 3 | 14 | 20) {
                continue;
            }
            for (j, b) in detections.iter().enumerate() {
                if i == j {
                    continue;
                }
                let applies = if category == 18 {
                    a.class == category
                } else {
                    b.class == category
                };
                if applies && contained(a, b) {
                    contained_by_other[i] = true;
                    contains_other[j] = true;
                }
            }
        }
        for i in 0..detections.len() {
            keep[i] &= if category == 18 {
                !contains_other[i] || contained_by_other[i]
            } else {
                !contained_by_other[i]
            };
        }
    }
    detections
        .into_iter()
        .zip(keep)
        .filter_map(|(detection, keep)| keep.then_some(detection))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(class: usize, score: f32, bbox: [f32; 4]) -> Detection {
        Detection {
            query: class,
            class,
            score,
            bbox,
            rank: class as u32,
        }
    }

    #[test]
    fn nms_uses_different_thresholds_between_classes() {
        let bbox = [10.0, 10.0, 110.0, 110.0];
        let shifted = [20.0, 10.0, 120.0, 110.0];
        assert_eq!(
            nms(vec![detection(22, 0.9, bbox), detection(22, 0.8, shifted)]).len(),
            1
        );
        assert_eq!(
            nms(vec![detection(22, 0.9, bbox), detection(21, 0.8, shifted)]).len(),
            2
        );
        assert_eq!(
            nms(vec![detection(22, 0.9, bbox), detection(21, 0.8, bbox)]).len(),
            1
        );
    }

    #[test]
    fn merge_preserves_graphics_and_prefers_small_reference_boxes() {
        let outer = [0.0, 0.0, 100.0, 100.0];
        let inner = [20.0, 20.0, 70.0, 70.0];
        let result = merge_course_boxes(vec![detection(22, 0.9, outer), detection(3, 0.8, inner)]);
        assert_eq!(result.len(), 2);
        let result = merge_course_boxes(vec![detection(22, 0.9, outer), detection(18, 0.8, inner)]);
        assert!(result.is_empty()); // Independent large(text)/small(reference) masks intersect.
        let result = merge_course_boxes(vec![detection(18, 0.9, outer), detection(22, 0.8, inner)]);
        assert_eq!(result.len(), 2); // A reference container does not discard its text child.
    }

    #[test]
    fn learned_order_can_reverse_geometric_query_order() {
        let mut logits = vec![20.0; QUERIES * QUERIES];
        // Scores near zero make lower-index queries receive more votes and
        // therefore rank later. The lower triangle is intentionally ignored.
        for i in 0..QUERIES {
            for j in i + 1..QUERIES {
                logits[i * QUERIES + j] = -20.0;
            }
        }
        let ranks = reading_order(&logits);
        assert_eq!(ranks[0], 299);
        assert_eq!(ranks[299], 0);
    }

    #[test]
    fn malformed_tensors_and_nonfinite_values_fail_closed() {
        assert!(postprocess(10, 10, &[], &[], &[], &[]).is_err());
        assert!(validate_tensor("test", &[f32::NAN], 1).is_err());
        assert!(validate_tensor("test", &[f32::INFINITY], 1).is_err());
        assert!(postprocess(0, 10, &[], &[], &[], &[]).is_err());
        assert!(postprocess(100_000, 100_000, &[], &[], &[], &[]).is_err());
    }

    #[test]
    fn pixel_quantization_precedes_bbox_normalization() {
        assert_eq!(normalize(123.9_f32 as i32 as f64, 333), 369);
        assert_eq!(normalize(123.9_f32 as f64, 333), 372);
        assert_eq!(task_for_class(14), Task::Skip);
        assert_eq!(task_for_class(13), Task::Abandon);
        assert_eq!(task_for_class(11), Task::Text);
    }

    #[test]
    fn complete_course_policy_preserves_learned_order_and_tasks() {
        let mut logits = vec![-20.0; QUERIES * CLASSES];
        let mut boxes = vec![0.5; QUERIES * 4];
        let orders = vec![20.0; QUERIES * QUERIES];
        let mut masks = vec![-20.0; QUERIES * MASK_PIXELS];
        let mut add = |query: usize, class: usize, confidence: f32, bbox: [f32; 4]| {
            logits[query * CLASSES + class] = confidence;
            boxes[query * 4..query * 4 + 4].copy_from_slice(&bbox);
        };
        add(0, 21, 4.0, [0.8, 0.8, 0.2, 0.2]); // Bottom table precedes top text.
        add(5, 22, 3.0, [0.2, 0.2, 0.1, 0.1]);
        add(6, 12, 3.0, [0.9, 0.1, 0.1, 0.1]);
        add(7, 14, 5.0, [0.5, 0.5, 1.0, 1.0]); // Page-spanning image is removed.
        add(8, 14, 3.0, [0.2, 0.8, 0.1, 0.1]); // Small image remains a skip region.
        add(9, 20, 3.0, [0.8, 0.8, 0.06, 0.06]); // Seal is protected from containment.
        add(10, 22, 3.0, [0.8, 0.8, 0.1, 0.1]); // Nested text is discarded by large(table).
        add(11, 18, 3.0, [0.7, 0.3, 0.2, 0.2]);
        add(12, 22, 3.0, [0.7, 0.3, 0.1, 0.1]); // Reference container keeps its text child.
        add(13, 22, 20.0, [0.5, 0.1, 0.005, 0.1]); // SDK prefilter excludes one-mask-pixel width.
        let result = postprocess(1000, 1000, &logits, &boxes, &orders, &masks).unwrap();
        assert_eq!(
            result
                .regions
                .iter()
                .map(|region| region.class_id)
                .collect::<Vec<_>>(),
            vec![21, 22, 14, 20, 22]
        );
        assert_eq!(
            result
                .regions
                .iter()
                .map(|region| region.reading_order)
                .collect::<Vec<_>>(),
            vec![0, 5, 8, 9, 12]
        );
        assert_eq!(
            result
                .regions
                .iter()
                .map(|region| region.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(result.regions[2].task, Task::Skip);
        assert_eq!(result.regions[0].task, Task::Table);
        assert_eq!(
            result
                .discarded
                .iter()
                .map(|region| region.label.as_str())
                .collect::<Vec<_>>(),
            vec!["header", "reference"]
        );
        assert!(
            result
                .regions
                .iter()
                .all(|region| region.polygon_source == "empty_mask_rectangle")
        );
        assert_eq!(result.regions[0].bbox_2d, [700, 700, 900, 900]);
        assert!(
            postprocess(
                1000,
                1000,
                &logits,
                &boxes,
                &orders,
                &masks[..masks.len() - 1]
            )
            .is_err()
        );
        masks[0] = f32::NAN;
        assert!(postprocess(1000, 1000, &logits, &boxes, &orders, &masks).is_err());
    }

    #[test]
    fn top_300_is_over_query_class_pairs_before_suppression() {
        let mut logits = vec![-20.0; QUERIES * CLASSES];
        let mut boxes = vec![0.0; QUERIES * 4];
        for query in 0..QUERIES {
            let x = 0.025 + (query % 20) as f32 * 0.05;
            let y = 0.025 + (query / 20) as f32 * 0.06;
            boxes[query * 4..query * 4 + 4].copy_from_slice(&[x, y, 0.02, 0.02]);
            logits[query * CLASSES + 22] = if query == 299 { 3.0 } else { 4.0 };
        }
        // Query zero consumes two of the 300 candidate slots. Its duplicate
        // class is suppressed afterward, so query 299 cannot refill that slot.
        logits[5] = 6.0;
        logits[22] = 5.0;
        let orders = vec![20.0; QUERIES * QUERIES];
        let masks = vec![-20.0; QUERIES * MASK_PIXELS];
        let result = postprocess(1000, 1000, &logits, &boxes, &orders, &masks).unwrap();
        assert_eq!(result.regions.len(), 299);
        assert_eq!(result.regions[0].task, Task::Formula);
        assert_eq!(result.regions.last().unwrap().reading_order, 298);
    }
}
