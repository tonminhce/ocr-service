use std::path::Path;

use anyhow::{Context, Result, ensure};
use image::RgbImage;
use ort::{session::Session, value::Tensor};
use sha2::{Digest, Sha256};

use crate::{PageLayout, postprocess, preprocessing};

pub struct Detector {
    session: Session,
}

impl Detector {
    pub fn load(path: &Path, digest: &str) -> Result<Self> {
        Self::load_with_cpu_arena(path, digest, true)
    }

    pub fn load_with_cpu_arena(path: &Path, digest: &str, cpu_arena_enabled: bool) -> Result<Self> {
        ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "layout model SHA-256 is required"
        );
        let length = std::fs::metadata(path)
            .context("layout model is unavailable")?
            .len();
        ensure!(
            length > 0 && length <= 256 * 1024 * 1024,
            "layout model exceeds its artifact bound"
        );
        let bytes = std::fs::read(path).context("cannot read layout model")?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == digest.to_ascii_lowercase(),
            "layout model SHA-256 mismatch"
        );
        let mut builder = Session::builder()?
            .with_intra_threads(2)?
            .with_inter_threads(1)?;
        if !cpu_arena_enabled {
            builder = builder.with_execution_providers([ort::ep::CPU::default()
                .with_arena_allocator(false)
                .build()])?;
        }
        let session = builder
            .commit_from_memory(&bytes)
            .context("cannot load PP-DocLayoutV3 graph")?;
        Ok(Self { session })
    }

    pub fn detect(&mut self, page: &RgbImage) -> Result<PageLayout> {
        ensure!(
            page.width() > 0
                && page.height() > 0
                && page.width() <= 8192
                && page.height() <= 8192
                && u64::from(page.width()) * u64::from(page.height()) <= 100_000_000,
            "page dimensions exceed the preprocessing bound"
        );
        let data = preprocessing::tensor(page);
        let input = Tensor::from_array(([1_usize, 3, 800, 800], data))?;
        let outputs = self.session.run(ort::inputs!["pixel_values" => input])?;
        let (ls, logits) = outputs["logits"].try_extract_tensor::<f32>()?;
        let (bs, boxes) = outputs["pred_boxes"].try_extract_tensor::<f32>()?;
        let (os, orders) = outputs["order_logits"].try_extract_tensor::<f32>()?;
        let (ms, masks) = outputs["out_masks"].try_extract_tensor::<f32>()?;
        ensure!(
            ls.as_ref() == [1, 300, 25]
                && bs.as_ref() == [1, 300, 4]
                && os.as_ref() == [1, 300, 300]
                && ms.as_ref() == [1, 300, 200, 200],
            "unexpected detector tensor shapes"
        );
        postprocess::postprocess(page.width(), page.height(), logits, boxes, orders, masks)
    }
}
