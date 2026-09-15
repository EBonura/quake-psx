use std::path::Path;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let status = Command::new("python3")
        .arg(root.join("tools/bootstrap-components.py"))
        .arg("--root").arg(root.join(".psoxide"))
        .arg("--lock").arg(root.join("components.lock.json"))
        .status()?;
    if !status.success() { return Err("component bootstrap failed".into()); }
    Ok(())
}
