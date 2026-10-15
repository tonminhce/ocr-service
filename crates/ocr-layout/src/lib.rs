//! PP-DocLayoutV3 course-profile detector. ONNX execution is isolated in a Rust
//! helper so the worker can terminate and reap it on deadline or lease loss.
//!
//! Postprocessing follows the Apache-2.0 upstream Transformers/Paddle model and
//! the course's SDK policy. See THIRD_PARTY_NOTICES.md for source versions.

pub mod model;
mod polygon;
pub mod postprocess;
pub mod preprocessing;
pub mod protocol;

use serde::{Deserialize, Serialize};

pub const MODEL_ID: &str = "PaddlePaddle/PP-DocLayoutV3_safetensors";
pub const MODEL_REVISION: &str = "e4f489dc536556fc1be02b973ba06756e0d4a2ac";
pub const QUERIES: usize = 300;
pub const CLASSES: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Text,
    Table,
    Formula,
    Skip,
    Abandon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub index: u32,
    pub class_id: u32,
    pub label: String,
    pub task: Task,
    /// Raw model box in upright page pixels; it can extend beyond the page.
    /// Crops use the SDK-clipped/quantized bbox_2d and polygon instead.
    pub bbox: [f32; 4],
    /// SDK crop coordinates, quantized in the 0..1000 page frame.
    pub bbox_2d: [i32; 4],
    pub polygon: Vec<[i32; 2]>,
    pub polygon_source: String,
    pub score: f32,
    /// Learned V3 query rank, preserved through suppression and filtering.
    pub reading_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageLayout {
    pub width: u32,
    pub height: u32,
    pub regions: Vec<Region>,
    pub discarded: Vec<Region>,
}
