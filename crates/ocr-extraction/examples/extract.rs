//! cargo run -p ocr-extraction --example extract -- /private/blocks.json
use ocr_extraction::{SourceBlock, extract};
use std::{
    env, fs,
    io::{self, Read},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = if let Some(path) = env::args().nth(1) {
        fs::read_to_string(path)?
    } else {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;
        input
    };
    let blocks: Vec<SourceBlock> = serde_json::from_str(&input)?;
    serde_json::to_writer_pretty(io::stdout(), &extract(&blocks))?;
    Ok(())
}
