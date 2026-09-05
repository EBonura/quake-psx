use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.psoxide");
    psoxide_link::hydrate_pinned(
        &destination,
        "5fa8c351c0b168ba6fea78d27e7134f668dab137",
        true,
    )
}
