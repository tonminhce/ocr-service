use std::io::{BufReader, Write};

use anyhow::{Result, ensure};
use ocr_layout::{
    PageLayout, Region, Task,
    protocol::{self, DetectRequest, Response},
};

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let digest = arguments
        .windows(2)
        .find(|args| args[0] == "--sha256")
        .map(|args| args[1].clone())
        .ok_or_else(|| anyhow::anyhow!("--sha256 required"))?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid fixture digest"
    );
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &Response::Ready {
            version: protocol::VERSION,
            model_sha256: digest,
        },
    )?;
    output.write_all(b"\n")?;
    output.flush()?;
    let mut input = BufReader::new(std::io::stdin().lock());
    while let Some(frame) = protocol::read_frame(&mut input, protocol::MAX_REQUEST_BYTES)? {
        let request: DetectRequest = serde_json::from_slice(&frame)?;
        ensure!(
            request.version == protocol::VERSION
                && protocol::valid_image_basename(&request.image)
                && (1..=8192).contains(&request.width)
                && (1..=8192).contains(&request.height),
            "invalid fixture request"
        );
        let region = Region {
            index: 0,
            class_id: 22,
            label: "text".into(),
            task: Task::Text,
            bbox: [0.0, 0.0, request.width as f32, request.height as f32],
            bbox_2d: [0, 0, 1000, 1000],
            polygon: vec![[0, 0], [1000, 0], [1000, 1000], [0, 1000]],
            polygon_source: "synthetic_smoke_fixture".into(),
            score: 1.0,
            reading_order: 0,
        };
        serde_json::to_writer(
            &mut output,
            &Response::Layout {
                id: request.id,
                layout: PageLayout {
                    width: request.width,
                    height: request.height,
                    regions: vec![region],
                    discarded: vec![],
                },
            },
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
