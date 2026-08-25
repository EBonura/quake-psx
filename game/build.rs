use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn json_integer_array<const N: usize>(text: &str, key: &str) -> [i32; N] {
    let marker = format!("\"{key}\"");
    let value = text
        .split_once(&marker)
        .unwrap_or_else(|| panic!("visual camera JSON lacks {key}"))
        .1;
    let values = value
        .split_once('[')
        .unwrap_or_else(|| panic!("visual camera JSON {key} lacks an array"))
        .1
        .split_once(']')
        .unwrap_or_else(|| panic!("visual camera JSON {key} has no closing bracket"))
        .0
        .split(',')
        .map(|field| {
            field
                .trim()
                .parse::<i32>()
                .unwrap_or_else(|error| panic!("visual camera JSON {key} value: {error}"))
        })
        .collect::<Vec<_>>();
    values.try_into().unwrap_or_else(|values: Vec<i32>| {
        panic!(
            "visual camera JSON {key} has {} values, expected {N}",
            values.len()
        )
    })
}

fn generate_visual_camera(root: &Path) {
    let camera_path = root.join("tools/visual-parity-cameras.json");
    println!("cargo:rerun-if-changed={}", camera_path.display());
    let text = fs::read_to_string(&camera_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", camera_path.display()));
    assert!(
        text.contains("\"schema\": 1") && text.contains("\"map\": \"E1M1\""),
        "visual camera JSON must be schema 1 and pin E1M1"
    );
    let origin = json_integer_array::<3>(&text, "origin_q12");
    let angles = json_integer_array::<3>(&text, "angles");
    let world = json_integer_array::<4>(&text, "world_region");
    let hud = json_integer_array::<4>(&text, "hud_region");
    let generated = format!(
        "pub const CAMERA_ORIGIN_Q12: [i32; 3] = {origin:?};\n\
         pub const CAMERA_ANGLES: [i16; 3] = [{}, {}, {}];\n\
         pub const WORLD_REGION: [u16; 4] = [{}, {}, {}, {}];\n\
         pub const HUD_REGION: [u16; 4] = [{}, {}, {}, {}];\n",
        angles[0],
        angles[1],
        angles[2],
        world[0],
        world[1],
        world[2],
        world[3],
        hud[0],
        hud[1],
        hud[2],
        hud[3],
    );
    let output =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("visual_parity_camera.rs");
    fs::write(&output, generated)
        .unwrap_or_else(|error| panic!("write {}: {error}", output.display()));
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".psoxide" | ".quakepsx" | "target" | "captures" | "dist" | "graphify-out"
    ) || name.starts_with("build-")
}

fn audit_rust_only(path: &Path) {
    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .map(|entry| entry.expect("read source entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            let name = entry
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !ignored_directory(name) {
                audit_rust_only(&entry);
            }
            continue;
        }
        let extension = entry
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            !matches!(
                extension.as_str(),
                "c" | "h" | "cc" | "cpp" | "cxx" | "m" | "mm" | "s" | "asm"
            ),
            "native implementation source is forbidden: {}",
            entry.display()
        );
        if extension == "rs" {
            let source = fs::read_to_string(&entry)
                .unwrap_or_else(|error| panic!("read {}: {error}", entry.display()));
            let foreign_abi = ["extern ", "\"C\""].concat();
            let foreign_ffi = ["core::", "ffi::", "c_"].concat();
            assert!(
                !source.contains(&foreign_abi) && !source.contains(&foreign_ffi),
                "foreign compatibility binding is forbidden: {}",
                entry.display()
            );
            println!("cargo:rerun-if-changed={}", entry.display());
        }
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = manifest.parent().expect("game lives under repository root");
    audit_rust_only(root);
    generate_visual_camera(root);
    println!("cargo:rerun-if-env-changed=PSOXIDE");
    let psoxide = env::var_os("PSOXIDE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".psoxide"));
    let linker = psoxide.join("sdk/psoxide.ld");
    assert!(
        linker.is_file(),
        "PSoXide SDK is not hydrated at {}",
        psoxide.display()
    );

    // Quake's shipping frame nests the orchestration and renderer paths. The
    // full shipping route reached SP=0x801f5f88: 40,824 bytes below the
    // linker's STACK_INIT=0x801fff00 (the profiler reports a 41,024-byte root
    // depth from its entry SP=0x801fffc8). Derive a game-local linker script
    // which reserves 52 KiB, leaving 12,424 bytes above the heap boundary,
    // without changing every PSoXide program.
    let linker_source = fs::read_to_string(&linker)
        .unwrap_or_else(|error| panic!("read {}: {error}", linker.display()));
    let stack_marker = "STACK_RESERVE = 0x8000;";
    assert!(
        linker_source.contains(stack_marker),
        "PSoXide linker stack marker changed; re-audit Quake's stack reserve"
    );
    let quake_linker = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"))
        .join("quake-psoxide.ld");
    fs::write(
        &quake_linker,
        linker_source.replacen(stack_marker, "STACK_RESERVE = 0xD000;", 1),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", quake_linker.display()));

    // The shipping executable and repository implementation are Rust-only.
    println!("cargo:rustc-link-arg=-T{}", quake_linker.display());
    if env::var_os("QUAKE_PSX_RUST_DEBUG").is_none() {
        println!("cargo:rustc-link-arg=--oformat=binary");
    }
    println!("cargo:rerun-if-env-changed=QUAKE_PSX_RUST_DEBUG");
    println!("cargo:rerun-if-changed={}", linker.display());
}
