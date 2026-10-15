//! Generate synthetic contract/card mechanics fixtures; never real accuracy labels.
use anyhow::{Context, Result, ensure};
#[allow(dead_code)]
#[path = "../tests/support/v2_fixture.rs"]
mod fixture;
fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: synthetic_v2_fixture OUTPUT_DIRECTORY")?;
    let path = std::path::Path::new(&path);
    ensure!(!path.exists(), "output directory must not already exist");
    std::fs::create_dir_all(path)?;
    let (mut manifest, prediction) = fixture::fixture();
    let manifest_path = fixture::save(path, &mut manifest, &prediction);
    println!("{}", manifest_path.display());
    Ok(())
}
