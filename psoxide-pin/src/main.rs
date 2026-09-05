use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.psoxide");
    psoxide_link::hydrate_pinned(
        &destination,
        "8df242b353b8a3664c1d2ed20622d692d1349306",
        true,
    )
}
