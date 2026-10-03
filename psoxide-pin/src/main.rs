use std::collections::BTreeMap;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    psoxide_link::components::materialize(
        &root.join(".psoxide"),
        &BTreeMap::new(),
        false,
        Some(&root.join("components.lock.json")),
    )?;
    Ok(())
}
