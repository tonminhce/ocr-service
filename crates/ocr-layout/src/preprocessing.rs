//! Uint8 separable bicubic preprocessing matching the course's Torchvision
//! processor: half-pixel centers, a=-0.75, no antialias, fixed-point weights,
//! horizontal then vertical, rounding/clipping after each pass.
//!
//! Algorithm source: PyTorch v2.13.0 UpSampleKernel.cpp (BSD-3-Clause).
//! See THIRD_PARTY_NOTICES.md. No Torch/Python runtime is needed.

use image::RgbImage;

const EDGE: usize = 800;

struct Axis {
    starts: Vec<usize>,
    weights: Vec<Vec<i32>>,
    precision: u32,
}

fn cubic(x: f64) -> f64 {
    let x = x.abs();
    let a = -0.75;
    if x < 1.0 {
        ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a
    } else {
        0.0
    }
}

fn axis(input: usize, output: usize) -> Axis {
    let scale = input as f64 / output as f64;
    let mut starts = Vec::with_capacity(output);
    let mut coefficients = Vec::with_capacity(output);
    let mut largest = 0.0_f64;
    for i in 0..output {
        let source = scale * (i as f64 + 0.5) - 0.5;
        let index = (source.floor() as i64).min(input as i64 - 1);
        let lambda = (source - index as f64).clamp(0.0, 1.0);
        let low = index - 1;
        let start = low.max(0) as usize;
        let end = (index + 3).min(input as i64) as usize;
        let mut weights = vec![0.0; end - start];
        for j in 0..4 {
            let at = (low + j).clamp(0, input as i64 - 1) as usize - start;
            weights[at] += cubic(j as f64 - 1.0 - lambda);
            largest = largest.max(weights[at]);
        }
        starts.push(start);
        coefficients.push(weights);
    }
    let mut precision = 0;
    while precision < 22 {
        if (0.5 + largest * f64::from(1 << (precision + 1))) as i32 >= 1 << 15 {
            break;
        }
        precision += 1;
    }
    let factor = f64::from(1 << precision);
    let weights = coefficients
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|v| {
                    let value = v * factor;
                    (value + if value < 0.0 { -0.5 } else { 0.5 }) as i32
                })
                .collect()
        })
        .collect();
    Axis {
        starts,
        weights,
        precision,
    }
}

pub fn resize_rgb(image: &RgbImage) -> RgbImage {
    assert!(image.width() > 0 && image.height() > 0);
    let w = image.width() as usize;
    let h = image.height() as usize;
    let mut horizontal = vec![0_u8; h * EDGE * 3];
    if w == EDGE {
        horizontal.copy_from_slice(image.as_raw());
    } else {
        let spec = axis(w, EDGE);
        for y in 0..h {
            for x in 0..EDGE {
                for channel in 0..3 {
                    let mut value = 1 << (spec.precision - 1);
                    for (offset, weight) in spec.weights[x].iter().enumerate() {
                        value += i32::from(
                            image.as_raw()[(y * w + spec.starts[x] + offset) * 3 + channel],
                        ) * weight;
                    }
                    horizontal[(y * EDGE + x) * 3 + channel] =
                        (value >> spec.precision).clamp(0, 255) as u8;
                }
            }
        }
    }
    if h == EDGE {
        return RgbImage::from_raw(EDGE as u32, EDGE as u32, horizontal)
            .expect("fixed raster length");
    }
    let spec = axis(h, EDGE);
    let mut output = vec![0_u8; EDGE * EDGE * 3];
    for y in 0..EDGE {
        for x in 0..EDGE {
            for channel in 0..3 {
                let mut value = 1 << (spec.precision - 1);
                for (offset, weight) in spec.weights[y].iter().enumerate() {
                    value +=
                        i32::from(horizontal[((spec.starts[y] + offset) * EDGE + x) * 3 + channel])
                            * weight;
                }
                output[(y * EDGE + x) * 3 + channel] =
                    (value >> spec.precision).clamp(0, 255) as u8;
            }
        }
    }
    RgbImage::from_raw(EDGE as u32, EDGE as u32, output).expect("fixed raster length")
}

pub fn tensor(image: &RgbImage) -> Vec<f32> {
    let resized = resize_rgb(image);
    let mut output = vec![0_f32; 3 * EDGE * EDGE];
    for (index, pixel) in resized.pixels().enumerate() {
        for channel in 0..3 {
            output[channel * EDGE * EDGE + index] = f32::from(pixel[channel]) / 255.0_f32;
        }
    }
    output
}
