//! Generate synthetic evaluator fixtures; these are not model accuracy results.
#[path = "../tests/support/mod.rs"]
mod support;
fn main() {
    let target = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/synthetic");
    std::fs::create_dir_all(&target).unwrap();
    let manifest = support::fixture_files(&target);
    support::save(&target, &manifest);
}
