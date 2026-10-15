//! Rust counterpart of PP-DocLayoutV3's mask polygons. Mask thresholding,
//! crop/resize, and custom vertices follow the Apache-2.0 PaddlePaddle and
//! Hugging Face image processor. Contours use imageproc's Suzuki/Abe tracer.
//!
//! Closed Douglas-Peucker initialization and cleanup are adapted from
//! OpenCV `modules/imgproc/src/approx.cpp` (Intel license):
//! Copyright (C) 2000, Intel Corporation, all rights reserved.
//! Third party copyrights are property of their respective owners.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are met:
//! * Redistributions of source code must retain the above copyright notice,
//!   this list of conditions and the following disclaimer.
//! * Redistributions in binary form must reproduce the above copyright notice,
//!   this list of conditions and the following disclaimer in the documentation
//!   and/or other materials provided with the distribution.
//! * The name of Intel Corporation may not be used to endorse or promote
//!   products derived from this software without specific prior written permission.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
//! IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
//! ARE DISCLAIMED. IN NO EVENT SHALL THE INTEL CORPORATION OR CONTRIBUTORS BE
//! LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
//! CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
//! SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
//! INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
//! CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
//! ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
//! POSSIBILITY OF SUCH DAMAGE.

use anyhow::{Result, ensure};
use image::{GrayImage, Luma};
use imageproc::contours::{BorderType, find_contours};

const SIDE: usize = 200;
type Point = [i32; 2];

pub(crate) fn extract_polygon(
    logits: &[f32],
    bbox: [f32; 4],
    width: u32,
    height: u32,
    threshold: f32,
) -> Result<(Vec<[f32; 2]>, &'static str)> {
    ensure!(
        logits.len() == SIDE * SIDE,
        "invalid single-query layout mask length"
    );
    let [x_min, y_min, x_max, y_max] = bbox.map(|value| value as i32);
    let box_width = x_max - x_min;
    let box_height = y_max - y_min;
    let rect = || {
        vec![
            [x_min as f32, y_min as f32],
            [x_max as f32, y_min as f32],
            [x_max as f32, y_max as f32],
            [x_min as f32, y_max as f32],
        ]
    };
    if box_width <= 0 || box_height <= 0 {
        return Ok((rect(), "degenerate_mask_rectangle"));
    }
    // The processor divides its fixed 800px resize scale by four. Use f32
    // arithmetic and Python's ties-to-even round before clipping to 200px.
    let scale_width = (800.0_f32 / width as f32) / 4.0;
    let scale_height = (800.0_f32 / height as f32) / 4.0;
    let mask_coordinate = |pixel: i32, scale: f32| {
        (pixel as f32 * scale)
            .round_ties_even()
            .clamp(0.0, SIDE as f32) as usize
    };
    let x_start = mask_coordinate(x_min, scale_width);
    let x_end = mask_coordinate(x_max, scale_width);
    let y_start = mask_coordinate(y_min, scale_height);
    let y_end = mask_coordinate(y_max, scale_height);
    if x_start >= x_end || y_start >= y_end {
        return Ok((rect(), "empty_mask_rectangle"));
    }
    let crop_width = x_end - x_start;
    let crop_height = y_end - y_start;
    let mut crop = vec![0_u8; crop_width * crop_height];
    let mut any_foreground = false;
    for y in 0..crop_height {
        for x in 0..crop_width {
            let value = u8::from(
                crate::postprocess::sigmoid(logits[(y + y_start) * SIDE + x + x_start]) > threshold,
            );
            crop[y * crop_width + x] = value;
            any_foreground |= value != 0;
        }
    }
    if !any_foreground {
        return Ok((rect(), "empty_mask_rectangle"));
    }

    // Padding makes imageproc trace objects touching the image edge exactly as
    // cv2.findContours does. INTER_NEAREST maps to floor(dst * src / dst_size).
    let mut resized = GrayImage::new(box_width as u32 + 2, box_height as u32 + 2);
    for y in 0..box_height as usize {
        let source_y = y * crop_height / box_height as usize;
        for x in 0..box_width as usize {
            let source_x = x * crop_width / box_width as usize;
            resized.put_pixel(
                x as u32 + 1,
                y as u32 + 1,
                Luma([crop[source_y * crop_width + source_x]]),
            );
        }
    }
    let points = largest_external_contour(&resized);
    let Some(points) = points else {
        // A nonempty crop can lose every foreground pixel when downsampled.
        // Upstream returns None here; the SDK then supplies the floating
        // clamped detection box (rather than the extractor's integer rect).
        return Ok((
            vec![
                [bbox[0], bbox[1]],
                [bbox[2], bbox[1]],
                [bbox[2], bbox[3]],
                [bbox[0], bbox[3]],
            ],
            "empty_resized_mask_rectangle",
        ));
    };
    let simple = chain_approx_simple(&points);
    let epsilon = 0.004 * arc_length(&simple);
    let approximate = approximate_closed_polygon(&simple, epsilon);
    let vertices = custom_vertices(&approximate);
    if vertices.len() < 4 {
        return Ok((rect(), "degenerate_mask_rectangle"));
    }
    Ok((
        vertices
            .into_iter()
            .map(|point| {
                [
                    (point[0] + f64::from(x_min)) as f32,
                    (point[1] + f64::from(y_min)) as f32,
                ]
            })
            .collect(),
        "mask",
    ))
}

fn largest_external_contour(image: &GrayImage) -> Option<Vec<Point>> {
    // cv2 RETR_EXTERNAL enumerates top-level contours in reverse scan order.
    // Choosing the last equal-area contour reproduces max()'s tie behavior.
    find_contours::<i32>(image)
        .into_iter()
        .filter(|contour| contour.border_type == BorderType::Outer && contour.parent.is_none())
        .map(|contour| {
            contour
                .points
                .into_iter()
                .map(|point| [point.x - 1, point.y - 1])
                .collect::<Vec<_>>()
        })
        .max_by(|a, b| contour_area(a).total_cmp(&contour_area(b)))
}

fn contour_area(points: &[Point]) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| f64::from(a[0]) * f64::from(b[1]) - f64::from(a[1]) * f64::from(b[0]))
        .sum::<f64>()
        .abs()
        / 2.0
}

fn chain_approx_simple(points: &[Point]) -> Vec<Point> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let n = points.len();
    points
        .iter()
        .enumerate()
        .filter_map(|(i, &current)| {
            let previous = points[(i + n - 1) % n];
            let next = points[(i + 1) % n];
            let incoming = [
                (current[0] - previous[0]).signum(),
                (current[1] - previous[1]).signum(),
            ];
            let outgoing = [
                (next[0] - current[0]).signum(),
                (next[1] - current[1]).signum(),
            ];
            (incoming != outgoing).then_some(current)
        })
        .collect()
}

fn arc_length(points: &[Point]) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    // OpenCV computes each segment length as float and accumulates in double.
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| {
            let dx = (b[0] - a[0]) as f32;
            let dy = (b[1] - a[1]) as f32;
            f64::from((dx * dx + dy * dy).sqrt())
        })
        .sum()
}

fn squared_distance(a: Point, b: Point) -> f64 {
    let dx = f64::from(a[0] - b[0]);
    let dy = f64::from(a[1] - b[1]);
    dx * dx + dy * dy
}

fn next_point(points: &[Point], position: &mut usize) -> Point {
    let point = points[*position];
    *position = (*position + 1) % points.len();
    point
}

fn approximate_closed_polygon(points: &[Point], epsilon: f64) -> Vec<Point> {
    let count = points.len();
    if count == 0 {
        return Vec::new();
    }
    let epsilon_squared = epsilon * epsilon;
    let mut position = 0;
    let mut farthest_relative = 0;
    let mut start = points[0];
    let mut within_epsilon = false;
    // OpenCV's three approximate-diameter passes define the cyclic start and
    // first split. A different start changes both RDP ties and custom vertices.
    for _ in 0..3 {
        let mut max_distance = 0.0;
        position = (position + farthest_relative) % count;
        start = next_point(points, &mut position);
        for j in 1..count {
            let distance = squared_distance(next_point(points, &mut position), start);
            if distance > max_distance {
                max_distance = distance;
                farthest_relative = j;
            }
        }
        within_epsilon = max_distance <= epsilon_squared;
    }
    let mut approximate = Vec::with_capacity(count);
    let mut stack = Vec::new();
    if within_epsilon {
        approximate.push(start);
    } else {
        let split = (farthest_relative + position) % count;
        stack.push((split, position));
        stack.push((position, split));
    }
    while let Some((slice_start, slice_end)) = stack.pop() {
        let end = points[slice_end];
        let mut position = slice_start;
        let start = next_point(points, &mut position);
        let dx = f64::from(end[0] - start[0]);
        let dy = f64::from(end[1] - start[1]);
        let segment_squared = dx * dx + dy * dy;
        let mut max_distance_scaled = 0.0;
        let mut split = slice_start;
        while position != slice_end {
            let current_index = position;
            let point = next_point(points, &mut position);
            let px = f64::from(point[0] - start[0]);
            let py = f64::from(point[1] - start[1]);
            let projection = px * dx + py * dy;
            let distance_scaled = if projection < 0.0 {
                squared_distance(point, start) * segment_squared
            } else if projection > segment_squared {
                squared_distance(point, end) * segment_squared
            } else {
                (py * dx - px * dy).powi(2)
            };
            if distance_scaled > max_distance_scaled {
                max_distance_scaled = distance_scaled;
                split = current_index;
            }
        }
        if max_distance_scaled <= epsilon_squared * segment_squared {
            approximate.push(start);
        } else {
            stack.push((split, slice_end));
            stack.push((slice_start, split));
        }
    }
    // OpenCV's final nearly-collinear cleanup operates cyclically in-place.
    let count = approximate.len();
    if count <= 2 {
        return approximate;
    }
    let mut position = count - 1;
    let mut start = next_point(&approximate, &mut position);
    let mut write_position = position;
    let mut current = next_point(&approximate, &mut position);
    let mut new_count = count;
    let mut i = 0;
    while i < count && new_count > 2 {
        let end = next_point(&approximate, &mut position);
        let dx = f64::from(end[0] - start[0]);
        let dy = f64::from(end[1] - start[1]);
        let distance =
            (f64::from(current[0] - start[0]) * dy - f64::from(current[1] - start[1]) * dx).abs();
        let inner_product = f64::from(current[0] - start[0]) * f64::from(end[0] - current[0])
            + f64::from(current[1] - start[1]) * f64::from(end[1] - current[1]);
        if distance * distance <= 0.5 * epsilon_squared * (dx * dx + dy * dy)
            && dx != 0.0
            && dy != 0.0
            && inner_product >= 0.0
        {
            new_count -= 1;
            start = end;
            approximate[write_position] = start;
            write_position = (write_position + 1) % count;
            current = next_point(&approximate, &mut position);
            i += 2;
        } else {
            start = current;
            approximate[write_position] = start;
            write_position = (write_position + 1) % count;
            current = end;
            i += 1;
        }
    }
    approximate.truncate(new_count);
    approximate
}

fn custom_vertices(points: &[Point]) -> Vec<[f64; 2]> {
    let mut vertices = Vec::new();
    for (i, &point) in points.iter().enumerate() {
        let previous = points[(i + points.len() - 1) % points.len()];
        let next = points[(i + 1) % points.len()];
        let v1 = [
            f64::from(previous[0] - point[0]),
            f64::from(previous[1] - point[1]),
        ];
        let v2 = [f64::from(next[0] - point[0]), f64::from(next[1] - point[1])];
        if v1[1] * v2[0] - v1[0] * v2[1] >= 0.0 {
            continue;
        }
        let length1 = v1[0].hypot(v1[1]);
        let length2 = v2[0].hypot(v2[1]);
        let angle = ((v1[0] * v2[0] + v1[1] * v2[1]) / (length1 * length2))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees();
        if (angle - 45.0).abs() < 1.0 {
            let direction = [
                v1[0] / length1 + v2[0] / length2,
                v1[1] / length1 + v2[1] / length2,
            ];
            let direction_length = direction[0].hypot(direction[1]);
            let step = (length1 + length2) / 2.0;
            vertices.push([
                f64::from(point[0]) + direction[0] / direction_length * step,
                f64::from(point[1]) + direction[1] / direction_length * step,
            ]);
        } else {
            vertices.push(point.map(f64::from));
        }
    }
    vertices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencv_rectangle_golden_preserves_mask_outline() {
        let mut mask = vec![-20.0; SIDE * SIDE];
        for y in 30..100 {
            for x in 20..150 {
                mask[y * SIDE + x] = 20.0;
            }
        }
        let (points, source) =
            extract_polygon(&mask, [0.0, 0.0, 200.0, 200.0], 200, 200, 0.3).unwrap();
        assert_eq!(source, "mask");
        // cv2 5.0 RETR_EXTERNAL + CHAIN_APPROX_SIMPLE + approxPolyDP(.004).
        assert_eq!(
            points,
            vec![[20.0, 30.0], [20.0, 99.0], [149.0, 99.0], [149.0, 30.0]]
        );
    }

    #[test]
    fn empty_mask_fallback_is_explicit() {
        let (points, source) = extract_polygon(
            &vec![-20.0; SIDE * SIDE],
            [10.9, 20.9, 100.9, 150.9],
            200,
            200,
            0.3,
        )
        .unwrap();
        assert_eq!(source, "empty_mask_rectangle");
        assert_eq!(
            points,
            vec![[10.0, 20.0], [100.0, 20.0], [100.0, 150.0], [10.0, 150.0]]
        );
    }

    #[test]
    fn foreground_lost_by_downsampling_uses_sdk_float_rectangle() {
        let mut mask = vec![-20.0; SIDE * SIDE];
        mask[SIDE + 1] = 20.0;
        let (points, source) =
            extract_polygon(&mask, [0.1, 0.2, 99.9, 99.9], 100, 100, 0.3).unwrap();
        assert_eq!(source, "empty_resized_mask_rectangle");
        assert_eq!(
            points,
            vec![[0.1, 0.2], [99.9, 0.2], [99.9, 99.9], [0.1, 99.9]]
        );
    }

    #[test]
    fn opencv_golden_ignores_holes_and_smaller_components() {
        let mut image = GrayImage::new(62, 62);
        for y in 11..51 {
            for x in 11..51 {
                image.put_pixel(x, y, Luma([1]));
            }
        }
        for y in 21..31 {
            for x in 21..31 {
                image.put_pixel(x, y, Luma([0]));
            }
        }
        image.put_pixel(2, 2, Luma([1]));
        let contour = largest_external_contour(&image).unwrap();
        let simple = chain_approx_simple(&contour);
        assert_eq!(simple, vec![[10, 10], [10, 49], [49, 49], [49, 10]]);
    }

    #[test]
    fn nonrectangular_and_resized_polygons_match_python_opencv_golden() {
        // Expected vertices were generated with the unmodified Transformers
        // V3 methods and local cv2 5.0.0, rather than with this implementation.
        let cases = [
            (
                "stagger",
                200,
                200,
                [0.0, 0.0, 200.0, 200.0],
                vec![[20.0, 30.0], [59.0, 149.0], [162.0, 149.0], [139.0, 30.0]],
            ),
            (
                "concave",
                200,
                200,
                [0.0, 0.0, 200.0, 200.0],
                vec![
                    [20.0, 159.0],
                    [59.0, 159.0],
                    [159.0, 69.0],
                    [159.0, 20.0],
                    [20.0, 20.0],
                ],
            ),
            (
                "diamond",
                200,
                200,
                [0.0, 0.0, 200.0, 200.0],
                vec![[100.0, 30.0], [30.0, 100.0], [100.0, 170.0], [170.0, 100.0]],
            ),
            (
                "sharp",
                200,
                200,
                [0.0, 0.0, 200.0, 200.0],
                vec![
                    [30.0, 110.0],
                    [100.0, 110.0],
                    [58.446_518, 80.349_1],
                    [30.0, 30.0],
                ],
            ),
            (
                "resize",
                333,
                287,
                [21.6, 33.2, 169.8, 178.1],
                vec![[33.0, 44.0], [83.0, 176.0], [168.0, 177.0], [168.0, 44.0]],
            ),
        ];
        for (name, width, height, bbox, expected) in cases {
            let mut mask = vec![-20.0; SIDE * SIDE];
            for y in 0_i32..200 {
                for x in 0_i32..200 {
                    let foreground = match name {
                        "stagger" | "resize" => {
                            (30..150).contains(&y)
                                && x >= 20 + (y - 30) / 3
                                && x < 140 + (y - 30) / 5
                        }
                        "concave" => {
                            ((20..160).contains(&x) && (20..70).contains(&y))
                                || ((20..60).contains(&x) && (20..160).contains(&y))
                        }
                        "diamond" => (x - 100).abs() + (y - 100).abs() <= 70,
                        "sharp" => (30..=110).contains(&y) && x >= 30 && x <= 210 - y,
                        _ => unreachable!(),
                    };
                    if foreground {
                        mask[y as usize * SIDE + x as usize] = 20.0;
                    }
                }
            }
            let (actual, source) = extract_polygon(&mask, bbox, width, height, 0.3).unwrap();
            assert_eq!(source, "mask", "{name}");
            assert_eq!(actual.len(), expected.len(), "{name}: {actual:?}");
            for (a, b) in actual.iter().zip(expected) {
                for i in 0..2 {
                    assert!((a[i] - b[i]).abs() < 0.0001, "{name}: {actual:?}");
                }
            }
        }
    }
}
