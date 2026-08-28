use quake_cook::{
    cook_entities, cook_geometry_and_models, cook_gfx, cook_global_sounds, cook_map, cook_sounds,
    Bsp, BspLump, GeometryLumps, MapCookConfig, PakArchive, SkyEncoding, SFX_PARKING_TAIL,
};
use quake_core::combat::pickup_for_entity;
use quake_core::monster::{highest_frame, MonsterKind};
use quake_core::targets::{TargetActions, TargetGraph, MAX_DELAYED_USES, MAX_TARGET_ENTITIES};
use quake_formats::resident::{MapLoadError as ResidentMapLoadError, ResidentMap};
use quake_formats::{
    alias_model_is_sprite, decode_sound_bank, episode_directory_index, AliasModelTable,
    CookedRecord, EpisodeDirectoryEncoder, Face, Leaf, LumpKind, MapEntity, PsbIndex, PsbVersion,
    RecordSlice, SliceReader, SoundBankKind, SoundEffect, TextureInfo, EPISODE_DIRECTORY_BYTES,
    RESIDENT_MAP_ARENA_BYTES, SOUND_GLOBAL_EFFECTS, SOUND_SPU_END, TEXTURE_LAYERED_SKY,
    TEXTURE_SKY,
};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SHAREWARE_URL: &str = "https://www.gamers.org/pub/idgames2/idstuff/quake/quake106.zip";
const SHAREWARE_SHA256: &str = "ec6c9d34b1ae0252ac0066045b6611a7919c2a0d78a3a66d9387a8f597553239";
const PAK0_SHA256: &str = "35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af";
// Keep this in sync with the psoxide-link revision in Cargo.lock.
const PSOXIDE_REV: &str = "f894437986e1c0148ad39eaa38134ab09185312d";
const PROVENANCE_FILE: &str = "quake-psx.provenance.json";
const GUEST_STAGE_SCHEMA: u32 = 1;
const GUEST_STAGE_ROOT: &str = "/tmp/quake-psx-guest-v1";
const GUEST_STAGE_MARKER: &str = ".quake-psx-guest-recipe";
const SHIPPING_GUEST_PROFILE: &str = "release";
const SHIPPING_CARGO_HOME: &str = "/tmp/quake-psx-cargo-home-v1";
const SHIPPING_CARGO_HOME_MARKER: &str = ".quake-psx-shipping-cargo-home";
const SHIPPING_CARGO_HOME_SCHEMA: u32 = 1;
const GUEST_RECIPE_PATHS: &[&str] = &[
    "rust-toolchain.toml",
    "game",
    "tools/visual-parity-cameras.json",
    "crates/quake-core",
    "crates/quake-formats",
    ".psoxide/sdk/psoxide.ld",
    ".psoxide/crates/psx-hw",
    ".psoxide/editor/crates/psxed-format",
    ".psoxide/engine/crates/psx-bsp",
    ".psoxide/engine/crates/psx-engine",
    ".psoxide/engine/crates/psx-level",
    ".psoxide/engine/crates/psx-render-contract",
    ".psoxide/sdk/crates/psx-asset",
    ".psoxide/sdk/crates/psx-font",
    ".psoxide/sdk/crates/psx-gpu",
    ".psoxide/sdk/crates/psx-gte",
    ".psoxide/sdk/crates/psx-gte-core",
    ".psoxide/sdk/crates/psx-io",
    ".psoxide/sdk/crates/psx-math",
    ".psoxide/sdk/crates/psx-pack",
    ".psoxide/sdk/crates/psx-pad",
    ".psoxide/sdk/crates/psx-rt",
    ".psoxide/sdk/crates/psx-sfx",
    ".psoxide/sdk/crates/psx-spu",
    ".psoxide/sdk/crates/psx-telemetry",
    ".psoxide/sdk/crates/psx-vram",
];
// Minimal PSoXide workspaces for the dependencies used by the PS1 executable.
// Editor, emulator and unrelated tool packages are not copied into the stage.
const PSOXIDE_ROOT_WORKSPACE: &str = r#"[workspace]
resolver = "2"
members = ["crates/psx-hw", "editor/crates/psxed-format"]

[workspace.package]
edition = "2021"
rust-version = "1.87"
license = "GPL-2.0-or-later"
repository = "https://github.com/EBonura/PSoXide"

[workspace.lints.rust]
missing_docs = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"

[workspace.lints.clippy]
dbg_macro = "deny"
todo = "deny"
too_many_arguments = "allow"

[workspace.dependencies]
bitflags = { version = "2.6", default-features = false }
"#;
const PSOXIDE_SDK_WORKSPACE: &str = r#"[workspace]
resolver = "2"
members = [
    "crates/psx-io",
    "crates/psx-rt",
    "crates/psx-gpu",
    "crates/psx-gte",
    "crates/psx-gte-core",
    "crates/psx-pad",
    "crates/psx-vram",
    "crates/psx-font",
    "crates/psx-math",
    "crates/psx-spu",
    "crates/psx-sfx",
    "crates/psx-telemetry",
    "crates/psx-asset",
    "crates/psx-pack",
]

[workspace.package]
edition = "2021"
license = "GPL-2.0-or-later"
repository = "https://github.com/EBonura/PSoXide"

[workspace.lints.rust]
missing_docs = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"

[workspace.lints.clippy]
dbg_macro = "deny"
todo = "deny"
too_many_arguments = "allow"

[workspace.dependencies]
psx-hw = { path = "../crates/psx-hw" }
psx-io = { path = "crates/psx-io" }
psx-rt = { path = "crates/psx-rt" }
psx-gpu = { path = "crates/psx-gpu" }
psx-gte = { path = "crates/psx-gte" }
psx-gte-core = { path = "crates/psx-gte-core" }
psx-pad = { path = "crates/psx-pad" }
psx-vram = { path = "crates/psx-vram" }
psx-font = { path = "crates/psx-font" }
psx-math = { path = "crates/psx-math" }
psx-spu = { path = "crates/psx-spu" }
psx-asset = { path = "crates/psx-asset" }
psx-pack = { path = "crates/psx-pack" }
"#;
const PSOXIDE_ENGINE_WORKSPACE: &str = r#"[workspace]
resolver = "2"
members = ["crates/psx-bsp", "crates/psx-engine", "crates/psx-level", "crates/psx-render-contract"]

[workspace.package]
edition = "2021"
license = "GPL-2.0-or-later"
repository = "https://github.com/EBonura/PSoXide"

[workspace.lints.rust]
missing_docs = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"

[workspace.lints.clippy]
dbg_macro = "deny"
todo = "deny"
too_many_arguments = "allow"
"#;
const GUEST_STAGE_WORKSPACES: &[(&str, &str)] = &[
    (".psoxide/Cargo.toml", PSOXIDE_ROOT_WORKSPACE),
    (".psoxide/sdk/Cargo.toml", PSOXIDE_SDK_WORKSPACE),
    (".psoxide/engine/Cargo.toml", PSOXIDE_ENGINE_WORKSPACE),
];
const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_BYTES: usize = 136;
const VISUAL_PROBE_MAGIC: u32 = 0x5156_4953;
const VISUAL_PROBE_VERSION: u32 = 2;
const VISUAL_PROBE_BYTES: usize = 60;
// The graphical armor counter starts at y=184, so hash the world and HUD
// separately.
const VISUAL_WORLD_REGION: ImageRegion = ImageRegion::new(0, 0, 320, 184);
const VISUAL_HUD_REGION: ImageRegion = ImageRegion::new(0, 184, 320, 56);
// Advance one simulation tick per rendered frame so renderer speed cannot
// change the animation sampled by this camera.
const EXPECTED_VISUAL_WORLD_FNV1A64: u64 = 0x951a_75ba_bd8f_2904;
const EXPECTED_VISUAL_HUD_FNV1A64: u64 = 0x09e9_6289_3d49_6136;
const VISUAL_MAX_WORLD_PACKETS: u32 = 400_000;
const VISUAL_MAX_HARDWARE_TRIANGLES: u32 = 460_000;
// Top of the guest heap. The linker script computes it as
// `STACK_INIT - STACK_RESERVE`, and rust-lld's map prints the location
// counter for that assignment rather than its value, so it is spelled out
// here. Every reading of the allocator cursor is range-checked against it and
// against the map's `__heap_start`, so a linker-script move fails loudly
// instead of quietly reporting a nonsense headroom.
const SHIP_BOOT_HEAP_END: u32 = 0x801f_2f00;
// Shipping builds have no guest telemetry, so this run uses an instruction
// budget long enough to cover boot and at least a thousand gameplay frames.
const SHIP_BOOT_STEPS: u64 = 2_500_000_000;
// A live shipping image reads the pad once per frame.
const SHIP_BOOT_MIN_PORT1_POLLS: u64 = 400;
// Minimum bump-allocator headroom after boot and the sampled gameplay period.
const SHIP_BOOT_MIN_HEAP_FREE: u32 = 8_192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Build,
    Assets,
    Compile,
    Disc,
    Check,
    ShipBoot,
    MapRegress,
    StartRouteRegress,
    E1m1ChainRegress,
    E1m1ChainBench,
    E1m1SelectionCacheBench,
    E1m1TopologyCacheBench,
    E1m1IndexedProjectionBench,
    E1m1AabbOffsetsBench,
    E1m1RelaxedQuadPairingBench,
    E1m1SharedSubdivisionEdgesBench,
    E1m1Level0FastPathBench,
    E1m1SpeculativeLevel0Bench,
    E1m1DepthOnlySubdivisionBench,
    E1m1GteOtzBench,
    E1m1CompactSubdivisionEmittersBench,
    E1m1CompactSubdivisionKernelsBench,
    E1m1CompactLevel2KernelBench,
    E1m1CompactWorldLevel2KernelBench,
    E1m1GpuLatticeClipBench,
    E1m1GpuPolygonClipBench,
    E1m1GpuPolygonDepthOnlyBench,
    E1m1GpuPolygonCompactOtBench,
    E1m1GpuPolygonFusedProjectionBench,
    E1m1GpuPolygonPlaneIndexBench,
    E1m1GpuPolygonWindowRunsBench,
    E1m1GpuPolygonWindowInsertBench,
    E1m1GpuPolygonWindowRangeBench,
    E1m1GpuPolygonCellStreamBench,
    E1m1GpuPolygonCellPolicyBench,
    GpuPolygonCellPolicyDisc,
    E1m1GpuPolygonQuakeKernelBench,
    E1m1GpuPolygonLevel0RunBench,
    E1m1GpuPolygonColdAdaptiveBench,
    E1m1GpuPolygonColdLevel2Bench,
    E1m1GpuPolygonResidentStreamBench,
    E1m1GpuPolygonResidentLevel2StreamBench,
    E1m1GpuPolygonResidentLevel2ScatterBench,
    E1m1GpuPolygonResidentLevel2ColdCacheBench,
    E1m1GpuPolygonResidentBaseCacheBench,
    E1m1GpuPolygonResidentBaseCacheFastBench,
    E1m1GpuSurfaceClipBench,
    E1m1StaticWorldReuseBench,
    E1m1HoistedIndexedWorldBench,
    E1m1FixedFanQuadsBench,
    E1m1FixedFanGuardedBench,
    E1m1FixedFanLevel2Bench,
    E1m1SubdivisionCacheBench,
    E1m1SubdivisionCacheLevel2Bench,
    E1m1SubdivisionCacheLevel2SmallBench,
    E1m1SubdivisionCacheLevel2LayoutControlBench,
    E1m1BlockFrustumBench,
    E1m1HierarchicalBlockFrustumBench,
    E1m1BlockFrustum32Bench,
    E1m1PlaneRunCacheBench,
    BlockFrustumRegress,
    GpuPolygonClipRegress,
    SelectionCacheRegress,
    SelectionCacheShipBoot,
    E1m1RendererCensus,
    E1m1GpuCensus,
    E1m1GpuPolygonCensus,
    E1m2E1m3RouteRegress,
    SurvivalRegress,
    VisualParityRegress,
    SystemsRegress,
    CombatRegress,
    MonsterRegress,
    MonsterJumpRegress,
    BestiaryRegress,
    Episode1Regress,
    ArsenalRegress,
    AudioRegress,
    AmbientRegress,
    Regress,
    Hardware,
}

#[derive(Debug)]
struct Cli {
    action: Action,
    quake_dir: Option<PathBuf>,
    psoxide: Option<PathBuf>,
    allow_psoxide_drift: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct Probe {
    version: u32,
    complete: u32,
    phase: u32,
    failure_code: u32,
    failure_map: u32,
    failure_entity: u32,
    failure_detail: u32,
    total_frames: u32,
    maps_loaded: u32,
    maps_validated: u32,
    transitions: u32,
    weapon_selected: u32,
    weapon_fired: u32,
    weapon_animated: u32,
    monster_present: u32,
    monster_animated: u32,
    monster_state_bounds: u32,
    monster_attack: u32,
    monster_pain: u32,
    monster_death: u32,
    boss: u32,
    current_map: u32,
    route_index: u32,
    last_health: u32,
    state_ranges: u32,
    valid_state_ranges: u32,
    map_loads: u32,
    stage_frames: u32,
    shock_count: u32,
    intermission_state: u32,
    player_state: u32,
    weapon_pickups: u32,
    target_edges: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VisualProbe {
    frames: u32,
    packets: u32,
    hardware_triangles: u32,
    windowed_packets: u32,
    window_resets: u32,
    reset_failures: u32,
    overflow_frames: u32,
    view_model_packets: u32,
    view_model_registered_packets: u32,
    hud_packets: u32,
    hud_registered_packets: u32,
    crosshair_registered_packets: u32,
    screen_registered_packets: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageRegion {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl ImageRegion {
    const fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PpmImage {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn main() {
    if let Err(error) = real_main() {
        eprintln!("quake-psx-build: {error}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = parse_cli()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    audit_sources(&root)?;
    let sdk = hydrate_psoxide(&root, cli.psoxide.as_deref(), cli.allow_psoxide_drift)?;
    println!("PSoXide SDK: {}", sdk.describe());

    match cli.action {
        Action::Check => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            require_tool(&["cargo"])?;
            verify_psoxide_rev_on_main()?;
            validate_source_episode(&root, &pak)?;
            if cooked_assets_complete(&root) {
                validate_geometry_parity(&root, &pak)?;
            }
            println!("shareware PAK: {}", pak.display());
            println!("PSoXide expected revision: {PSOXIDE_REV}");
            println!(
                "PSoXide hydrated tree: {} ({})",
                root.join(".psoxide").display(),
                sdk.describe()
            );
        }
        Action::Assets => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, true)?;
        }
        Action::Compile => {
            if cooked_assets_complete(&root) {
                validate_cooked_episode(&root)?;
                println!("validated Rust PSB indexes for Start and E1M1-E1M8");
            }
            build_game(&root, None, false)?;
            println!("PS1 executable: {}", game_exe(&root).display());
        }
        Action::Build | Action::Disc => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            verify_psoxide_rev_on_main()?;
            let provenance = capture_shipping_inputs(&root, &sdk, &pak)?;
            invalidate_shipping_provenance(&root)?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide");
            build_disc(&root, &build, None, true)?;
            package_dist(&root, &build, &sdk, &pak, &provenance)?;
        }
        Action::ShipBoot => {
            // Regression features omit the intro, menu and music. Test the
            // normal release configuration separately because its heap use is
            // higher.
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-ship-boot");
            fs::create_dir_all(&build)?;
            let map = build.join("quake-psx.map");
            request_guest_link_map(map.clone())?;
            build_disc(&root, &build, None, true)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_ship_boot(&root, &frontend, &build, &map)?;
        }
        Action::MapRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-map-regression");
            build_disc(&root, &build, Some("episode1-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_map_regression(&root, &frontend, &build)?;
        }
        Action::StartRouteRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-start-route-regression");
            build_disc(&root, &build, Some("start-route-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_start_route_regression(&root, &frontend, &build)?;
        }
        Action::VisualParityRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-visual-parity-regression");
            build_disc(&root, &build, Some("visual-parity-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_visual_parity_regression(&root, &frontend, &build)?;
        }
        Action::E1m1ChainRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-chain-regression");
            build_disc(&root, &build, Some("e1m1-chain-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-chain-regression")?;
        }
        Action::E1m1ChainBench => {
            // Step the route by three ticks per frame so renderer changes do
            // not alter the path. Use the normal route for release timing.
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-chain-bench");
            build_disc(
                &root,
                &build,
                Some("e1m1-chain-regression,perf-fixed-ticks"),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-chain-bench")?;
        }
        Action::E1m1SelectionCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-selection-cache-bench");
            build_disc(
                &root,
                &build,
                Some("e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache"),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-selection-cache-bench")?;
        }
        Action::E1m1TopologyCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-topology-cache-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-topology-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-topology-cache-bench")?;
        }
        Action::E1m1IndexedProjectionBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-indexed-projection-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-indexed-projection",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-indexed-projection-bench")?;
        }
        Action::E1m1AabbOffsetsBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-aabb-offsets-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-aabb-support-offsets",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-aabb-offsets-bench")?;
        }
        Action::E1m1RelaxedQuadPairingBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-relaxed-quad-pairing-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-relaxed-quad-pairing",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-relaxed-quad-pairing-bench")?;
        }
        Action::E1m1SharedSubdivisionEdgesBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-shared-subdivision-edges-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-shared-subdivision-edges",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-shared-subdivision-edges-bench",
            )?;
        }
        Action::E1m1Level0FastPathBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-level0-fast-path-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-level0-fast-path",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-level0-fast-path-bench")?;
        }
        Action::E1m1SpeculativeLevel0Bench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-speculative-level0-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-speculative-level0",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-speculative-level0-bench")?;
        }
        Action::E1m1StaticWorldReuseBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-static-world-reuse-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-static-world-reuse",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-static-world-reuse-bench")?;
        }
        Action::E1m1HoistedIndexedWorldBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-hoisted-indexed-world-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-hoisted-indexed-world",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-hoisted-indexed-world-bench",
            )?;
        }
        Action::E1m1DepthOnlySubdivisionBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-depth-only-subdivision-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-depth-only-subdivision",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-depth-only-subdivision-bench",
            )?;
        }
        Action::E1m1GteOtzBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gte-otz-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gte-otz",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-gte-otz-bench")?;
        }
        Action::E1m1CompactSubdivisionEmittersBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-compact-subdivision-emitters-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-compact-subdivision-emitters",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-compact-subdivision-emitters-bench",
            )?;
        }
        Action::E1m1CompactSubdivisionKernelsBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-compact-subdivision-kernels-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-compact-subdivision-kernels",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-compact-subdivision-kernels-bench",
            )?;
        }
        Action::E1m1CompactLevel2KernelBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-compact-level2-kernel-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-compact-level2-kernel",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-compact-level2-kernel-bench",
            )?;
        }
        Action::E1m1CompactWorldLevel2KernelBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-compact-world-level2-kernel-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-compact-world-level2-kernel",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-compact-world-level2-kernel-bench",
            )?;
        }
        Action::E1m1GpuLatticeClipBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-lattice-clip-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-lattice-clip",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-gpu-lattice-clip-bench")?;
        }
        Action::E1m1FixedFanQuadsBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-fixed-fan-quads-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-fixed-fan-quads",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-fixed-fan-quads-bench")?;
        }
        Action::E1m1FixedFanGuardedBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-fixed-fan-guarded-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-fixed-fan-guarded",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-fixed-fan-guarded-bench")?;
        }
        Action::E1m1FixedFanLevel2Bench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-fixed-fan-level2-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-fixed-fan-level2",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-fixed-fan-level2-bench")?;
        }
        Action::E1m1SubdivisionCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-subdivision-cache-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-subdivision-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-subdivision-cache-bench")?;
        }
        Action::E1m1SubdivisionCacheLevel2Bench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-subdivision-cache-level2-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-subdivision-cache-level2",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-subdivision-cache-level2-bench",
            )?;
        }
        Action::E1m1GpuPolygonClipBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-clip-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-gpu-polygon-clip-bench")?;
        }
        Action::E1m1GpuPolygonDepthOnlyBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-depth-only-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-depth-only-subdivision",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-depth-only-bench",
            )?;
        }
        Action::E1m1GpuPolygonCompactOtBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-compact-ot-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-compact-ot-256",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-compact-ot-bench",
            )?;
        }
        Action::E1m1GpuPolygonFusedProjectionBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-fused-projection-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-fused-materialize-project",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-fused-projection-bench",
            )?;
        }
        Action::E1m1GpuPolygonPlaneIndexBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-plane-index-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-plane-index-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-plane-index-bench",
            )?;
        }
        Action::E1m1GpuPolygonWindowRunsBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-window-runs-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-window-run-coalescing",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-window-runs-bench",
            )?;
        }
        Action::E1m1GpuPolygonWindowInsertBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-window-insert-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-window-insert-coalescing",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-window-insert-bench",
            )?;
        }
        Action::E1m1GpuPolygonWindowRangeBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-window-range-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-window-range-coalescing",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-window-range-bench",
            )?;
        }
        Action::E1m1GpuPolygonCellStreamBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-cell-stream-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-compact-cell-stream",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-cell-stream-bench",
            )?;
        }
        Action::E1m1SubdivisionCacheLevel2SmallBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-subdivision-cache-level2-small-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-subdivision-cache-level2-small",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-subdivision-cache-level2-small-bench",
            )?;
        }
        Action::E1m1GpuPolygonCellPolicyBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-cell-policy-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-cell-policy-bench",
            )?;
        }
        Action::GpuPolygonCellPolicyDisc => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-gpu-polygon-cell-policy-playable");
            fs::create_dir_all(&build)?;
            let map = build.join("quake-psx.map");
            request_guest_link_map(map.clone())?;
            build_disc(
                &root,
                &build,
                Some(
                    "renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_ship_boot(&root, &frontend, &build, &map)?;
        }
        Action::E1m1GpuPolygonQuakeKernelBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-quake-kernel-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-quake-specialized-kernel",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-quake-kernel-bench",
            )?;
        }
        Action::E1m1GpuPolygonLevel0RunBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-level0-run-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-quake-level0-run",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-level0-run-bench",
            )?;
        }
        Action::E1m1GpuPolygonColdAdaptiveBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-cold-adaptive-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-quake-cold-adaptive",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-cold-adaptive-bench",
            )?;
        }
        Action::E1m1GpuPolygonColdLevel2Bench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-cold-level2-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-quake-cold-level2",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-cold-level2-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentStreamBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-resident-stream-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-subdivision-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-stream-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentLevel2StreamBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build =
                root.join("build-psoxide-e1m1-gpu-polygon-resident-level2-stream-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-subdivision-cache-level2-small",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-level2-stream-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentLevel2ScatterBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build =
                root.join("build-psoxide-e1m1-gpu-polygon-resident-level2-scatter-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-resident-level2-scatter",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-level2-scatter-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentLevel2ColdCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root
                .join("build-psoxide-e1m1-gpu-polygon-resident-level2-cold-cache-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-resident-level2-cold-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-level2-cold-cache-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentBaseCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-resident-base-cache-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-resident-base-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-base-cache-bench",
            )?;
        }
        Action::E1m1GpuPolygonResidentBaseCacheFastBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build =
                root.join("build-psoxide-e1m1-gpu-polygon-resident-base-cache-fast-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip,renderer-cell-policy,renderer-resident-base-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-gpu-polygon-resident-base-cache-fast-bench",
            )?;
        }
        Action::E1m1GpuSurfaceClipBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-surface-clip-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-surface-clip",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-gpu-surface-clip-bench")?;
        }
        Action::E1m1SubdivisionCacheLevel2LayoutControlBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build =
                root.join("build-psoxide-e1m1-subdivision-cache-level2-layout-control-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-subdivision-cache-level2-layout-control",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-subdivision-cache-level2-layout-control-bench",
            )?;
        }
        Action::E1m1BlockFrustumBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-block-frustum-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-block-frustum-bench")?;
        }
        Action::E1m1HierarchicalBlockFrustumBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-hierarchical-block-frustum-bench");
            fs::create_dir_all(&build)?;
            request_guest_link_map(build.join("quake-psx.map"))?;
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-hierarchical-block-frustum",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(
                &root,
                &frontend,
                &build,
                "e1m1-hierarchical-block-frustum-bench",
            )?;
        }
        Action::E1m1BlockFrustum32Bench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-block-frustum-32-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum-32",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-block-frustum-32-bench")?;
        }
        Action::E1m1PlaneRunCacheBench => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-plane-run-cache-bench");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-plane-run-cache",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-plane-run-cache-bench")?;
        }
        Action::BlockFrustumRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            let features = "renderer-selection-cache,renderer-block-frustum";

            let visual = root.join("build-psoxide-block-frustum-visual-regression");
            build_disc(
                &root,
                &visual,
                Some("visual-parity-regression,renderer-selection-cache,renderer-block-frustum"),
                false,
            )?;
            run_visual_parity_regression(&root, &frontend, &visual)?;

            let routes = root.join("build-psoxide-block-frustum-route-regression");
            build_disc(
                &root,
                &routes,
                Some("e1m2-e1m3-route-regression,renderer-selection-cache,renderer-block-frustum"),
                false,
            )?;
            run_e1m2_e1m3_route_regression(&root, &frontend, &routes)?;

            let shipping = root.join("build-psoxide-block-frustum-ship-boot");
            fs::create_dir_all(&shipping)?;
            let map = shipping.join("quake-psx.map");
            request_guest_link_map(map.clone())?;
            build_disc(&root, &shipping, Some(features), false)?;
            run_ship_boot(&root, &frontend, &shipping, &map)?;
        }
        Action::GpuPolygonClipRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            let features =
                "renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip";

            let visual = root.join("build-psoxide-gpu-polygon-clip-visual-regression");
            build_disc(
                &root,
                &visual,
                Some(
                    "visual-parity-regression,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip",
                ),
                false,
            )?;
            run_visual_parity_regression(&root, &frontend, &visual)?;

            let routes = root.join("build-psoxide-gpu-polygon-clip-route-regression");
            build_disc(
                &root,
                &routes,
                Some(
                    "e1m2-e1m3-route-regression,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip",
                ),
                false,
            )?;
            run_e1m2_e1m3_route_regression(&root, &frontend, &routes)?;

            let shipping = root.join("build-psoxide-gpu-polygon-clip-ship-boot");
            fs::create_dir_all(&shipping)?;
            let map = shipping.join("quake-psx.map");
            request_guest_link_map(map.clone())?;
            build_disc(&root, &shipping, Some(features), false)?;
            run_ship_boot(&root, &frontend, &shipping, &map)?;
        }
        Action::SelectionCacheRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;

            let visual = root.join("build-psoxide-selection-cache-visual-regression");
            build_disc(
                &root,
                &visual,
                Some("visual-parity-regression,renderer-selection-cache"),
                false,
            )?;
            run_visual_parity_regression(&root, &frontend, &visual)?;

            let routes = root.join("build-psoxide-selection-cache-route-regression");
            build_disc(
                &root,
                &routes,
                Some("e1m2-e1m3-route-regression,renderer-selection-cache"),
                false,
            )?;
            run_e1m2_e1m3_route_regression(&root, &frontend, &routes)?;
        }
        Action::SelectionCacheShipBoot => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            let shipping = root.join("build-psoxide-selection-cache-ship-boot");
            fs::create_dir_all(&shipping)?;
            let map = shipping.join("quake-psx.map");
            request_guest_link_map(map.clone())?;
            build_disc(&root, &shipping, Some("renderer-selection-cache"), false)?;
            run_ship_boot(&root, &frontend, &shipping, &map)?;
        }
        Action::E1m1RendererCensus => {
            // This image performs extra renderer passes and logs every frame.
            // It follows the deterministic benchmark route only to make two
            // captures directly comparable; its timing is intentionally
            // meaningless.
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-renderer-census");
            build_disc(
                &root,
                &build,
                Some("e1m1-chain-regression,perf-fixed-ticks,renderer-census"),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_chain_regression(&root, &frontend, &build, "e1m1-renderer-census")?;
            println!(
                "renderer census logs: {}",
                root.join("captures/e1m1-renderer-census").display()
            );
        }
        Action::E1m1GpuCensus => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-census");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_gpu_census(&root, &frontend, &build)?;
        }
        Action::E1m1GpuPolygonCensus => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m1-gpu-polygon-census");
            build_disc(
                &root,
                &build,
                Some(
                    "e1m1-chain-regression,perf-fixed-ticks,renderer-selection-cache,renderer-block-frustum,renderer-gpu-polygon-clip",
                ),
                false,
            )?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m1_gpu_census_named(&root, &frontend, &build, "e1m1-gpu-polygon-census")?;
        }
        Action::E1m2E1m3RouteRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-e1m2-e1m3-route-regression");
            build_disc(&root, &build, Some("e1m2-e1m3-route-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_e1m2_e1m3_route_regression(&root, &frontend, &build)?;
        }
        Action::SurvivalRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-survival-regression");
            build_disc(&root, &build, Some("survival-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            assert_authored_survival_sources(&root)?;
            run_survival_regression(&root, &frontend, &build)?;
        }
        Action::SystemsRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-systems-regression");
            build_disc(&root, &build, Some("systems-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_systems_regression(&root, &frontend, &build)?;
        }
        Action::CombatRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-combat-regression");
            build_disc(&root, &build, Some("combat-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_combat_regression(&root, &frontend, &build)?;
        }
        Action::BestiaryRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-bestiary-regression");
            build_disc(&root, &build, Some("bestiary-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_bestiary_regression(&root, &frontend, &build)?;
        }
        Action::Episode1Regress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-episode1-regression");
            build_disc(&root, &build, Some("episode1-route-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_episode1_regression(&root, &frontend, &build)?;
        }
        Action::MonsterRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-monster-regression");
            build_disc(&root, &build, Some("monster-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_monster_regression(&root, &frontend, &build)?;
        }
        Action::MonsterJumpRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-monsterjump-regression");
            build_disc(&root, &build, Some("monsterjump-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_monsterjump_regression(&root, &frontend, &build)?;
        }
        Action::ArsenalRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-arsenal-regression");
            build_disc(&root, &build, Some("arsenal-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_arsenal_regression(&root, &frontend, &build)?;
        }
        Action::AudioRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-audio-regression");
            build_disc(&root, &build, Some("audio-residency-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_audio_regression(&root, &frontend, &build)?;
        }
        Action::AmbientRegress => {
            let pak = resolve_pak(&root, cli.quake_dir.as_deref())?;
            cook_assets(&root, &pak, false)?;
            let build = root.join("build-psoxide-ambient-regression");
            build_disc(&root, &build, Some("ambient-regression"), false)?;
            let frontend = resolve_frontend(&root, cli.psoxide.as_deref())?;
            run_ambient_regression(&root, &frontend, &build)?;
        }
        Action::Regress => {
            return Err(
                "full Episode 1 regression is disabled until its Rust gameplay probe is complete"
                    .into(),
            );
        }
        Action::Hardware => {
            return Err(
                "hardware acceptance image is disabled until its Rust gameplay probe is complete"
                    .into(),
            );
        }
    }
    Ok(())
}

fn validate_source_episode(root: &Path, pak_path: &Path) -> Result<()> {
    let pak_bytes = fs::read(pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    let entity_map = fs::read_to_string(root.join("tools/cfg/id1/entmap.txt"))?;
    let model_map = fs::read_to_string(root.join("tools/cfg/id1/mdlmap.txt"))?;
    let resource_list = fs::read_to_string(root.join("tools/cfg/id1/reslist.txt"))?;
    let mut census = PoolCensus::default();
    for map in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ] {
        let bsp = Bsp::parse(pak.require(&format!("maps/{map}.bsp"))?)?;
        let stats = bsp.stats();
        if stats.faces == 0 || stats.models == 0 || bsp.lump(BspLump::Entities).is_empty() {
            return Err(format!("maps/{map}.bsp is incomplete").into());
        }
        for texture in 0..bsp.texture_count() {
            let _ = bsp.mip_texture(texture)?;
        }
        let cooked = cook_entities(&bsp, &entity_map, &model_map, &resource_list)?;
        validate_level_title(map, &bsp)?;
        census.merge(validate_canonical_target_contract(map, &bsp, &cooked)?);
    }
    println!("Rust-validated Quake BSP29 and bounded target contracts for Start and E1M1-E1M8");
    println!(
        "guest pool worst cases across Episode 1: triggers {}/{GUEST_MAX_TRIGGERS}, movers {}/{GUEST_MAX_MOVERS}, trains {}/{GUEST_MAX_TRAINS}, teleports {}/{GUEST_MAX_TELEPORTS}, fireball emitters {}/{GUEST_MAX_FIREBALL_EMITTERS}, fireballs {}/{GUEST_MAX_FIREBALLS}, render entities {}/{GUEST_MAX_RENDER_ENTITIES}, target fan-out {}/{GUEST_MAX_TARGET_ACTIONS}, linked doors {}/{GUEST_MAX_PLAYER_ACTIVATIONS}, killable monsters {}",
        census.triggers,
        census.movers,
        census.trains,
        census.teleports,
        census.emitters,
        census.emitters * FIREBALLS_PER_EMITTER,
        census.render,
        census.fan_out,
        census.door_group,
        census.monsters,
    );
    Ok(())
}

fn validate_canonical_target_contract(
    map: &str,
    bsp: &Bsp<'_>,
    cooked: &quake_cook::CookedEntities,
) -> Result<PoolCensus> {
    let source = cooked.source_entities();
    if cooked.runtime_entity_count() > MAX_TARGET_ENTITIES {
        return Err(format!(
            "{map} cooks {} entities, target runtime capacity is {MAX_TARGET_ENTITIES}",
            cooked.runtime_entity_count(),
        )
        .into());
    }
    let runtime = RecordSlice::<MapEntity>::new(&cooked.entities)
        .ok_or_else(|| format!("{map} cooked entity table is malformed"))?;
    let census = validate_runtime_pools(map, bsp, &runtime)?;
    for exclusion in [0x0100, 0x0200, 0x0400] {
        for source_index in 0..runtime.len() {
            let mut graph = TargetGraph::new();
            graph
                .load(&runtime)
                .map_err(|error| format!("{map} target graph load failed: {error:?}"))?;
            for (index, entity) in runtime.iter().enumerate() {
                if entity.spawn_flags & exclusion != 0 {
                    graph.disable_entity(index as u16).map_err(|error| {
                        format!("{map} target difficulty filter failed: {error:?}")
                    })?;
                }
            }
            let mut actions = TargetActions::new();
            graph
                .fire_source(&runtime, source_index as u16, &mut actions)
                .map_err(|error| {
                    format!("{map} target source {source_index} failed bounded dispatch: {error:?}")
                })?;
            actions.clear();
            graph
                .tick(u16::MAX, &runtime, &mut actions)
                .map_err(|error| {
                    format!("{map} delayed target source {source_index} failed: {error:?}")
                })?;
        }
    }
    let delayed = source
        .iter()
        .filter(|entity| {
            entity
                .get("delay")
                .and_then(|value| value.parse::<f32>().ok())
                .is_some_and(|delay| delay > 0.0)
        })
        .count();
    if delayed > MAX_DELAYED_USES {
        return Err(format!(
            "{map} has {delayed} authored delayed uses, capacity is {MAX_DELAYED_USES}"
        )
        .into());
    }
    for teleport in source
        .iter()
        .filter(|entity| entity.class_name() == "trigger_teleport")
    {
        let excluded_from_single_player = teleport
            .get("spawnflags")
            .and_then(|value| value.parse::<u16>().ok())
            .is_some_and(|flags| flags & 0x0700 == 0x0700);
        if excluded_from_single_player {
            continue;
        }
        let target = teleport
            .get("target")
            .ok_or_else(|| format!("{map} has a trigger_teleport without target"))?;
        let destination_classes = source
            .iter()
            .filter(|entity| {
                matches!(
                    entity.class_name(),
                    "info_teleport_destination" | "info_null"
                ) && entity.get("targetname") == Some(target)
            })
            .map(|entity| entity.class_name())
            .collect::<Vec<_>>();
        if destination_classes.len() != 1 {
            return Err(format!(
                "{map} teleporter target {target:?} resolves to {} destinations",
                destination_classes.len(),
            )
            .into());
        }
    }
    for entity in source
        .iter()
        .filter(|entity| matches!(entity.class_name(), "trigger_relay" | "trigger_counter"))
    {
        let Some(target) = entity.get("target") else {
            continue;
        };
        if !source
            .iter()
            .any(|candidate| candidate.get("targetname") == Some(target))
        {
            return Err(format!(
                "{map} {} target {target:?} is dangling",
                entity.class_name()
            )
            .into());
        }
    }

    match map {
        "start" => {
            require_source_count(map, source, "trigger_setskill", 4)?;
            require_source_count(map, source, "trigger_teleport", 9)?;
            require_source_count(map, source, "info_teleport_destination", 4)?;
            require_source_entity(
                map,
                source,
                "info_player_start",
                &[("origin", "544 288 32")],
            )?;
            require_source_entity(
                map,
                source,
                "trigger_setskill",
                &[("model", "*9"), ("message", "0")],
            )?;
            require_source_entity(
                map,
                source,
                "info_teleport_destination",
                &[("targetname", "t1"), ("origin", "544 1536 16")],
            )?;
            require_source_entity(map, source, "trigger_changelevel", &[("map", "e1m1")])?;
        }
        "e1m1" => {
            for (index, class_name, model, spawn_flags) in [
                (6, 0x0c, -3, 0),
                (11, 0x0c, -8, 1),
                (12, 0x0b, -9, 0),
                (35, 0x0b, -19, 0),
                (52, 0x0b, -25, 2048),
                (53, 0x0b, -26, 2048),
                (54, 0x0b, -27, 2048),
                (55, 0x48, -28, 0),
                (56, 0x0c, -29, 2048),
                (69, 0x0c, -38, 2048),
                (70, 0x4c, -39, 2048),
            ] {
                require_runtime_entity(map, &runtime, index, class_name, model, spawn_flags)?;
            }
            let counter = runtime.get(55).ok_or("e1m1 runtime counter vanished")?;
            let exit_door = runtime.get(56).ok_or("e1m1 runtime exit door vanished")?;
            let message_door = runtime
                .get(69)
                .ok_or("e1m1 runtime message door vanished")?;
            if counter.count != 3
                || counter.target == 0
                || counter.target_name == 0
                || exit_door.target_name != counter.target
                || exit_door.string == 0
                || message_door.target_name == 0
                || message_door.string == 0
            {
                return Err("e1m1 cooked runtime target/message identities changed".into());
            }
            let buttons = source
                .iter()
                .filter(|entity| {
                    entity.class_name() == "func_button" && entity.get("target") == Some("t9")
                })
                .count();
            if buttons != 3 {
                return Err(format!("e1m1 three-button chain has {buttons} buttons").into());
            }
            require_source_entity_at(
                map,
                source,
                214,
                "trigger_counter",
                &[
                    ("model", "*28"),
                    ("targetname", "t9"),
                    ("target", "t10"),
                    ("count", "3"),
                ],
            )?;
            for (index, model, angle) in
                [(211, "*25", "0"), (212, "*26", "90"), (213, "*27", "270")]
            {
                require_source_entity_at(
                    map,
                    source,
                    index,
                    "func_button",
                    &[
                        ("model", model),
                        ("target", "t9"),
                        ("angle", angle),
                        ("wait", "-1"),
                        ("spawnflags", "2048"),
                    ],
                )?;
            }
            require_source_entity_at(
                map,
                source,
                215,
                "func_door",
                &[
                    ("model", "*29"),
                    ("targetname", "t10"),
                    ("angle", "180"),
                    ("wait", "-1"),
                    ("spawnflags", "2048"),
                    ("message", "You must press the three buttons..."),
                ],
            )?;
            require_source_entity_at(
                map,
                source,
                234,
                "func_door",
                &[
                    ("model", "*38"),
                    ("targetname", "t15"),
                    ("wait", "-1"),
                    ("spawnflags", "2048"),
                    ("message", "This door opens elsewhere..."),
                ],
            )?;
            require_source_entity_at(
                map,
                source,
                235,
                "trigger_once",
                &[("model", "*39"), ("target", "t15"), ("spawnflags", "2048")],
            )?;
            require_source_entity_at(
                map,
                source,
                83,
                "func_door",
                &[
                    ("model", "*8"),
                    ("targetname", "t2"),
                    ("angle", "270"),
                    ("speed", "600"),
                    ("wait", "-1"),
                    ("spawnflags", "1"),
                ],
            )?;
            require_source_entity_at(
                map,
                source,
                88,
                "func_button",
                &[
                    ("model", "*9"),
                    ("target", "t2"),
                    ("angle", "270"),
                    ("wait", "-1"),
                ],
            )?;
            require_source_entity_at(
                map,
                source,
                188,
                "func_plat",
                &[("model", "*22"), ("height", "400"), ("spawnflags", "1")],
            )?;
            for (index, model) in [(366, "*56"), (367, "*57")] {
                require_source_entity_at(
                    map,
                    source,
                    index,
                    "func_wall",
                    &[("model", model), ("spawnflags", "1792")],
                )?;
            }
            for (model, mins, maxs) in [
                (8, [1, 2633, -95], [319, 2879, -81]),
                (22, [753, 481, 65], [831, 543, 79]),
                (25, [1289, 2033, -207], [1295, 2063, -177]),
                (26, [1217, 2505, -271], [1247, 2511, -241]),
                (27, [785, 1985, -143], [815, 1991, -113]),
                (28, [793, 1937, -143], [807, 1951, -113]),
                (29, [753, 1873, -431], [895, 1887, -321]),
                (38, [1089, 961, -271], [1103, 1087, -161]),
                (39, [1249, 1121, -263], [1383, 1135, -153]),
            ] {
                require_brush_bounds(map, bsp, model, mins, maxs)?;
            }
        }
        "e1m6" => {
            require_source_count(map, source, "trigger_monsterjump", 1)?;
            require_source_entity_at(map, source, 367, "trigger_monsterjump", &[("model", "*83")])?;
            let authored = source
                .get(367)
                .ok_or("e1m6 canonical monster-jump source vanished")?;
            if authored.get("speed").is_some()
                || authored.get("height").is_some()
                || authored.get("angle").is_some()
                || authored.get("angles").is_some()
            {
                return Err(
                    "e1m6 monster-jump no longer uses Quake's 200/200 yaw-zero defaults".into(),
                );
            }
            require_runtime_entity(map, &runtime, 192, 0x4a, -83, 0)?;
            let cooked = runtime
                .get(192)
                .ok_or("e1m6 cooked monster-jump source vanished")?;
            if cooked.speed != 0
                || cooked.height != 0
                || cooked.angles != quake_formats::Vec3I16::default()
            {
                return Err("e1m6 cooked monster-jump defaults changed".into());
            }
            require_brush_bounds(map, bsp, 83, [-511, 1473, 65], [-193, 1479, 127])?;
        }
        "e1m7" => {
            require_source_count(map, source, "trigger_relay", 2)?;
            require_source_entity(map, source, "event_lightning", &[("targetname", "t14")])?;
            require_source_entity(
                map,
                source,
                "trigger_changelevel",
                &[("map", "start"), ("target", "t18"), ("model", "*14")],
            )?;
            require_source_entity(
                map,
                source,
                "trigger_relay",
                &[("targetname", "t18"), ("target", "t16")],
            )?;
        }
        _ => {}
    }
    Ok(census)
}

fn require_source_count(
    map: &str,
    source: &[quake_cook::SourceEntity],
    class_name: &str,
    expected: usize,
) -> Result<()> {
    let actual = source
        .iter()
        .filter(|entity| entity.class_name() == class_name)
        .count();
    if actual != expected {
        return Err(
            format!("{map} has {actual} {class_name} entities, expected {expected}").into(),
        );
    }
    Ok(())
}

fn require_source_entity(
    map: &str,
    source: &[quake_cook::SourceEntity],
    class_name: &str,
    fields: &[(&str, &str)],
) -> Result<()> {
    if source.iter().any(|entity| {
        entity.class_name() == class_name
            && fields
                .iter()
                .all(|(key, expected)| entity.get(key) == Some(*expected))
    }) {
        return Ok(());
    }
    Err(format!("{map} lacks canonical {class_name} fields {fields:?}").into())
}

fn require_source_entity_at(
    map: &str,
    source: &[quake_cook::SourceEntity],
    index: usize,
    class_name: &str,
    fields: &[(&str, &str)],
) -> Result<()> {
    let Some(entity) = source.get(index) else {
        return Err(format!("{map} lacks canonical source entity {index}").into());
    };
    if entity.class_name() != class_name
        || !fields
            .iter()
            .all(|(key, expected)| entity.get(key) == Some(*expected))
    {
        return Err(format!(
            "{map} source entity {index} is not canonical {class_name} fields {fields:?}"
        )
        .into());
    }
    Ok(())
}

fn require_runtime_entity(
    map: &str,
    runtime: &RecordSlice<'_, MapEntity>,
    index: usize,
    class_name: u8,
    model: i16,
    spawn_flags: u16,
) -> Result<()> {
    let entity = runtime
        .get(index)
        .ok_or_else(|| format!("{map} lacks cooked runtime entity {index}"))?;
    if entity.class_name != class_name || entity.model != model || entity.spawn_flags != spawn_flags
    {
        return Err(format!(
            "{map} cooked runtime entity {index} changed: class=0x{:02x} model={} spawnflags={}, expected class=0x{class_name:02x} model={model} spawnflags={spawn_flags}",
            entity.class_name, entity.model, entity.spawn_flags,
        )
        .into());
    }
    Ok(())
}

fn require_brush_bounds(
    map: &str,
    bsp: &Bsp<'_>,
    model: usize,
    expected_mins: [i32; 3],
    expected_maxs: [i32; 3],
) -> Result<()> {
    let bytes = bsp
        .lump(BspLump::Models)
        .get(model * 64..model * 64 + 64)
        .ok_or_else(|| format!("{map} lacks BSP model *{model}"))?;
    let value =
        |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as i32;
    let mins = [value(0), value(4), value(8)];
    let maxs = [value(12), value(16), value(20)];
    if mins != expected_mins || maxs != expected_maxs {
        return Err(format!(
            "{map} BSP model *{model} bounds changed: {mins:?}..{maxs:?}, expected {expected_mins:?}..{expected_maxs:?}"
        )
        .into());
    }
    Ok(())
}

fn validate_geometry_parity(root: &Path, pak_path: &Path) -> Result<()> {
    let pak_bytes = fs::read(pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    let entity_map = fs::read_to_string(root.join("tools/cfg/id1/entmap.txt"))?;
    let model_map = fs::read_to_string(root.join("tools/cfg/id1/mdlmap.txt"))?;
    let model_props = fs::read_to_string(root.join("tools/cfg/id1/mdlprops.txt"))?;
    let sound_map = fs::read_to_string(root.join("tools/cfg/id1/sfxmap.txt"))?;
    let resource_list = fs::read_to_string(root.join("tools/cfg/id1/reslist.txt"))?;
    let global_sounds = cook_global_sounds(&pak, &sound_map, &resource_list)?;
    let cooked_global = fs::read(root.join("id1psx/sounds/global.qsb"))?;
    validate_sound_bank("global", &cooked_global, &global_sounds.data)?;
    let mut ambient_emitters = 0usize;
    let mut monster_census: Vec<String> = Vec::new();
    let mut worst_monsters = 0usize;
    let mut worst_render_slots = 0usize;
    let mut worst_cluster = 0usize;
    let mut ambient_maps = Vec::new();
    let mut layered_sky_textures = 0usize;
    let mut layered_sky_faces = 0usize;
    let mut layered_sky_maps = Vec::new();
    for map in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ] {
        let source = Bsp::parse(pak.require(&format!("maps/{map}.bsp"))?)?;
        let rust_entities = cook_entities(&source, &entity_map, &model_map, &resource_list)?;
        if map == "e1m1" {
            validate_e1m1_monster_sources(&rust_entities.entities)?;
        }
        let (rust, rust_models) = cook_geometry_and_models(
            &source,
            &pak,
            &rust_entities,
            &model_map,
            &resource_list,
            &model_props,
            SkyEncoding::Layered,
        )?;
        let (map_sky_textures, map_sky_faces) =
            validate_layered_sky(map, &rust.texture_info, &rust.faces, &rust.texture_data)?;
        layered_sky_textures += map_sky_textures;
        layered_sky_faces += map_sky_faces;
        if map_sky_textures != 0 {
            layered_sky_maps.push(format!(
                "{map}:{map_sky_textures} textures/{map_sky_faces} faces"
            ));
        }
        let cooked_bytes = fs::read(root.join(format!("id1psx/maps/{map}.psb")))?;
        let mut reader = SliceReader::new(&cooked_bytes);
        let index = PsbIndex::read(&mut reader).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        for (kind, rust_bytes) in geometry_lumps(&rust) {
            let range = index.lump(kind);
            let legacy = &cooked_bytes[range.offset as usize..range.end() as usize];
            if legacy != rust_bytes {
                let first = legacy
                    .iter()
                    .zip(rust_bytes)
                    .position(|(left, right)| left != right)
                    .unwrap_or(legacy.len().min(rust_bytes.len()));
                let start = first.saturating_sub(8);
                let legacy_end = (first + 16).min(legacy.len());
                let rust_end = (first + 16).min(rust_bytes.len());
                return Err(format!(
                    "Rust geometry parity failed for {map} {kind:?}: legacy={} Rust={}, first difference={first}, legacy={:02x?}, Rust={:02x?}",
                    legacy.len(),
                    rust_bytes.len(),
                    &legacy[start..legacy_end],
                    &rust_bytes[start..rust_end]
                )
                .into());
            }
        }
        for (kind, rust_bytes) in [
            (LumpKind::Strings, rust_entities.strings.as_slice()),
            (LumpKind::Entities, rust_entities.entities.as_slice()),
        ] {
            let range = index.lump(kind);
            let legacy = &cooked_bytes[range.offset as usize..range.end() as usize];
            if legacy != rust_bytes {
                let first = legacy
                    .iter()
                    .zip(rust_bytes)
                    .position(|(left, right)| left != right)
                    .unwrap_or(legacy.len().min(rust_bytes.len()));
                return Err(format!(
                    "Rust entity parity failed for {map} {kind:?}: legacy={} Rust={}, first difference={first}",
                    legacy.len(), rust_bytes.len()
                )
                .into());
            }
        }
        let texture_range = index.lump(LumpKind::TextureData);
        let legacy_textures =
            &cooked_bytes[texture_range.offset as usize..texture_range.end() as usize];
        if legacy_textures != rust.texture_data {
            return Err(format!(
                "Rust model atlas parity failed for {map}: legacy={} Rust={}",
                legacy_textures.len(),
                rust.texture_data.len()
            )
            .into());
        }
        let model_range = index.lump(LumpKind::ModelData);
        let legacy_models = &cooked_bytes[model_range.offset as usize..model_range.end() as usize];
        validate_model_parity(map, legacy_models, &rust_models.data)?;
        let rust_sounds = cook_sounds(
            &pak,
            &rust_entities,
            &sound_map,
            &resource_list,
            &global_sounds,
        )?;
        let range = index.lump(LumpKind::SoundData);
        let legacy = &cooked_bytes[range.offset as usize..range.end() as usize];
        validate_sound_bank(map, legacy, &rust_sounds.data)?;
        let map_ambients =
            validate_ambient_sounds(map, &rust_entities.entities, &rust_sounds.data)?;
        ambient_emitters += map_ambients;
        if map_ambients != 0 {
            ambient_maps.push(format!("{map}:{map_ambients}"));
        }
        let population =
            validate_monster_population(map, &rust_entities.entities, &rust_models.data)?;
        if population.monsters != 0 {
            monster_census.push(format!("{map}:{}", population.monsters));
        }
        worst_monsters = worst_monsters.max(population.monsters);
        worst_render_slots = worst_render_slots.max(population.render_slots);
        worst_cluster = worst_cluster.max(population.densest_cluster);
    }
    if layered_sky_textures == 0 || layered_sky_faces == 0 {
        return Err("Episode 1 contains no rendered layered sky".into());
    }
    println!(
        "authored Easy monster population fits the guest pools: worst map {worst_monsters} monsters, worst render slots {worst_render_slots}/{MAX_RENDER_ENTITIES}, densest body cluster {worst_cluster}/{MAX_BODY_CANDIDATES} ({})",
        monster_census.join(", ")
    );
    println!(
        "Rust geometry, entities, model headers/frames, atlas, and sound selection match Start and E1M1-E1M8; {layered_sky_textures} layered sky textures cover {layered_sky_faces} faces across {}; {ambient_emitters} ambient emitters have looped samples across {}; sound banks carry SDK parking tails, and equal-depth viewmodel triangle ties are order-independent",
        layered_sky_maps.join(", "),
        ambient_maps.join(", ")
    );
    Ok(())
}

fn validate_e1m1_monster_sources(entity_bytes: &[u8]) -> Result<()> {
    const NOT_EASY: u16 = 0x0100;
    const ARMY: u8 = 0x36;
    const DOG: u8 = 0x39;
    let entities = RecordSlice::<MapEntity>::new(entity_bytes)
        .ok_or("E1M1 has a malformed cooked entity table")?;
    let mut total_army = 0usize;
    let mut total_dog = 0usize;
    let mut easy_army = 0usize;
    let mut easy_dog = 0usize;
    for entity in entities.iter() {
        match entity.class_name {
            ARMY => {
                total_army += 1;
                easy_army += usize::from(entity.spawn_flags & NOT_EASY == 0);
            }
            DOG => {
                total_dog += 1;
                easy_dog += usize::from(entity.spawn_flags & NOT_EASY == 0);
            }
            _ => {}
        }
    }
    if (total_army, total_dog, easy_army, easy_dog) != (34, 8, 9, 1) {
        return Err(format!(
            "E1M1 Soldier/Dog source population drifted: total={total_army}/{total_dog}, Easy={easy_army}/{easy_dog}, expected 34/8 and 9/1"
        )
        .into());
    }
    for (index, class_name, origin) in [
        (21usize, ARMY, [248, 2_392, 40]),
        (82usize, DOG, [88, 1_520, -200]),
    ] {
        let entity = entities
            .get(index)
            .ok_or_else(|| format!("E1M1 source entity {index} is missing"))?;
        let actual_origin = [
            entity.origin.x >> 12,
            entity.origin.y >> 12,
            entity.origin.z >> 12,
        ];
        if entity.class_name != class_name
            || entity.spawn_flags & NOT_EASY != 0
            || actual_origin != origin
        {
            return Err(format!(
                "E1M1 source entity {index} drifted: class=0x{:02x} flags=0x{:04x} origin={actual_origin:?}",
                entity.class_name, entity.spawn_flags
            )
            .into());
        }
    }
    Ok(())
}

fn validate_layered_sky(
    map: &str,
    texture_bytes: &[u8],
    face_bytes: &[u8],
    atlas: &[u8],
) -> Result<(usize, usize)> {
    const ATLAS_VRAM_X_WORDS: usize = 320;
    const ATLAS_ROW_BYTES: usize = 640 * 2;

    let textures = RecordSlice::<TextureInfo>::new(texture_bytes)
        .ok_or_else(|| format!("{map} has a partial texture-info record"))?;
    let faces = RecordSlice::<Face>::new(face_bytes)
        .ok_or_else(|| format!("{map} has a partial face record"))?;
    let mut sky_textures = 0usize;
    for texture_index in 0..textures.len() {
        let texture = textures.get(texture_index).expect("bounded texture index");
        let is_sky = texture.flags & TEXTURE_SKY != 0;
        let is_layered = texture.flags & TEXTURE_LAYERED_SKY != 0;
        if is_layered && !is_sky {
            return Err(format!(
                "{map} texture {texture_index} sets layered-sky without the sky flag"
            )
            .into());
        }
        if !is_sky {
            continue;
        }
        if !is_layered {
            return Err(format!("{map} texture {texture_index} is a flattened sky").into());
        }

        let layer_width = usize::try_from(texture.size.x)
            .map_err(|_| format!("{map} sky texture {texture_index} has negative width"))?;
        let height = usize::try_from(texture.size.y)
            .map_err(|_| format!("{map} sky texture {texture_index} has negative height"))?;
        if !(8..=128).contains(&layer_width)
            || !layer_width.is_power_of_two()
            || !(8..=128).contains(&height)
            || !height.is_power_of_two()
            || texture.atlas.x as usize % layer_width != 0
            || texture.atlas.y as usize % height != 0
            || texture.atlas.x as usize + layer_width * 2 > 256
            || texture.atlas.y as usize + height > 256
        {
            return Err(format!(
                "{map} sky texture {texture_index} cannot form two aligned {layer_width}x{height} texture windows at {},{}",
                texture.atlas.x, texture.atlas.y
            )
            .into());
        }

        let tpage_x_words = usize::from(texture.texture_page & 0x0f) * 64;
        let tpage_y = usize::from((texture.texture_page >> 4) & 1) * 256;
        let byte_x = tpage_x_words
            .checked_sub(ATLAS_VRAM_X_WORDS)
            .and_then(|value| value.checked_mul(2))
            .and_then(|value| value.checked_add(texture.atlas.x as usize))
            .ok_or_else(|| format!("{map} sky texture {texture_index} is outside the atlas"))?;
        if byte_x + layer_width * 2 > ATLAS_ROW_BYTES {
            return Err(format!(
                "{map} sky texture {texture_index} crosses the streamed atlas row"
            )
            .into());
        }

        let mut foreground_transparent = 0usize;
        let mut foreground_opaque = 0usize;
        let mut background_transparent = 0usize;
        for row in 0..height {
            let y = tpage_y + texture.atlas.y as usize + row;
            let start = y
                .checked_mul(ATLAS_ROW_BYTES)
                .and_then(|value| value.checked_add(byte_x))
                .ok_or_else(|| format!("{map} sky texture {texture_index} atlas range overflow"))?;
            let end = start + layer_width * 2;
            let pixels = atlas.get(start..end).ok_or_else(|| {
                format!("{map} sky texture {texture_index} exceeds the cooked atlas")
            })?;
            foreground_transparent += pixels[..layer_width]
                .iter()
                .filter(|&&pixel| pixel == 0xff)
                .count();
            foreground_opaque += pixels[..layer_width]
                .iter()
                .filter(|&&pixel| pixel != 0xff)
                .count();
            background_transparent += pixels[layer_width..]
                .iter()
                .filter(|&&pixel| pixel == 0xff)
                .count();
        }
        if foreground_transparent == 0 || foreground_opaque == 0 {
            return Err(format!(
                "{map} sky texture {texture_index} foreground is not a mixed transparent layer"
            )
            .into());
        }
        if background_transparent != 0 {
            return Err(format!(
                "{map} sky texture {texture_index} background contains {background_transparent} transparent texels"
            )
            .into());
        }
        sky_textures += 1;
    }

    let mut sky_faces = 0usize;
    for face_index in 0..faces.len() {
        let face = faces.get(face_index).expect("bounded face index");
        let texture_index = usize::try_from(face.texture)
            .map_err(|_| format!("{map} face {face_index} has a negative texture index"))?;
        let texture = textures
            .get(texture_index)
            .ok_or_else(|| format!("{map} face {face_index} has an invalid texture index"))?;
        if texture.flags & TEXTURE_SKY != 0 {
            if texture.flags & TEXTURE_LAYERED_SKY == 0 {
                return Err(format!("{map} face {face_index} references a flattened sky").into());
            }
            sky_faces += 1;
        }
    }
    Ok((sky_textures, sky_faces))
}

fn validate_sound_bank(map: &str, actual: &[u8], expected: &[u8]) -> Result<()> {
    if actual != expected {
        let first = actual
            .iter()
            .zip(expected)
            .position(|(left, right)| left != right)
            .unwrap_or(actual.len().min(expected.len()));
        return Err(format!(
            "Rust sound bank changed for {map}: actual={}, expected={}, first difference={first}",
            actual.len(),
            expected.len()
        )
        .into());
    }
    let (header, records, _, payload) = decode_sound_bank(expected)
        .map_err(|error| format!("versioned sound bank is malformed for {map}: {error:?}"))?;
    if map == "global" {
        if header.kind != SoundBankKind::Global || records.len() != SOUND_GLOBAL_EFFECTS {
            return Err("persistent global sound bank has the wrong kind or record count".into());
        }
    } else if header.kind != SoundBankKind::Local {
        return Err(format!("{map} sound lump is not a local suffix").into());
    }
    if header.spu_high_water > SOUND_SPU_END {
        return Err(format!("{map} sound bank exceeds SPU RAM").into());
    }
    for index in 0..records.len() {
        let effect = records.get(index).ok_or("Rust sound record missing")?;
        let payload_start = effect
            .spu_address
            .checked_sub(header.payload_base)
            .ok_or("Rust sound starts below the SPU bank")? as usize;
        let end = if index + 1 < records.len() {
            let next = records
                .get(index + 1)
                .ok_or("Rust next sound record missing")?;
            next.spu_address
                .checked_sub(header.payload_base)
                .ok_or("Rust sound starts below the SPU bank")? as usize
        } else {
            payload.len()
        };
        let tail_start = end
            .checked_sub(SFX_PARKING_TAIL.len())
            .filter(|tail| *tail >= payload_start)
            .ok_or("Rust sound is smaller than its parking tail")?;
        if payload.get(tail_start..end) != Some(SFX_PARKING_TAIL.as_slice()) {
            return Err(
                format!("Rust sound parking tail is missing for {map} record {index}").into(),
            );
        }
    }
    Ok(())
}

/// Guest render-entity pool size (`game/src/entity.rs`).
const MAX_RENDER_ENTITIES: usize = 384;
/// Per-trace dynamic body candidate ceiling (`quake_core::body`).
const MAX_BODY_CANDIDATES: usize = 16;
/// Fixed projectile render slots the guest installs in every map: eight
/// rockets, sixty nails, eight grenades, twelve monster missiles.
const PROJECTILE_RENDER_SLOTS: usize = 8 + 60 + 8 + 12;

struct MonsterPopulation {
    monsters: usize,
    render_slots: usize,
    densest_cluster: usize,
}

/// Prove, from the cooked entity and model lumps of a real map, that the
/// authored Easy monster population fits every fixed guest pool it touches:
/// the render-entity table, the per-trace body candidate set, and the alias
/// model each monster's authored frame ranges index into.
fn validate_monster_population(
    map: &str,
    entity_bytes: &[u8],
    model_bytes: &[u8],
) -> Result<MonsterPopulation> {
    const NOT_EASY: u16 = 0x0100;
    // The body broad phase in `EntityScene::monster_step_bodies` keeps every
    // candidate within one step plus the largest body and hull.
    const CLUSTER_UNITS: i64 = 128 + 64;
    let entities = RecordSlice::<MapEntity>::new(entity_bytes)
        .ok_or_else(|| format!("Rust entity table is malformed for {map}"))?;
    let models = AliasModelTable::new(model_bytes)
        .map_err(|error| format!("Rust alias-model table is malformed for {map}: {error}"))?;

    let mut monsters = 0usize;
    let mut render_slots = PROJECTILE_RENDER_SLOTS;
    let mut origins: Vec<(i32, i32, i32)> = Vec::new();
    for entity in entities.iter().skip(2) {
        if entity.spawn_flags & NOT_EASY != 0 {
            continue;
        }
        // Every authored entity the guest renders occupies one slot.
        if entity.model != 0 || entity.class_name != 0 {
            // Counting exactly is the loader's job; the bound below is what
            // matters, so count every non-worldspawn record conservatively.
            render_slots += 1;
        }
        let Some(kind) = MonsterKind::from_class_name(entity.class_name) else {
            continue;
        };
        monsters += 1;
        let highest = highest_frame(kind);
        let model = models.get(kind.model_id()).ok_or_else(|| {
            format!(
                "{map} authors class {:#04x} but its alias model {:#04x} is not cooked",
                entity.class_name,
                kind.model_id()
            )
        })?;
        let frames = model.header().frame_count;
        if highest >= frames {
            return Err(format!(
                "{map}: {kind:?} authors frame {highest} but its cooked model has {frames}"
            )
            .into());
        }
        origins.push((
            entity.origin.x >> 12,
            entity.origin.y >> 12,
            entity.origin.z >> 12,
        ));
    }

    if render_slots > MAX_RENDER_ENTITIES {
        return Err(format!(
            "{map} needs {render_slots} render slots but the guest pool holds {MAX_RENDER_ENTITIES}"
        )
        .into());
    }

    // Densest simultaneous body cluster: how many other monsters could ever be
    // offered to one monster's bounded candidate set.
    let mut densest = 0usize;
    for (index, origin) in origins.iter().enumerate() {
        let mut neighbours = 0usize;
        for (other_index, other) in origins.iter().enumerate() {
            if index == other_index {
                continue;
            }
            let dx = i64::from(origin.0 - other.0);
            let dy = i64::from(origin.1 - other.1);
            let dz = i64::from(origin.2 - other.2);
            if dx * dx + dy * dy + dz * dz <= CLUSTER_UNITS * CLUSTER_UNITS {
                neighbours += 1;
            }
        }
        // The player is always offered first, so it costs one slot too.
        densest = densest.max(neighbours + 1);
    }
    if densest > MAX_BODY_CANDIDATES {
        return Err(format!(
            "{map} has a {densest}-body cluster but one trace admits {MAX_BODY_CANDIDATES}"
        )
        .into());
    }

    if env::var_os("QUAKE_PSX_MONSTER_CENSUS").is_some() {
        let mut per_kind: Vec<(String, usize, u16, (i32, i32, i32))> = Vec::new();
        // The cooker writes the player start as record 1, which is what
        // `Player::from_start` reads.
        let start = entities
            .get(1)
            .map(|entity| {
                (
                    entity.origin.x >> 12,
                    entity.origin.y >> 12,
                    entity.origin.z >> 12,
                )
            })
            .unwrap_or_default();
        for (index, entity) in entities.iter().enumerate() {
            if entity.spawn_flags & NOT_EASY != 0 {
                continue;
            }
            let Some(kind) = MonsterKind::from_class_name(entity.class_name) else {
                continue;
            };
            let origin = (
                entity.origin.x >> 12,
                entity.origin.y >> 12,
                entity.origin.z >> 12,
            );
            let name = format!("{kind:?}");
            let distance = {
                let dx = i64::from(origin.0 - start.0);
                let dy = i64::from(origin.1 - start.1);
                let dz = i64::from(origin.2 - start.2);
                ((dx * dx + dy * dy + dz * dz) as f64).sqrt() as usize
            };
            match per_kind
                .iter_mut()
                .find(|(existing, _, _, _)| *existing == name)
            {
                Some(slot) => {
                    slot.1 += 1;
                    if distance < slot.2 as usize {
                        slot.2 = distance.min(u16::MAX as usize) as u16;
                        slot.3 = origin;
                    }
                }
                None => per_kind.push((name, 1, distance.min(u16::MAX as usize) as u16, origin)),
            }
            let _ = index;
        }
        per_kind.sort();
        let summary: Vec<String> = per_kind
            .iter()
            .map(|(name, count, nearest, origin)| {
                format!("{name} x{count} nearest={nearest} at {origin:?}")
            })
            .collect();
        println!("census {map}: start={start:?} {}", summary.join("; "));
    }
    Ok(MonsterPopulation {
        monsters,
        render_slots,
        densest_cluster: densest,
    })
}

/// Guest pool capacities, mirrored from `game/src/entity.rs` and
/// `crates/quake-core`. Every one is checked against the real worst case the
/// cooked shareware maps author, so a pool can never silently drop authored
/// content the way the ambient voice pool is checked.
const GUEST_MAX_RENDER_ENTITIES: usize = 384;
const GUEST_MAX_MOVERS: usize = 64;
const GUEST_MAX_TRIGGERS: usize = 32;
const GUEST_MAX_TELEPORTS: usize = 32;
const GUEST_MAX_TRAINS: usize = 8;
const GUEST_MAX_FIREBALL_EMITTERS: usize = 16;
const GUEST_MAX_FIREBALLS: usize = 32;
const GUEST_MAX_CHANGE_LEVELS: usize = 4;
const GUEST_MAX_TARGET_ACTIONS: usize = 128;
const GUEST_MAX_PLAYER_ACTIVATIONS: usize = 8;
const GUEST_NAIL_POOL_CAPACITY: usize = 60;
const GUEST_MAX_ROCKETS: usize = 8;
const GUEST_MAX_GRENADES: usize = 8;
/// Every emitter re-arms after at least three seconds and each ball lives five,
/// so at most two of a spout's balls are ever in flight.
const FIREBALLS_PER_EMITTER: usize = 2;
/// `Mod_LoadBrushModel` grows a submodel's runtime bounds by one unit per axis
/// before `setmodel` hands them to `EntitiesTouching`.
const SUBMODEL_BOUNDS_MARGIN: f32 = 1.0;

/// Worst per-map population of every fixed guest pool this port added or grew.
#[derive(Clone, Copy, Debug, Default)]
struct PoolCensus {
    triggers: usize,
    movers: usize,
    trains: usize,
    teleports: usize,
    emitters: usize,
    render: usize,
    fan_out: usize,
    door_group: usize,
    monsters: usize,
}

impl PoolCensus {
    fn merge(&mut self, other: Self) {
        self.triggers = self.triggers.max(other.triggers);
        self.movers = self.movers.max(other.movers);
        self.trains = self.trains.max(other.trains);
        self.teleports = self.teleports.max(other.teleports);
        self.emitters = self.emitters.max(other.emitters);
        self.render = self.render.max(other.render);
        self.fan_out = self.fan_out.max(other.fan_out);
        self.door_group = self.door_group.max(other.door_group);
        self.monsters = self.monsters.max(other.monsters);
    }
}

/// Per-map worst case for every fixed guest pool this port added or grew.
fn validate_runtime_pools(
    map: &str,
    bsp: &Bsp<'_>,
    runtime: &RecordSlice<'_, MapEntity>,
) -> Result<PoolCensus> {
    // Skill zero is what ships; a NOT_EASY entity never spawns.
    let spawned = |entity: &MapEntity| entity.spawn_flags & 0x0100 == 0;
    let count = |predicate: &dyn Fn(&MapEntity) -> bool| {
        runtime
            .iter()
            .skip(2)
            .filter(|entity| spawned(entity) && predicate(entity))
            .count()
    };
    let brush = |entity: &MapEntity| entity.model < 0;

    let triggers = count(&|entity| {
        matches!(entity.class_name, 0x4b | 0x4c | 0x50 | 0x4d | 0x51) && brush(entity)
    });
    let movers =
        count(&|entity| matches!(entity.class_name, 0x0b | 0x0c | 0x0d | 0x10) && brush(entity));
    let trains = count(&|entity| entity.class_name == 0x11 && brush(entity));
    let teleports = count(&|entity| entity.class_name == 0x52 && brush(entity));
    let emitters = count(&|entity| entity.class_name == 0x34);
    let gates = count(&|entity| matches!(entity.class_name, 0x0a | 0x0e) && brush(entity));
    // Every brush model this port renders, plus the alias entities, plus the
    // projectile slots that are installed unconditionally, plus the lava-ball
    // slots the spouted maps add.
    let brush_models = count(&|entity| {
        matches!(
            entity.class_name,
            0x0a | 0x0b | 0x0c | 0x0d | 0x0e | 0x0f | 0x10 | 0x11 | 0x12 | 0x35
        ) && brush(entity)
    });
    let alias = count(&|entity| !brush(entity));
    let projectile_slots = GUEST_MAX_ROCKETS
        + GUEST_NAIL_POOL_CAPACITY
        + GUEST_MAX_GRENADES
        + if emitters == 0 {
            0
        } else {
            GUEST_MAX_FIREBALLS
        };
    let render = brush_models + alias + projectile_slots;

    let fan_out = runtime
        .iter()
        .skip(2)
        .filter(|entity| spawned(entity) && entity.target_name != 0)
        .map(|entity| {
            runtime
                .iter()
                .filter(|other| spawned(other) && other.target_name == entity.target_name)
                .count()
        })
        .max()
        .unwrap_or(0);
    let door_group = largest_linked_door_group(bsp, runtime)?;
    // `total_monsters` for the intermission panel. The counter is a u16 pair,
    // so the assertion is that the authored Easy population of every map fits
    // the panel's own numeric field rather than a pool slot.
    let monsters = usize::from(quake_core::level::count_authored(runtime, 0));
    if monsters > usize::from(u16::MAX) {
        return Err(
            format!("{map} authors {monsters} killable monsters, the counter is u16").into(),
        );
    }
    // The panel needs an authored camera. `SelectIntermissionPoint` has a
    // fallback in this port, but a map that silently lost its
    // `info_intermission` would take it every time, so the census refuses.
    if !runtime.iter().any(|entity| entity.class_name == 0x13) {
        return Err(
            format!("{map} authors no info_intermission for the end-of-level panel").into(),
        );
    }

    for (label, worst, capacity) in [
        ("touch triggers", triggers, GUEST_MAX_TRIGGERS),
        ("brush movers", movers, GUEST_MAX_MOVERS),
        ("trains", trains, GUEST_MAX_TRAINS),
        ("teleport volumes", teleports, GUEST_MAX_TELEPORTS),
        ("fireball emitters", emitters, GUEST_MAX_FIREBALL_EMITTERS),
        (
            "fireballs in flight",
            emitters * FIREBALLS_PER_EMITTER,
            GUEST_MAX_FIREBALLS,
        ),
        ("render entities", render, GUEST_MAX_RENDER_ENTITIES),
        ("one-target fan-out", fan_out, GUEST_MAX_TARGET_ACTIONS),
        (
            "linked door group",
            door_group,
            GUEST_MAX_PLAYER_ACTIVATIONS,
        ),
    ] {
        if worst > capacity {
            return Err(
                format!("{map} authors {worst} {label}, guest capacity is {capacity}").into(),
            );
        }
    }
    // Episode gates are counted in the render budget above but only spawn with
    // the matching rune; the assertion deliberately takes the worst case where
    // every gate is present.
    let _ = gates;
    Ok(PoolCensus {
        triggers,
        movers,
        trains,
        teleports,
        emitters,
        render,
        fan_out,
        door_group,
        monsters,
    })
}

/// Every runtime level title must be the map's own authored worldspawn
/// `message`. The cook drops it, so the guest carries a table; this keeps that
/// table honest against the shipping PAK instead of trusting it.
fn validate_level_title(map: &str, bsp: &Bsp<'_>) -> Result<()> {
    let index = quake_core::level::LEVEL_NAMES
        .iter()
        .position(|name| *name == map)
        .ok_or_else(|| format!("{map} is not a shareware Episode 1 level"))?;
    let text = std::str::from_utf8(bsp.lump(BspLump::Entities))
        .map_err(|_| format!("{map} entity lump is not UTF-8"))?;
    let worldspawn = text.split('}').next().unwrap_or_default();
    let authored = worldspawn
        .split("\"message\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .ok_or_else(|| format!("{map} worldspawn authors no message"))?;
    let expected = quake_core::level::LEVEL_TITLES[index];
    if authored != expected {
        return Err(format!(
            "{map} worldspawn message is {authored:?}, the runtime table says {expected:?}"
        )
        .into());
    }
    Ok(())
}

/// Largest `LinkDoors` chain the map authors, over the engine's grown submodel
/// bounds. Every shareware key lock is two half doors, so this must stay at or
/// under the guest's per-frame player-activation list.
fn largest_linked_door_group(bsp: &Bsp<'_>, runtime: &RecordSlice<'_, MapEntity>) -> Result<usize> {
    const MODEL_RECORD_BYTES: usize = 64;
    let models = bsp.lump(BspLump::Models);
    let bounds = |model: i16| -> Option<([f32; 3], [f32; 3])> {
        let index = model.checked_neg()? as usize;
        let record = models.get(index * MODEL_RECORD_BYTES..(index + 1) * MODEL_RECORD_BYTES)?;
        let mut mins = [0.0f32; 3];
        let mut maxs = [0.0f32; 3];
        for axis in 0..3 {
            mins[axis] = f32::from_le_bytes(record[axis * 4..axis * 4 + 4].try_into().unwrap())
                - SUBMODEL_BOUNDS_MARGIN;
            maxs[axis] =
                f32::from_le_bytes(record[12 + axis * 4..16 + axis * 4].try_into().unwrap())
                    + SUBMODEL_BOUNDS_MARGIN;
        }
        Some((mins, maxs))
    };
    let doors = runtime
        .iter()
        .skip(2)
        .filter(|entity| {
            entity.class_name == 0x0c && entity.model < 0 && entity.spawn_flags & 0x0104 == 0
        })
        .filter_map(|entity| bounds(entity.model))
        .collect::<Vec<_>>();
    let mut group = (0..doors.len()).collect::<Vec<_>>();
    for left in 0..doors.len() {
        for right in (left + 1)..doors.len() {
            let touching = (0..3).all(|axis| {
                doors[left].0[axis] <= doors[right].1[axis]
                    && doors[left].1[axis] >= doors[right].0[axis]
            });
            if !touching || group[left] == group[right] {
                continue;
            }
            let replaced = group[right];
            let merged = group[left];
            for entry in &mut group {
                if *entry == replaced {
                    *entry = merged;
                }
            }
        }
    }
    Ok((0..doors.len())
        .map(|root| group.iter().filter(|entry| **entry == root).count())
        .max()
        .unwrap_or(0))
}

fn validate_ambient_sounds(map: &str, entity_bytes: &[u8], sound: &[u8]) -> Result<usize> {
    const MAX_AMBIENT_SOURCES: usize = 32;
    const MAX_AMBIENT_VOICES: usize = 11;
    let entities = RecordSlice::<MapEntity>::new(entity_bytes)
        .ok_or_else(|| format!("Rust entity table is malformed for {map}"))?;
    let (header, effects, _, payload) = decode_sound_bank(sound)
        .map_err(|error| format!("Rust sound suffix is malformed for {map}: {error:?}"))?;
    if header.kind != SoundBankKind::Local {
        return Err(format!("Rust sound suffix has the wrong kind for {map}").into());
    }

    let mut count = 0usize;
    let mut voice_sounds = [0i16; MAX_AMBIENT_VOICES];
    let mut voice_count = 0usize;
    for entity in entities.iter().skip(2) {
        let Some(sound_id) = (match entity.class_name {
            0x03 => Some(0x02),
            0x04 => Some(0x03),
            0x05 => Some(0x04),
            0x06 => Some(0x08),
            0x07 | 0x08 => Some(0x09),
            _ => None,
        }) else {
            continue;
        };
        count += 1;
        if count > MAX_AMBIENT_SOURCES {
            return Err(
                format!("{map} has more than {MAX_AMBIENT_SOURCES} ambient sources").into(),
            );
        }
        if !voice_sounds[..voice_count].contains(&sound_id) {
            if voice_count == voice_sounds.len() {
                return Err(
                    format!("{map} has more than {MAX_AMBIENT_VOICES} ambient voices").into(),
                );
            }
            voice_sounds[voice_count] = sound_id;
            voice_count += 1;
        }
        let effect_index = effects
            .iter()
            .position(|effect| effect.id == sound_id)
            .ok_or_else(|| format!("{map} ambient sound {sound_id:#04x} is not in its bank"))?;
        let effect = effects
            .get(effect_index)
            .ok_or("ambient sound record missing")?;
        let start = effect
            .spu_address
            .checked_sub(header.payload_base)
            .ok_or("ambient sound starts below the SPU bank")? as usize;
        let end = if effect_index + 1 < effects.len() {
            let next = effects
                .get(effect_index + 1)
                .ok_or("next ambient sound record missing")?;
            next.spu_address
                .checked_sub(header.payload_base)
                .ok_or("ambient sound starts below the SPU bank")? as usize
        } else {
            payload.len()
        };
        let encoded_end = end
            .checked_sub(SFX_PARKING_TAIL.len())
            .filter(|encoded_end| *encoded_end >= start)
            .ok_or("ambient sound is smaller than its parking tail")?;
        let encoded = payload
            .get(start..encoded_end)
            .ok_or("ambient sound payload is truncated")?;
        let loop_start = encoded.chunks_exact(16).any(|block| block[1] & 0x04 != 0);
        let repeat = encoded.chunks_exact(16).any(|block| block[1] & 0x02 != 0);
        if !loop_start || !repeat {
            return Err(
                format!("{map} ambient sound {sound_id:#04x} has no cooked hardware loop").into(),
            );
        }
    }
    if entities
        .iter()
        .skip(2)
        .any(|entity| entity.class_name == 0x52)
        && voice_count == MAX_AMBIENT_VOICES
    {
        return Err(format!("{map} has no static voice left for teleporter hum").into());
    }
    Ok(count)
}

fn validate_model_parity(map: &str, legacy: &[u8], rust: &[u8]) -> Result<()> {
    const HEADER_BYTES: usize = 68;
    const TRIANGLE_BYTES: usize = 12;
    if legacy.len() != rust.len() || legacy.len() < 4 {
        return Err(format!(
            "Rust model size parity failed for {map}: legacy={} Rust={}",
            legacy.len(),
            rust.len()
        )
        .into());
    }
    AliasModelTable::new(rust)
        .map_err(|error| format!("Rust alias-model validation failed for {map}: {error}"))?;
    let legacy_count = u32::from_le_bytes(legacy[..4].try_into().unwrap()) as usize;
    let rust_count = u32::from_le_bytes(rust[..4].try_into().unwrap()) as usize;
    let header_end = 4usize
        .checked_add(
            legacy_count
                .checked_mul(HEADER_BYTES)
                .ok_or("model header overflow")?,
        )
        .ok_or("model header overflow")?;
    if legacy_count != rust_count
        || header_end > legacy.len()
        || legacy[..header_end] != rust[..header_end]
    {
        return Err(format!("Rust model header parity failed for {map}").into());
    }
    let model_data_len = legacy.len() - header_end;
    for model in 0..legacy_count {
        let header = 4 + model * HEADER_BYTES;
        let triangle_count =
            u16::from_le_bytes(legacy[header + 8..header + 10].try_into().unwrap()) as usize;
        let skin_count =
            u16::from_le_bytes(legacy[header + 10..header + 12].try_into().unwrap()) as usize;
        let triangle_offset =
            u32::from_le_bytes(legacy[header + 60..header + 64].try_into().unwrap()) as usize;
        let frame_offset =
            u32::from_le_bytes(legacy[header + 64..header + 68].try_into().unwrap()) as usize;
        let triangle_len = triangle_count
            .checked_mul(skin_count)
            .and_then(|count| count.checked_mul(TRIANGLE_BYTES))
            .ok_or("model triangle size overflow")?;
        let triangle_end = triangle_offset
            .checked_add(triangle_len)
            .ok_or("model triangle range overflow")?;
        let frame_end = if model + 1 < legacy_count {
            let next = header + HEADER_BYTES;
            u32::from_le_bytes(legacy[next + 60..next + 64].try_into().unwrap()) as usize
        } else {
            model_data_len
        };
        if triangle_end != frame_offset || frame_end < frame_offset || frame_end > model_data_len {
            return Err(format!("malformed model ranges in {map} model {model}").into());
        }
        let legacy_triangles = &legacy[header_end + triangle_offset..header_end + triangle_end];
        let rust_triangles = &rust[header_end + triangle_offset..header_end + triangle_end];
        if legacy_triangles != rust_triangles {
            let mut legacy_records = legacy_triangles
                .chunks_exact(TRIANGLE_BYTES)
                .collect::<Vec<_>>();
            let mut rust_records = rust_triangles
                .chunks_exact(TRIANGLE_BYTES)
                .collect::<Vec<_>>();
            legacy_records.sort_unstable();
            rust_records.sort_unstable();
            if legacy_records != rust_records {
                let mismatch = legacy_records
                    .iter()
                    .zip(&rust_records)
                    .position(|(left, right)| left != right)
                    .unwrap_or(legacy_records.len().min(rust_records.len()));
                return Err(format!(
                    "Rust triangle parity failed for {map} model {model} at sorted record {mismatch}: legacy={:02x?}, Rust={:02x?}",
                    legacy_records.get(mismatch), rust_records.get(mismatch)
                )
                .into());
            }
        }
        if legacy[header_end + frame_offset..header_end + frame_end]
            != rust[header_end + frame_offset..header_end + frame_end]
        {
            return Err(format!("Rust model frame parity failed for {map} model {model}").into());
        }
    }
    Ok(())
}

fn geometry_lumps(geometry: &GeometryLumps) -> [(LumpKind, &[u8]); 10] {
    [
        (LumpKind::Planes, &geometry.planes),
        (LumpKind::TextureInfo, &geometry.texture_info),
        (LumpKind::Vertices, &geometry.vertices),
        (LumpKind::Faces, &geometry.faces),
        (LumpKind::MarkSurfaces, &geometry.mark_surfaces),
        (LumpKind::Visibility, &geometry.visibility),
        (LumpKind::Leaves, &geometry.leaves),
        (LumpKind::Nodes, &geometry.nodes),
        (LumpKind::ClipNodes, &geometry.clip_nodes),
        (LumpKind::Models, &geometry.models),
    ]
}

fn parse_cli() -> Result<Cli> {
    let mut args = env::args_os().skip(1).peekable();
    let mut action = Action::Build;
    let mut action_seen = false;
    let mut quake_dir = None;
    let mut psoxide = None;
    let mut allow_psoxide_drift = false;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "build"
            | "assets"
            | "compile"
            | "disc"
            | "check"
            | "ship-boot"
            | "map-regress"
            | "start-route-regress"
            | "visual-parity-regress"
            | "e1m1-chain-regress"
            | "e1m1-chain-bench"
            | "e1m1-selection-cache-bench"
            | "e1m1-topology-cache-bench"
            | "e1m1-indexed-projection-bench"
            | "e1m1-aabb-offsets-bench"
            | "e1m1-relaxed-quad-pairing-bench"
            | "e1m1-shared-subdivision-edges-bench"
            | "e1m1-level0-fast-path-bench"
            | "e1m1-speculative-level0-bench"
            | "e1m1-depth-only-subdivision-bench"
            | "e1m1-gte-otz-bench"
            | "e1m1-compact-subdivision-emitters-bench"
            | "e1m1-compact-subdivision-kernels-bench"
            | "e1m1-compact-level2-kernel-bench"
            | "e1m1-compact-world-level2-kernel-bench"
            | "e1m1-gpu-lattice-clip-bench"
            | "e1m1-gpu-polygon-clip-bench"
            | "e1m1-gpu-polygon-depth-only-bench"
            | "e1m1-gpu-polygon-compact-ot-bench"
            | "e1m1-gpu-polygon-fused-projection-bench"
            | "e1m1-gpu-polygon-plane-index-bench"
            | "e1m1-gpu-polygon-window-runs-bench"
            | "e1m1-gpu-polygon-window-insert-bench"
            | "e1m1-gpu-polygon-window-range-bench"
            | "e1m1-gpu-polygon-cell-stream-bench"
            | "e1m1-gpu-polygon-cell-policy-bench"
            | "gpu-polygon-cell-policy-disc"
            | "e1m1-gpu-polygon-quake-kernel-bench"
            | "e1m1-gpu-polygon-level0-run-bench"
            | "e1m1-gpu-polygon-cold-adaptive-bench"
            | "e1m1-gpu-polygon-cold-level2-bench"
            | "e1m1-gpu-polygon-resident-stream-bench"
            | "e1m1-gpu-polygon-resident-level2-stream-bench"
            | "e1m1-gpu-polygon-resident-level2-scatter-bench"
            | "e1m1-gpu-polygon-resident-level2-cold-cache-bench"
            | "e1m1-gpu-polygon-resident-base-cache-bench"
            | "e1m1-gpu-polygon-resident-base-cache-fast-bench"
            | "e1m1-gpu-surface-clip-bench"
            | "e1m1-static-world-reuse-bench"
            | "e1m1-hoisted-indexed-world-bench"
            | "e1m1-fixed-fan-quads-bench"
            | "e1m1-fixed-fan-guarded-bench"
            | "e1m1-fixed-fan-level2-bench"
            | "e1m1-subdivision-cache-bench"
            | "e1m1-subdivision-cache-level2-bench"
            | "e1m1-subdivision-cache-level2-small-bench"
            | "e1m1-subdivision-cache-level2-layout-control-bench"
            | "e1m1-block-frustum-bench"
            | "e1m1-hierarchical-block-frustum-bench"
            | "e1m1-block-frustum-32-bench"
            | "e1m1-plane-run-cache-bench"
            | "block-frustum-regress"
            | "gpu-polygon-clip-regress"
            | "selection-cache-regress"
            | "selection-cache-ship-boot"
            | "e1m1-renderer-census"
            | "e1m1-gpu-census"
            | "e1m1-gpu-polygon-census"
            | "e1m2-e1m3-route-regress"
            | "survival-regress"
            | "systems-regress"
            | "combat-regress"
            | "monster-regress"
            | "monsterjump-regress"
            | "bestiary-regress"
            | "episode1-regress"
            | "arsenal-regress"
            | "audio-regress"
            | "ambient-regress"
            | "regress"
            | "hardware" => {
                if action_seen {
                    return Err("only one action may be selected".into());
                }
                action_seen = true;
                action = match text.as_ref() {
                    "build" => Action::Build,
                    "assets" => Action::Assets,
                    "compile" => Action::Compile,
                    "disc" => Action::Disc,
                    "check" => Action::Check,
                    "ship-boot" => Action::ShipBoot,
                    "map-regress" => Action::MapRegress,
                    "start-route-regress" => Action::StartRouteRegress,
                    "visual-parity-regress" => Action::VisualParityRegress,
                    "e1m1-chain-regress" => Action::E1m1ChainRegress,
                    "e1m1-chain-bench" => Action::E1m1ChainBench,
                    "e1m1-selection-cache-bench" => Action::E1m1SelectionCacheBench,
                    "e1m1-topology-cache-bench" => Action::E1m1TopologyCacheBench,
                    "e1m1-indexed-projection-bench" => Action::E1m1IndexedProjectionBench,
                    "e1m1-aabb-offsets-bench" => Action::E1m1AabbOffsetsBench,
                    "e1m1-relaxed-quad-pairing-bench" => Action::E1m1RelaxedQuadPairingBench,
                    "e1m1-shared-subdivision-edges-bench" => {
                        Action::E1m1SharedSubdivisionEdgesBench
                    }
                    "e1m1-level0-fast-path-bench" => Action::E1m1Level0FastPathBench,
                    "e1m1-speculative-level0-bench" => Action::E1m1SpeculativeLevel0Bench,
                    "e1m1-depth-only-subdivision-bench" => Action::E1m1DepthOnlySubdivisionBench,
                    "e1m1-gte-otz-bench" => Action::E1m1GteOtzBench,
                    "e1m1-compact-subdivision-emitters-bench" => {
                        Action::E1m1CompactSubdivisionEmittersBench
                    }
                    "e1m1-compact-subdivision-kernels-bench" => {
                        Action::E1m1CompactSubdivisionKernelsBench
                    }
                    "e1m1-compact-level2-kernel-bench" => Action::E1m1CompactLevel2KernelBench,
                    "e1m1-compact-world-level2-kernel-bench" => {
                        Action::E1m1CompactWorldLevel2KernelBench
                    }
                    "e1m1-gpu-lattice-clip-bench" => Action::E1m1GpuLatticeClipBench,
                    "e1m1-gpu-polygon-clip-bench" => Action::E1m1GpuPolygonClipBench,
                    "e1m1-gpu-polygon-depth-only-bench" => Action::E1m1GpuPolygonDepthOnlyBench,
                    "e1m1-gpu-polygon-compact-ot-bench" => Action::E1m1GpuPolygonCompactOtBench,
                    "e1m1-gpu-polygon-fused-projection-bench" => {
                        Action::E1m1GpuPolygonFusedProjectionBench
                    }
                    "e1m1-gpu-polygon-plane-index-bench" => Action::E1m1GpuPolygonPlaneIndexBench,
                    "e1m1-gpu-polygon-window-runs-bench" => Action::E1m1GpuPolygonWindowRunsBench,
                    "e1m1-gpu-polygon-window-insert-bench" => {
                        Action::E1m1GpuPolygonWindowInsertBench
                    }
                    "e1m1-gpu-polygon-window-range-bench" => Action::E1m1GpuPolygonWindowRangeBench,
                    "e1m1-gpu-polygon-cell-stream-bench" => Action::E1m1GpuPolygonCellStreamBench,
                    "e1m1-gpu-polygon-cell-policy-bench" => Action::E1m1GpuPolygonCellPolicyBench,
                    "gpu-polygon-cell-policy-disc" => Action::GpuPolygonCellPolicyDisc,
                    "e1m1-gpu-polygon-quake-kernel-bench" => Action::E1m1GpuPolygonQuakeKernelBench,
                    "e1m1-gpu-polygon-level0-run-bench" => Action::E1m1GpuPolygonLevel0RunBench,
                    "e1m1-gpu-polygon-cold-adaptive-bench" => {
                        Action::E1m1GpuPolygonColdAdaptiveBench
                    }
                    "e1m1-gpu-polygon-cold-level2-bench" => Action::E1m1GpuPolygonColdLevel2Bench,
                    "e1m1-gpu-polygon-resident-stream-bench" => {
                        Action::E1m1GpuPolygonResidentStreamBench
                    }
                    "e1m1-gpu-polygon-resident-level2-stream-bench" => {
                        Action::E1m1GpuPolygonResidentLevel2StreamBench
                    }
                    "e1m1-gpu-polygon-resident-level2-scatter-bench" => {
                        Action::E1m1GpuPolygonResidentLevel2ScatterBench
                    }
                    "e1m1-gpu-polygon-resident-level2-cold-cache-bench" => {
                        Action::E1m1GpuPolygonResidentLevel2ColdCacheBench
                    }
                    "e1m1-gpu-polygon-resident-base-cache-bench" => {
                        Action::E1m1GpuPolygonResidentBaseCacheBench
                    }
                    "e1m1-gpu-polygon-resident-base-cache-fast-bench" => {
                        Action::E1m1GpuPolygonResidentBaseCacheFastBench
                    }
                    "e1m1-gpu-surface-clip-bench" => Action::E1m1GpuSurfaceClipBench,
                    "e1m1-static-world-reuse-bench" => Action::E1m1StaticWorldReuseBench,
                    "e1m1-hoisted-indexed-world-bench" => Action::E1m1HoistedIndexedWorldBench,
                    "e1m1-fixed-fan-quads-bench" => Action::E1m1FixedFanQuadsBench,
                    "e1m1-fixed-fan-guarded-bench" => Action::E1m1FixedFanGuardedBench,
                    "e1m1-fixed-fan-level2-bench" => Action::E1m1FixedFanLevel2Bench,
                    "e1m1-subdivision-cache-bench" => Action::E1m1SubdivisionCacheBench,
                    "e1m1-subdivision-cache-level2-bench" => {
                        Action::E1m1SubdivisionCacheLevel2Bench
                    }
                    "e1m1-subdivision-cache-level2-small-bench" => {
                        Action::E1m1SubdivisionCacheLevel2SmallBench
                    }
                    "e1m1-subdivision-cache-level2-layout-control-bench" => {
                        Action::E1m1SubdivisionCacheLevel2LayoutControlBench
                    }
                    "e1m1-block-frustum-bench" => Action::E1m1BlockFrustumBench,
                    "e1m1-hierarchical-block-frustum-bench" => {
                        Action::E1m1HierarchicalBlockFrustumBench
                    }
                    "e1m1-block-frustum-32-bench" => Action::E1m1BlockFrustum32Bench,
                    "e1m1-plane-run-cache-bench" => Action::E1m1PlaneRunCacheBench,
                    "block-frustum-regress" => Action::BlockFrustumRegress,
                    "gpu-polygon-clip-regress" => Action::GpuPolygonClipRegress,
                    "selection-cache-regress" => Action::SelectionCacheRegress,
                    "selection-cache-ship-boot" => Action::SelectionCacheShipBoot,
                    "e1m1-renderer-census" => Action::E1m1RendererCensus,
                    "e1m1-gpu-census" => Action::E1m1GpuCensus,
                    "e1m1-gpu-polygon-census" => Action::E1m1GpuPolygonCensus,
                    "e1m2-e1m3-route-regress" => Action::E1m2E1m3RouteRegress,
                    "survival-regress" => Action::SurvivalRegress,
                    "systems-regress" => Action::SystemsRegress,
                    "combat-regress" => Action::CombatRegress,
                    "monster-regress" => Action::MonsterRegress,
                    "monsterjump-regress" => Action::MonsterJumpRegress,
                    "bestiary-regress" => Action::BestiaryRegress,
                    "episode1-regress" => Action::Episode1Regress,
                    "arsenal-regress" => Action::ArsenalRegress,
                    "audio-regress" => Action::AudioRegress,
                    "ambient-regress" => Action::AmbientRegress,
                    "regress" => Action::Regress,
                    "hardware" => Action::Hardware,
                    _ => unreachable!(),
                };
            }
            "--quake-dir" => quake_dir = Some(next_path(&mut args, "--quake-dir")?),
            "--psoxide" => psoxide = Some(next_path(&mut args, "--psoxide")?),
            "--allow-psoxide-drift" => allow_psoxide_drift = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {text}").into()),
        }
    }
    Ok(Cli {
        action,
        quake_dir,
        psoxide,
        allow_psoxide_drift,
    })
}

fn next_path<I: Iterator<Item = OsString>>(args: &mut I, flag: &str) -> Result<PathBuf> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} needs a path").into())
}

fn print_help() {
    println!(
        "quake-psx-build\n\n\
         Usage: cargo run --release -- [ACTION] [OPTIONS]\n\n\
         Actions:\n  \
           build     Download/cook shareware, build the PSoXide disc, package dist (default)\n  \
           assets    Force a complete Episode 1 recook\n  \
           compile   Rebuild only the PSoXide PS1 executable\n  \
           disc      Build and package the disc from current assets\n  \
           check     Validate source data and PSoXide tool discovery\n  \
           ship-boot  Boot the real shipping disc headlessly and report heap headroom\n  \
           map-regress  Exercise every Episode 1 map load and change-level edge\n  \
           start-route-regress  Walk Start's real Easy slipgate into E1M1 headlessly\n  \
           visual-parity-regress  Capture the pinned E1M1 owner camera and audit GP0(E2) scope\n  \
           e1m1-chain-regress  Walk E1M1's authored three-button door chain headlessly\n  \
           e1m1-chain-bench  The same route at a fixed sim step, for ranking performance work\n  \
           e1m1-selection-cache-bench  A/B exact-key selected-face reuse\n  \
           e1m1-topology-cache-bench  A/B resident adaptive packet topology\n  \
           e1m1-indexed-projection-bench  A/B dense shared-position world projection\n  \
           e1m1-aabb-offsets-bench  A/B hoisted exact AABB support selectors\n  \
           e1m1-relaxed-quad-pairing-bench  A/B native GT4 pairing across adjacent OT slots\n  \
           e1m1-shared-subdivision-edges-bench  A/B omit duplicate same-level radial underdraw\n  \
           e1m1-level0-fast-path-bench  A/B isolate common level-zero fans from lattice fallback\n  \
           e1m1-speculative-level0-bench  A/B one-pass level-zero emit with exact adaptive rollback\n  \
           e1m1-depth-only-subdivision-bench  A/B compile Quake's exact OTZ-only selector\n  \
           e1m1-gte-otz-bench  A/B replace cached-depth MIPS OTZ math with GTE AVSZ3\n  \
           e1m1-compact-subdivision-emitters-bench  A/B share exact adaptive GT3/GT4 kernels\n  \
           e1m1-compact-subdivision-kernels-bench  A/B share complete exact L1/L2 lattices\n  \
           e1m1-compact-level2-kernel-bench  A/B share only the rare exact L2 lattice\n  \
           e1m1-compact-world-level2-kernel-bench  A/B share ordinary-world L2 only\n  \
           e1m1-gpu-lattice-clip-bench  A/B PS1 draw-area clipping versus per-packet CPU rejects\n  \
           e1m1-gpu-polygon-clip-bench  A/B draw-area clipping for every admitted world GT3/GT4\n  \
           e1m1-gpu-polygon-depth-only-bench  Combine GPU clip ownership with compact subdivision depth scratch\n  \
           e1m1-gpu-polygon-compact-ot-bench  Quantise final world DMA buckets into a 256-slot OT\n  \
           e1m1-gpu-polygon-fused-projection-bench  Fuse indexed materialisation with GTE projection\n  \
           e1m1-gpu-polygon-plane-index-bench  Memoize BSP plane sides by direct cooked index\n  \
           e1m1-gpu-polygon-window-runs-bench  Coalesce final GPU-order liquid window state runs\n  \
           e1m1-gpu-polygon-window-insert-bench  Coalesce liquid state inside the existing OT linker\n  \
           e1m1-gpu-polygon-cell-stream-bench  Compact leaf-local draw records and prune invariant backs\n  \
           gpu-polygon-cell-policy-disc  Build and boot-test the playable 23.432 renderer feature stack\n  \
           e1m1-gpu-surface-clip-bench  A/B remove the projected scan after PVS/frustum admission\n  \
           e1m1-static-world-reuse-bench  A/B reuse exact same-camera ordinary world packets\n  \
           e1m1-hoisted-indexed-world-bench  A/B decode PSB5 indexed view once per world frame\n  \
           e1m1-fixed-fan-quads-bench  Measure Quake II-style fixed GT4 world ceiling\n  \
           e1m1-fixed-fan-guarded-bench  Fixed fans with screen-spanning adaptive fallback\n  \
           e1m1-fixed-fan-level2-bench  Fixed packets retaining closest-band subdivision\n  \
           e1m1-subdivision-cache-bench  A/B resident exact adaptive subdivision packets\n  \
           e1m1-subdivision-cache-level2-bench  A/B persist only large level-two roots\n  \
           e1m1-subdivision-cache-level2-small-bench  Control with only 26 resident level-two roots\n  \
           e1m1-gpu-polygon-resident-stream-bench  Exact GPU-clip plus fixed resident-root stream linking\n  \
           e1m1-gpu-polygon-resident-level2-stream-bench  Exact bounded L2 resident-root stream linking\n  \
           e1m1-gpu-polygon-resident-level2-scatter-bench  Position-only fixed L2 resident XY scatter\n  \
           e1m1-gpu-polygon-resident-level2-cold-cache-bench  Scatter with cold miss/replacement paths\n  \
           e1m1-gpu-polygon-resident-base-cache-bench  Direct-map resident L0 GT3/GT4 packets\n  \
           e1m1-gpu-polygon-resident-base-cache-fast-bench  Resident L0 with batch-folded counters\n  \
           e1m1-subdivision-cache-level2-layout-control-bench  Control with original slab addresses\n  \
           e1m1-block-frustum-bench  A/B cached 16-face conservative frustum blocks\n  \
           e1m1-hierarchical-block-frustum-bench  A/B conservative 64-to-16-face hierarchy\n  \
           e1m1-block-frustum-32-bench  A/B cached 32-face conservative frustum blocks\n  \
           e1m1-plane-run-cache-bench  A/B exact repeated BSP-plane side tests\n  \
           block-frustum-regress  Visual, route and shipping validation of 16-face blocks\n  \
           gpu-polygon-clip-regress  Visual, route and shipping validation of GPU clip ownership\n  \
           selection-cache-regress  Visual and E1M2/E1M3 validation of selected-face reuse\n  \
           selection-cache-ship-boot  Release-image boot/headroom check with selected-face reuse\n  \
           e1m1-renderer-census  Diagnose selection and projection reuse on that fixed route\n  \
           e1m1-gpu-census  Capture GP0 work for the accepted selector on the fixed route\n  \
           e1m1-gpu-polygon-census  Capture GP0 work for the GPU-clipped candidate\n  \
           e1m2-e1m3-route-regress  Walk E1M2 and E1M3's authored progression into E1M4 headlessly\n  \
           survival-regress  Walk E1M1's authored hazards: burn, fall, drown, die, respawn\n  \
           systems-regress  Prove Start's authored lava spouts headlessly\n  \
           combat-regress  Prove shotgun damage and death against cooked E1M1\n  \
           monster-regress  Prove Soldier and Dog runtime against cooked E1M1\n  \
           monsterjump-regress  Prove E1M6's authored monster-jump flight on MIPS\n  \
           bestiary-regress  Fight authored E1M2 and E1M4 monsters with ordinary input\n  \
           episode1-regress  Walk E1M7's sigil chain and prove the episode-completion state\n  \
           arsenal-regress  Prove authored pickups and six weapons across cooked E1M1-E1M5\n  \
           audio-regress  Verify persistent-bank reuse plus an R2-triggered one-shot\n  \
           ambient-regress  Verify audible spatial looping ambience in the Rust image\n  \
           regress   Reserved for the complete Rust Episode 1 route probe\n  \
           hardware  Reserved for the complete Rust on-console acceptance image\n\n\
         Options:\n  \
           --quake-dir PATH  Use a local Quake/id1 directory\n  \
           --psoxide PATH    Override the pinned SDK with a PSoXide checkout\n  \
           --allow-psoxide-drift  Accept a --psoxide checkout whose revision or\n                    \
           dirty state does not match the pinned expectation"
    );
}

/// PSoXide source used for this invocation.
enum PsoxideSource {
    /// `.psoxide` hydrated from the pinned remote revision.
    Pinned { rev: String },
    /// `.psoxide` hydrated from a local checkout whose HEAD and dirty state
    /// were verified against [`PSOXIDE_REV`].
    LocalCheckout {
        path: PathBuf,
        revision: String,
        dirty_files: usize,
    },
    /// `--psoxide` pointed at a frontend binary; the existing `.psoxide`
    /// hydration is reused as-is.
    FrontendBinary { path: PathBuf },
}

impl PsoxideSource {
    fn describe(&self) -> String {
        match self {
            Self::Pinned { rev } => format!("pinned {rev}"),
            Self::LocalCheckout {
                path,
                revision,
                dirty_files,
            } => {
                let cleanliness = if *dirty_files == 0 {
                    "clean".to_string()
                } else {
                    format!("DIRTY, {dirty_files} changed files")
                };
                format!("local {} at {revision} ({cleanliness})", path.display())
            }
            Self::FrontendBinary { path } => format!(
                "frontend binary {} over existing hydration (revision from stamp below)",
                path.display()
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShippingInputs {
    quake_revision: String,
    psoxide_revision: String,
    psoxide_source_kind: &'static str,
    pak0_sha256: String,
    pak0_bytes: u64,
    guest_recipe_sha256: String,
    rust_toolchain_sha256: String,
    rustc_version: String,
    cargo_version: String,
}

#[derive(Debug, Eq, PartialEq)]
struct ArtifactProvenance {
    file: &'static str,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolchainIdentity {
    rust_toolchain_sha256: String,
    rustc_version: String,
    cargo_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GuestRecipe {
    sha256: String,
    toolchain: ToolchainIdentity,
}

struct GuestStageLock {
    path: PathBuf,
}

impl Drop for GuestStageLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct PreparedGuestStage {
    path: PathBuf,
    _lock: GuestStageLock,
}

const HYDRATION_STAMP: &str = ".hydration-stamp";

fn git_capture(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!("git {} failed in {}", args.join(" "), dir.display()).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn hydrate_psoxide(
    root: &Path,
    requested: Option<&Path>,
    allow_drift: bool,
) -> Result<PsoxideSource> {
    let destination = root.join(".psoxide");
    let source = match requested {
        Some(source) if source.join("sdk/psoxide.ld").is_file() => {
            let source = source.canonicalize()?;
            let revision = git_capture(&source, &["rev-parse", "HEAD"]).map_err(|_| {
                format!(
                    "{} is not a Git checkout; the source contract requires a verifiable revision",
                    source.display()
                )
            })?;
            let dirty_files = git_capture(&source, &["status", "--porcelain"])?
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            let drifted = revision != PSOXIDE_REV || dirty_files != 0;
            if drifted && !allow_drift {
                return Err(format!(
                    "PSoXide override {} is {revision} with {dirty_files} dirty files, but this build expects clean {PSOXIDE_REV}; pass --allow-psoxide-drift to accept an unverified SDK",
                    source.display()
                )
                .into());
            }
            if drifted {
                eprintln!(
                    "quake-psx-build: WARNING: building against unpinned PSoXide {revision} ({dirty_files} dirty files); results are not reproducible from the pinned revision"
                );
            }
            psoxide_link::hydrate(&source, &destination, None, true)?;
            let source = PsoxideSource::LocalCheckout {
                path: source,
                revision,
                dirty_files,
            };
            write_hydration_stamp(&destination, &source)?;
            source
        }
        Some(path) if path.is_file() => PsoxideSource::FrontendBinary {
            path: path.to_path_buf(),
        },
        Some(path) => {
            return Err(format!("{} is not a PSoXide checkout or frontend", path.display()).into())
        }
        None => {
            // hydrate_pinned copies the revision compiled into psoxide-link.
            // Refuse a mismatched pin before writing the destination.
            let linked = linked_psoxide_link_rev()?;
            let rev = default_hydration_plan(&linked, PSOXIDE_REV)?;
            psoxide_link::hydrate_pinned(&destination, &rev, true)?;
            let source = PsoxideSource::Pinned { rev };
            write_hydration_stamp(&destination, &source)?;
            source
        }
    };
    if !destination.join("sdk/psoxide.ld").is_file() {
        return Err("PSoXide SDK hydration did not produce sdk/psoxide.ld".into());
    }
    if matches!(source, PsoxideSource::FrontendBinary { .. }) {
        let stamp = fs::read_to_string(destination.join(HYDRATION_STAMP))
            .unwrap_or_else(|_| "unstamped hydration predating source verification".to_string());
        println!("PSoXide existing hydration stamp: {}", stamp.trim());
    }
    Ok(source)
}

/// Resolved commit of the psoxide-link dependency this binary was compiled
/// with, parsed from the embedded lockfile fragment.
fn linked_psoxide_link_rev() -> Result<String> {
    parse_psoxide_link_rev(include_str!("../../Cargo.lock"))
}

fn parse_psoxide_link_rev(lock: &str) -> Result<String> {
    let mut in_package = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_package = false;
        }
        if line == "name = \"psoxide-link\"" {
            in_package = true;
        }
        if in_package && line.starts_with("source = ") {
            let rev = line
                .rsplit('#')
                .next()
                .unwrap_or_default()
                .trim_end_matches('"');
            if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(rev.to_string());
            }
            return Err(format!("unparseable psoxide-link source: {line}").into());
        }
    }
    Err("Cargo.lock does not resolve psoxide-link from a Git source".into())
}

/// Return the revision allowed for default hydration.
fn default_hydration_plan(linked: &str, expected: &str) -> Result<String> {
    if linked == expected {
        return Ok(linked.to_string());
    }
    Err(format!(
        "default hydration is disabled: PSoXide {expected} is not published and the \
         compiled hydration library resolves {linked}, so hydrating would copy the \
         {linked} sources mislabeled as {expected}. Supply the pinned local worktree \
         instead: --psoxide ../PSoXide-rc1-pin"
    )
    .into())
}

fn write_hydration_stamp(destination: &Path, source: &PsoxideSource) -> Result<()> {
    fs::write(
        destination.join(HYDRATION_STAMP),
        format!("{}\n", source.describe()),
    )?;
    Ok(())
}

fn resolve_pak(root: &Path, quake_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = quake_dir {
        for candidate in [
            dir.join("ID1/PAK0.PAK"),
            dir.join("id1/pak0.pak"),
            dir.join("PAK0.PAK"),
            dir.join("pak0.pak"),
        ] {
            if candidate.is_file() {
                validate_hash(&candidate, PAK0_SHA256, "Quake 1.06 PAK0.PAK")?;
                return Ok(candidate);
            }
        }
        return Err(format!("no PAK0.PAK found below {}", dir.display()).into());
    }

    let cache = root.join(".quakepsx/cache");
    let archive = cache.join("quake106.zip");
    let installer = cache.join("installer");
    let extracted = cache.join("shareware");
    let pak = extracted.join("ID1/PAK0.PAK");
    fs::create_dir_all(&cache)?;
    if !archive.is_file() || sha256_path(&archive)? != SHAREWARE_SHA256 {
        let download = cache.join("quake106.zip.download");
        if download.exists() {
            fs::remove_file(&download)?;
        }
        let mut command = Command::new(require_tool(&["curl"])?);
        command
            .args(["--fail", "--location", "--show-error", "--output"])
            .arg(&download)
            .arg(SHAREWARE_URL);
        run(&mut command)?;
        validate_hash(&download, SHAREWARE_SHA256, "original quake106.zip")?;
        fs::rename(download, &archive)?;
    }
    validate_hash(&archive, SHAREWARE_SHA256, "original quake106.zip")?;
    if !pak.is_file() || sha256_path(&pak)? != PAK0_SHA256 {
        fs::create_dir_all(&installer)?;
        fs::create_dir_all(&extracted)?;
        let mut unzip = Command::new(require_tool(&["unzip"])?);
        unzip
            .args(["-o", "-q"])
            .arg(&archive)
            .arg("-d")
            .arg(&installer);
        run(&mut unzip)?;
        let resource = installer.join("resource.1");
        if !resource.is_file() {
            return Err("quake106.zip did not contain resource.1".into());
        }
        let mut extract = Command::new(require_tool(&["7zz", "7z"])?);
        extract
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", extracted.display()))
            .arg(resource);
        run(&mut extract)?;
    }
    validate_hash(&pak, PAK0_SHA256, "Quake 1.06 PAK0.PAK")?;
    Ok(pak)
}

fn cook_assets(root: &Path, pak: &Path, force: bool) -> Result<()> {
    let stamp = root.join(".quakepsx/cooked-pak0.sha256");
    let recipe_hash = asset_recipe_hash(root)?;
    let expected = format!("{PAK0_SHA256}\n{recipe_hash}");
    if !force
        && cooked_assets_complete(root)
        && fs::read_to_string(&stamp)
            .map(|v| v.trim() == expected)
            .unwrap_or(false)
    {
        validate_cooked_episode(root)?;
        println!("Episode 1 assets match the verified shareware PAK and current cooker");
        return Ok(());
    }
    cook_graphics(root, pak)?;
    let summaries = cook_episode_maps(root, pak)?;
    write_episode_directory(root)?;
    let log = root.join(".quakepsx/logs/cook.log");
    fs::create_dir_all(log.parent().unwrap())?;
    fs::write(&log, summaries.join("\n") + "\n")?;
    if !cooked_assets_complete(root) {
        return Err("asset cooker did not produce Start and E1M1-E1M8".into());
    }
    validate_cooked_episode(root)?;
    fs::create_dir_all(stamp.parent().unwrap())?;
    fs::write(stamp, format!("{expected}\n"))?;
    println!("cooked Start and E1M1-E1M8; log: {}", log.display());
    Ok(())
}

fn cook_episode_maps(root: &Path, pak_path: &Path) -> Result<Vec<String>> {
    let pak_bytes = fs::read(pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    let entity_map = fs::read_to_string(root.join("tools/cfg/id1/entmap.txt"))?;
    let model_map = fs::read_to_string(root.join("tools/cfg/id1/mdlmap.txt"))?;
    let sound_map = fs::read_to_string(root.join("tools/cfg/id1/sfxmap.txt"))?;
    let resource_list = fs::read_to_string(root.join("tools/cfg/id1/reslist.txt"))?;
    let model_props = fs::read_to_string(root.join("tools/cfg/id1/mdlprops.txt"))?;
    let global_sounds = cook_global_sounds(&pak, &sound_map, &resource_list)?;
    if global_sounds.stats.sound_count != SOUND_GLOBAL_EFFECTS {
        return Err(format!(
            "global sound catalog has {} entries, expected {SOUND_GLOBAL_EFFECTS}",
            global_sounds.stats.sound_count
        )
        .into());
    }
    let global_destination = root.join("id1psx/sounds/global.qsb");
    fs::create_dir_all(global_destination.parent().unwrap())?;
    let global_temporary = root.join("id1psx/sounds/.global.qsb.tmp");
    fs::write(&global_temporary, &global_sounds.data)?;
    fs::rename(&global_temporary, &global_destination)?;
    let config = MapCookConfig {
        entity_map: &entity_map,
        model_map: &model_map,
        sound_map: &sound_map,
        resource_list: &resource_list,
        model_props: &model_props,
        global_sounds: &global_sounds,
        sky: SkyEncoding::Layered,
    };
    let destination = root.join("id1psx/maps");
    fs::create_dir_all(&destination)?;
    let mut summaries = vec![format!(
        "global sounds: {} bytes, {} sounds, {} looping, {} SPU bytes, high-water {:#x}",
        global_sounds.data.len(),
        global_sounds.stats.sound_count,
        global_sounds.stats.looping_sounds,
        global_sounds.stats.payload_bytes,
        global_sounds.stats.spu_high_water,
    )];
    let mut monolithic_bytes = 0usize;
    let mut local_bytes = 0usize;
    for map in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ] {
        let bsp = Bsp::parse(pak.require(&format!("maps/{map}.bsp"))?)?;
        let cooked = cook_map(&pak, &bsp, config)?;
        let output_path = destination.join(format!("{map}.psb"));
        let temporary = destination.join(format!(".{map}.psb.tmp"));
        fs::write(&temporary, &cooked.psb)?;
        fs::rename(&temporary, &output_path)?;
        let old_bank_bytes = 4
            + cooked.sound_stats.combined_sound_count * SoundEffect::SIZE
            + cooked.sound_stats.combined_payload_bytes;
        let new_bank_bytes = quake_formats::SOUND_BANK_HEADER_BYTES
            + cooked.sound_stats.sound_count
                * (SoundEffect::SIZE + quake_formats::SOUND_BANK_RATE_BYTES)
            + cooked.sound_stats.payload_bytes;
        monolithic_bytes += old_bank_bytes;
        local_bytes += new_bank_bytes;
        summaries.push(format!(
            "{map}: {} bytes, {} entities, {} models, {} local/{} combined sounds, {} local looping, {} local/{} combined SPU bytes, high-water {:#x}, sound {} -> {} bytes",
            cooked.psb.len(),
            cooked.entity_count,
            cooked.model_count,
            cooked.sound_stats.sound_count,
            cooked.sound_stats.combined_sound_count,
            cooked.sound_stats.looping_sounds,
            cooked.sound_stats.payload_bytes,
            cooked.sound_stats.combined_payload_bytes,
            cooked.sound_stats.spu_high_water,
            old_bank_bytes,
            new_bank_bytes,
        ));
        if !cooked.sound_stats.omitted_for_space.is_empty() {
            summaries.push(format!(
                "{map}: omitted for SPU space: {}",
                cooked.sound_stats.omitted_for_space.join(", ")
            ));
        }
    }
    let persistent_bytes = global_sounds.data.len() + local_bytes;
    summaries.push(format!(
        "sound corpus dedup: {monolithic_bytes} -> {persistent_bytes} bytes (-{}, global loaded once)",
        monolithic_bytes - persistent_bytes
    ));
    Ok(summaries)
}

fn cook_graphics(root: &Path, pak_path: &Path) -> Result<()> {
    let pak_bytes = fs::read(pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    let pic_map = fs::read_to_string(root.join("tools/cfg/id1/picmap.txt"))?;
    let graphics = cook_gfx(&pak, &pic_map)?;
    let destination = root.join("id1psx/gfx.dat");
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(&destination, graphics)?;
    println!("Rust-cooked {}", destination.display());
    Ok(())
}

fn write_episode_directory(root: &Path) -> Result<()> {
    let mut encoder = EpisodeDirectoryEncoder::new();
    for (slot, map) in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("id1psx/maps/{map}.psb"));
        let bytes = fs::read(&path)?;
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader)
            .map_err(|error| format!("cannot index cooked {}: {error}", path.display()))?;
        if !encoder.set(slot, 100 + slot as u32, &index) {
            return Err(format!("Episode directory rejected slot {slot} for {map}").into());
        }
    }
    let destination = root.join("id1psx/maps/episode.qidx");
    let temporary = root.join("id1psx/maps/.episode.qidx.tmp");
    fs::write(&temporary, encoder.finish())?;
    fs::rename(&temporary, &destination)?;
    Ok(())
}

fn validate_episode_directory(root: &Path) -> Result<()> {
    let path = root.join("id1psx/maps/episode.qidx");
    let bytes = fs::read(&path)?;
    if bytes.len() != EPISODE_DIRECTORY_BYTES {
        return Err(format!(
            "{} is {} bytes, expected {EPISODE_DIRECTORY_BYTES}",
            path.display(),
            bytes.len()
        )
        .into());
    }
    for (slot, map) in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ]
    .into_iter()
    .enumerate()
    {
        let cooked = fs::read(root.join(format!("id1psx/maps/{map}.psb")))?;
        let mut reader = SliceReader::new(&cooked);
        let expected = PsbIndex::read(&mut reader)
            .map_err(|error| format!("cannot validate cooked index for {map}: {error}"))?;
        let actual = episode_directory_index(&bytes, 100 + slot as u32)
            .map_err(|error| format!("{} is malformed: {error:?}", path.display()))?
            .ok_or_else(|| format!("{} omits {map}", path.display()))?;
        if actual != expected {
            return Err(format!("{} has a stale index for {map}", path.display()).into());
        }
    }
    Ok(())
}

/// Prove every cooked map still fits the guest's one resident-map arena.
///
/// The guest reserves [`RESIDENT_MAP_ARENA_BYTES`] once and reuses it for every
/// Episode 1 map, so a cooked map that outgrows the arena has nowhere to load.
/// Without this the overflow only surfaces on the guest, as a null bump
/// allocation far from the cause. Loading each map through an arena of exactly
/// the guest's size reproduces the guest's own capacity check on the host.
fn assert_cooked_maps_fit_resident_arena(root: &Path) -> Result<()> {
    const MAX_RENDER_FACE_COUNT: usize = 6_614;
    const MAX_VISIBLE_FACE_COUNT: usize = 1_325;
    const EXPECTED_FACE_COUNTS: [usize; 9] = [
        5_750, 5_890, 5_812, 5_566, 6_614, 5_273, 4_408, 1_780, 3_443,
    ];
    const EXPECTED_MAX_PVS_FACES: [usize; 9] =
        [1_176, 1_325, 1_052, 898, 1_046, 939, 1_201, 913, 1_306];
    let mut largest_required = 0usize;
    let mut pickup_count = 0usize;
    let mut floor_boundary_pickups = 0usize;
    for (map_index, map) in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("id1psx/maps/{map}.psb"));
        let bytes = fs::read(&path)?;
        let mut reader = SliceReader::new(&bytes);
        let mut resident = ResidentMap::with_capacity(RESIDENT_MAP_ARENA_BYTES);
        if let Err(error) = resident.load(1, &mut reader) {
            return Err(format!(
                "cooked {map} does not fit the guest's {RESIDENT_MAP_ARENA_BYTES}-byte resident \
                 arena: {error:?}"
            )
            .into());
        }
        let models = resident.alias_models();
        let sprite_ids = (0..models.len())
            .filter_map(|index| models.model_at(index))
            .filter_map(|model| alias_model_is_sprite(model.header()).then_some(model.header().id))
            .collect::<Vec<_>>();
        if sprite_ids != [quake_core::effects::BUBBLE_SPRITE_MODEL_ID] {
            return Err(format!(
                "cooked {map} has sprite model IDs {sprite_ids:?}, expected only the resident death bubble"
            )
            .into());
        }
        let face_count = resident.faces().len();
        if face_count != EXPECTED_FACE_COUNTS[map_index] || face_count > MAX_RENDER_FACE_COUNT {
            return Err(format!(
                "cooked {map} has {face_count} faces; expected {} and renderer capacity is {MAX_RENDER_FACE_COUNT}",
                EXPECTED_FACE_COUNTS[map_index]
            )
            .into());
        }
        let leaves = resident.leaves();
        let marks = resident.mark_surfaces();
        let visibility = resident.visibility();
        let world = resident
            .brush_models()
            .get(0)
            .ok_or_else(|| format!("cooked {map} has no world model"))?;
        let visible_leaf_count = world.visible_leaves.max(0) as usize;
        let row_bytes = (visible_leaf_count + 7) >> 3;
        let mut maximum_pvs_faces = 0usize;
        let mut pvs = vec![0u8; row_bytes];
        let mut marked = vec![false; face_count];
        for source_leaf_index in 1..leaves.len() {
            let source_leaf = leaves.get(source_leaf_index).expect("validated leaf");
            if source_leaf.visibility_offset < 0 {
                continue;
            }
            pvs.fill(0);
            let mut source = source_leaf.visibility_offset as usize;
            let mut destination = 0usize;
            while destination < row_bytes {
                let value = *visibility.get(source).ok_or_else(|| {
                    format!("cooked {map} leaf {source_leaf_index} has truncated PVS")
                })?;
                source += 1;
                if value != 0 {
                    pvs[destination] = value;
                    destination += 1;
                    continue;
                }
                let run = *visibility.get(source).ok_or_else(|| {
                    format!("cooked {map} leaf {source_leaf_index} has truncated PVS run")
                })? as usize;
                source += 1;
                if run == 0 || destination + run > row_bytes {
                    return Err(format!(
                        "cooked {map} leaf {source_leaf_index} has invalid PVS run {run}"
                    )
                    .into());
                }
                destination += run;
            }
            marked.fill(false);
            for visible_index in 0..visible_leaf_count {
                if pvs[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                    continue;
                }
                let visible_leaf = leaves
                    .get(visible_index + 1)
                    .ok_or_else(|| format!("cooked {map} PVS references a missing leaf"))?;
                let start = visible_leaf.first_mark_surface as usize;
                let end = start + visible_leaf.mark_surface_count as usize;
                for mark_index in start..end {
                    let face = marks.get(mark_index).ok_or_else(|| {
                        format!("cooked {map} leaf mark range exceeds the mark-surface lump")
                    })? as usize;
                    *marked.get_mut(face).ok_or_else(|| {
                        format!("cooked {map} mark surface references missing face {face}")
                    })? = true;
                }
            }
            maximum_pvs_faces = maximum_pvs_faces.max(marked.iter().filter(|face| **face).count());
        }
        if maximum_pvs_faces != EXPECTED_MAX_PVS_FACES[map_index]
            || maximum_pvs_faces > MAX_VISIBLE_FACE_COUNT
        {
            return Err(format!(
                "cooked {map} PVS exposes at most {maximum_pvs_faces} faces; expected {} and renderer capacity is {MAX_VISIBLE_FACE_COUNT}",
                EXPECTED_MAX_PVS_FACES[map_index]
            )
            .into());
        }
        for entity in resident.entities().iter() {
            if pickup_for_entity(entity.class_name, entity.spawn_flags).is_none() {
                continue;
            }
            pickup_count += 1;
            let leaf = resident
                .point_leaf_index(entity.origin)
                .ok_or_else(|| format!("cooked {map} pickup has no BSP leaf"))?;
            if leaf == 0 {
                floor_boundary_pickups += 1;
                let lifted = quake_formats::Vec3I32 {
                    z: entity.origin.z.saturating_add(16 << 12),
                    ..entity.origin
                };
                let lifted_leaf = resident
                    .point_leaf_index(lifted)
                    .ok_or_else(|| format!("cooked {map} floor pickup has no lifted BSP leaf"))?;
                if lifted_leaf == 0 {
                    return Err(format!(
                        "cooked {map} floor pickup remains in solid leaf 0 after its visibility probe"
                    )
                    .into());
                }
            }
        }
        let mut probe = ResidentMap::with_capacity(0);
        let mut probe_reader = SliceReader::new(&bytes);
        let required = match probe.load(1, &mut probe_reader) {
            Err(ResidentMapLoadError::TooLarge { required, .. }) => required,
            Err(error) => {
                return Err(format!("cannot measure cooked {map} resident bytes: {error:?}").into())
            }
            Ok(()) => return Err(format!("cooked {map} unexpectedly needs zero bytes").into()),
        };
        largest_required = largest_required.max(required);
    }
    if largest_required != 865_958 {
        return Err(format!(
            "indexed resident census drifted: largest map needs {largest_required} bytes, expected 865958"
        )
        .into());
    }
    if pickup_count != 488 || floor_boundary_pickups != 320 {
        return Err(format!(
            "pickup visibility census drifted: {pickup_count} pickups, {floor_boundary_pickups} on floor boundaries; expected 488/320"
        )
        .into());
    }
    println!(
        "PSB5 resident arena: {largest_required} bytes required, {RESIDENT_MAP_ARENA_BYTES} reserved ({} bytes margin)",
        RESIDENT_MAP_ARENA_BYTES - largest_required
    );
    println!(
        "pickup visibility leaves: {pickup_count} authored, {floor_boundary_pickups} lifted out of solid leaf 0"
    );
    Ok(())
}

/// Pin PSB5 plus the one QSB1 global bank against the last PSB1 corpus.
/// This catches accidental content/chunk changes while making both compact
/// records and persistent-sound dedup visible in every validation run.
fn validate_indexed_psb4_census(root: &Path) -> Result<()> {
    const MAPS: [(&str, usize, usize); 9] = [
        ("start", 1_769_840, 1_464_806),
        ("e1m1", 1_862_013, 1_549_333),
        ("e1m2", 2_076_988, 1_756_594),
        ("e1m3", 2_147_866, 1_844_570),
        ("e1m4", 2_096_303, 1_781_981),
        ("e1m5", 2_036_505, 1_713_133),
        ("e1m6", 1_990_529, 1_688_683),
        ("e1m7", 1_601_558, 1_389_440),
        ("e1m8", 1_646_042, 1_417_964),
    ];
    let mut legacy_total = 0usize;
    let mut compact_total = 0usize;
    for (map, legacy_bytes, expected_compact_bytes) in MAPS {
        let path = root.join(format!("id1psx/maps/{map}.psb"));
        let bytes = fs::read(&path)?;
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader)
            .map_err(|error| format!("cannot census cooked {map}: {error}"))?;
        if index.version() != PsbVersion::IndexedV5 {
            return Err(format!(
                "cooked {map} is {:?}, expected explicit indexed PSB5",
                index.version()
            )
            .into());
        }
        if bytes.len() != expected_compact_bytes {
            return Err(format!(
                "cooked {map} is {} bytes, expected PSB5/QSB1 suffix size {expected_compact_bytes} (PSB1 was {legacy_bytes})",
                bytes.len()
            )
            .into());
        }
        legacy_total += legacy_bytes;
        compact_total += bytes.len();
        let map_saved_sectors = legacy_bytes.div_ceil(2048) - bytes.len().div_ceil(2048);
        println!(
            "PSB5 {map}: {legacy_bytes} -> {} bytes (-{}, {map_saved_sectors} ISO sectors)",
            bytes.len(),
            legacy_bytes - bytes.len()
        );
    }
    let global_bytes = fs::metadata(root.join("id1psx/sounds/global.qsb"))?.len() as usize;
    let persistent_total = compact_total + global_bytes;
    if legacy_total != 17_227_644
        || compact_total != 14_606_504
        || global_bytes != 159_418
        || persistent_total != 14_765_922
    {
        return Err(format!(
            "PSB5/QSB1 episode census drifted: {legacy_total} -> {compact_total} + {global_bytes}"
        )
        .into());
    }
    let old_sectors = MAPS
        .iter()
        .map(|(_, legacy, _)| legacy.div_ceil(2048))
        .sum::<usize>();
    let persistent_sectors = MAPS
        .iter()
        .map(|(_, _, compact)| compact.div_ceil(2048))
        .sum::<usize>()
        + global_bytes.div_ceil(2048);
    println!(
        "PSB5/QSB1 Episode 1: {legacy_total} -> {persistent_total} bytes (-{}, {} ISO sectors; {compact_total} map suffix bytes + {global_bytes} global bytes)",
        legacy_total - persistent_total,
        old_sectors - persistent_sectors,
    );
    Ok(())
}

fn validate_cooked_episode(root: &Path) -> Result<()> {
    validate_episode_directory(root)?;
    validate_persistent_sound_corpus(root)?;
    validate_indexed_psb4_census(root)?;
    assert_cooked_maps_fit_resident_arena(root)?;
    let maps: [(&str, &[&str]); 9] = [
        ("start", &["E1M1"]),
        ("e1m1", &["E1M2"]),
        ("e1m2", &["E1M3"]),
        ("e1m3", &["E1M4"]),
        ("e1m4", &["E1M5", "E1M8"]),
        ("e1m5", &["E1M6"]),
        ("e1m6", &["E1M7"]),
        ("e1m7", &["START"]),
        ("e1m8", &["E1M5"]),
    ];
    for (map, expected_transitions) in maps {
        let path = root.join(format!("id1psx/maps/{map}.psb"));
        let bytes = fs::read(&path)?;
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
        for required in [
            LumpKind::Vertices,
            LumpKind::Planes,
            LumpKind::Faces,
            LumpKind::Models,
            LumpKind::Entities,
        ] {
            if index.lump(required).len == 0 {
                return Err(format!("{} has an empty {required:?} lump", path.display()).into());
            }
        }
        let strings = cooked_lump(&bytes, &index, LumpKind::Strings);
        let entity_bytes = cooked_lump(&bytes, &index, LumpKind::Entities);
        let entities = RecordSlice::<MapEntity>::new(entity_bytes)
            .ok_or_else(|| format!("{} has malformed entities", path.display()))?;
        let mut transitions = Vec::new();
        for entity in entities.iter().filter(|entity| entity.class_name == 0x47) {
            let destination = cooked_string(strings, entity.string)
                .ok_or_else(|| format!("{map} has a malformed changelevel destination"))?;
            if is_episode_one_map(destination) {
                transitions.push(destination);
            }
        }
        transitions.sort_unstable();
        transitions.dedup();
        if transitions.as_slice() != expected_transitions {
            return Err(format!(
                "{map} changelevel destinations are {transitions:?}, expected {expected_transitions:?}"
            )
            .into());
        }
    }
    Ok(())
}

fn validate_persistent_sound_corpus(root: &Path) -> Result<()> {
    let global_path = root.join("id1psx/sounds/global.qsb");
    let global_bytes = fs::read(&global_path)?;
    let (global, global_effects, _, _) = decode_sound_bank(&global_bytes)
        .map_err(|error| format!("{} is malformed: {error:?}", global_path.display()))?;
    if global.kind != SoundBankKind::Global || global_effects.len() != SOUND_GLOBAL_EFFECTS {
        return Err("persistent sound catalog is not the canonical 34-entry global bank".into());
    }
    let mut monolithic_bytes = 0usize;
    let mut suffix_bytes = 0usize;
    let mut max_spu = global.spu_high_water;
    for map in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ] {
        let path = root.join(format!("id1psx/maps/{map}.psb"));
        let bytes = fs::read(&path)?;
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader)
            .map_err(|error| format!("cannot index {}: {error}", path.display()))?;
        let sound = cooked_lump(&bytes, &index, LumpKind::SoundData);
        let (local, local_effects, _, _) = decode_sound_bank(sound)
            .map_err(|error| format!("{map} sound suffix is malformed: {error:?}"))?;
        if local.kind != SoundBankKind::Local
            || local.dependency_hash != global.content_hash
            || local.payload_base != global.spu_high_water
            || global_effects.len() + local_effects.len() > quake_formats::SOUND_MAX_EFFECTS
        {
            return Err(format!("{map} sound suffix does not bind to the global catalog").into());
        }
        for local_effect in local_effects.iter() {
            if global_effects
                .iter()
                .any(|global_effect| global_effect.id == local_effect.id)
            {
                return Err(format!(
                    "{map} local sound {:#04x} duplicates the global catalog",
                    local_effect.id
                )
                .into());
            }
        }
        monolithic_bytes += 4
            + (global_effects.len() + local_effects.len()) * SoundEffect::SIZE
            + (local.spu_high_water - quake_formats::SOUND_SPU_BASE) as usize;
        suffix_bytes += sound.len();
        max_spu = max_spu.max(local.spu_high_water);
    }
    let persistent_bytes = global_bytes.len() + suffix_bytes;
    if max_spu > SOUND_SPU_END || persistent_bytes >= monolithic_bytes {
        return Err("persistent sound corpus has no validated size or SPU benefit".into());
    }
    println!(
        "QSB1 sound corpus: {monolithic_bytes} -> {persistent_bytes} bytes (-{}), {} global sounds, SPU high-water {max_spu:#x}/{SOUND_SPU_END:#x}",
        monolithic_bytes - persistent_bytes,
        global_effects.len(),
    );
    Ok(())
}

fn cooked_lump<'a>(bytes: &'a [u8], index: &PsbIndex, kind: LumpKind) -> &'a [u8] {
    let range = index.lump(kind);
    &bytes[range.offset as usize..range.end() as usize]
}

fn cooked_string(strings: &[u8], offset: u16) -> Option<&str> {
    let tail = strings.get(offset as usize..)?;
    let end = tail.iter().position(|&byte| byte == 0)?;
    std::str::from_utf8(&tail[..end]).ok()
}

fn is_episode_one_map(name: &str) -> bool {
    matches!(
        name,
        "START" | "E1M1" | "E1M2" | "E1M3" | "E1M4" | "E1M5" | "E1M6" | "E1M7" | "E1M8"
    )
}

fn asset_recipe_hash(root: &Path) -> Result<String> {
    asset_recipe_hash_with_psoxide_revision(root, PSOXIDE_REV)
}

fn asset_recipe_hash_with_psoxide_revision(root: &Path, psoxide_revision: &str) -> Result<String> {
    let mut files = Vec::new();
    for relative in [
        "crates/quake-cook/Cargo.toml",
        "crates/quake-cook/src",
        "crates/quake-formats/Cargo.toml",
        "crates/quake-formats/src",
        ".psoxide/engine/crates/psx-bsp/Cargo.toml",
        ".psoxide/engine/crates/psx-bsp/src",
        ".psoxide/engine/crates/psx-render-contract/Cargo.toml",
        ".psoxide/engine/crates/psx-render-contract/src",
        "tools/cfg/id1",
    ] {
        collect_recipe_files(&root.join(relative), &mut files)?;
    }
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"quake-psx-asset-recipe\0");
    hash.update(psoxide_revision.as_bytes());
    hash.update([0]);
    let mut buffer = [0u8; 64 * 1024];
    for path in files {
        hash.update(path.strip_prefix(root)?.to_string_lossy().as_bytes());
        hash.update([0]);
        let mut file = File::open(path)?;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn collect_recipe_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_recipe_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn cooked_assets_complete(root: &Path) -> bool {
    let mut files = vec![
        root.join("id1psx/gfx.dat"),
        root.join("id1psx/sounds/global.qsb"),
        root.join("id1psx/maps/start.psb"),
        root.join("id1psx/maps/episode.qidx"),
    ];
    for index in 1..=8 {
        files.push(root.join(format!("id1psx/maps/e1m{index}.psb")));
    }
    files.iter().all(|path| path.is_file())
}

fn game_exe(root: &Path) -> PathBuf {
    root.join("game/target/mipsel-sony-psx/release/quake-psx.exe")
}

fn tool_version(root: &Path, tool: &str) -> Result<String> {
    let output = Command::new(require_tool(&[tool])?)
        .current_dir(root)
        .args(["--version", "--verbose"])
        .output()?;
    if !output.status.success() {
        return Err(format!("{tool} --version --verbose failed in {}", root.display()).into());
    }
    let version = String::from_utf8(output.stdout)?.trim().to_string();
    if version.is_empty() {
        return Err(format!("{tool} --version returned no identity").into());
    }
    Ok(version)
}

fn toolchain_identity(root: &Path) -> Result<ToolchainIdentity> {
    let rust_toolchain = root.join("rust-toolchain.toml");
    if !rust_toolchain.is_file() {
        return Err(format!("missing guest toolchain pin {}", rust_toolchain.display()).into());
    }
    Ok(ToolchainIdentity {
        rust_toolchain_sha256: sha256_path(&rust_toolchain)?,
        rustc_version: tool_version(root, "rustc")?,
        cargo_version: tool_version(root, "cargo")?,
    })
}

fn guest_recipe(root: &Path) -> Result<GuestRecipe> {
    let toolchain = toolchain_identity(root)?;
    let sha256 = guest_recipe_hash(root, &toolchain)?;
    Ok(GuestRecipe { sha256, toolchain })
}

fn guest_recipe_hash(root: &Path, toolchain: &ToolchainIdentity) -> Result<String> {
    guest_recipe_hash_with_workspaces(root, toolchain, GUEST_STAGE_WORKSPACES)
}

fn guest_recipe_hash_with_workspaces(
    root: &Path,
    toolchain: &ToolchainIdentity,
    workspaces: &[(&str, &str)],
) -> Result<String> {
    let mut files = guest_recipe_files(root)?;
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"quake-psx-guest-recipe\0");
    hash.update(GUEST_STAGE_SCHEMA.to_le_bytes());
    hash.update(SHIPPING_CARGO_HOME.as_bytes());
    hash.update([0]);
    hash.update(SHIPPING_CARGO_HOME_SCHEMA.to_le_bytes());
    hash.update(toolchain.rust_toolchain_sha256.as_bytes());
    hash.update([0]);
    hash.update(toolchain.rustc_version.as_bytes());
    hash.update([0]);
    hash.update(toolchain.cargo_version.as_bytes());
    hash.update([0]);
    for (path, contents) in workspaces {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(contents.as_bytes());
        hash.update([0]);
    }
    let mut buffer = [0u8; 64 * 1024];
    for path in files {
        let relative = path.strip_prefix(root)?;
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("guest recipe path is not UTF-8: {}", relative.display()))?;
        hash.update(relative.as_bytes());
        hash.update([0]);
        let mut file = File::open(&path)?;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn guest_recipe_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for relative in GUEST_RECIPE_PATHS {
        let path = root.join(relative);
        if !path.exists() {
            return Err(format!("guest recipe input is missing: {}", path.display()).into());
        }
        collect_guest_recipe_files(root, &path, &mut files)?;
    }
    Ok(files)
}

fn collect_guest_recipe_files(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(format!("guest recipe rejects symlink {}", path.display()).into());
    }
    let relative = path.strip_prefix(root)?;
    if guest_recipe_ignored(relative) {
        return Ok(());
    }
    if metadata.is_file() {
        if is_native_source_or_object(relative) {
            return Err(format!(
                "Rust-only guest recipe rejects native source or object {}",
                path.display()
            )
            .into());
        }
        files.push(path.to_path_buf());
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!("unsupported guest recipe input {}", path.display()).into());
    }
    let mut entries = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    for child in entries {
        collect_guest_recipe_files(root, &child, files)?;
    }
    Ok(())
}

fn guest_recipe_ignored(relative: &Path) -> bool {
    if relative
        .components()
        .any(|component| component.as_os_str() == "target")
    {
        return true;
    }
    if relative.starts_with(".psoxide/sdk/crates/psx-font/vendor") {
        // psx-font's generated Rust modules already contain the glyph bytes.
        // The vendor tree is source provenance, not a compiler input, and
        // includes three C headers that must not enter the Rust-only stage.
        return true;
    }
    matches!(
        relative.to_str(),
        Some(".psoxide/.hydration-stamp" | ".psoxide/.psoxide-source")
    )
}

fn is_native_source_or_object(relative: &Path) -> bool {
    let Some(extension) = relative
        .extension()
        .and_then(|extension| extension.to_str())
    else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "c" | "cc"
            | "cpp"
            | "cxx"
            | "h"
            | "hh"
            | "hpp"
            | "hxx"
            | "m"
            | "mm"
            | "s"
            | "asm"
            | "o"
            | "obj"
            | "a"
            | "lib"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "elf"
            | "com"
    )
}

fn audit_rust_only_guest_lock(stage: &Path) -> Result<()> {
    const NATIVE_BUILD_PACKAGES: &[&str] = &[
        "bindgen",
        "cc",
        "clang-sys",
        "cmake",
        "cxx",
        "cxx-build",
        "pkg-config",
        "vcpkg",
    ];
    let lock = stage.join("game/Cargo.lock");
    let text = fs::read_to_string(&lock)?;
    for line in text.lines() {
        let Some(name) = line
            .strip_prefix("name = \"")
            .and_then(|name| name.strip_suffix('"'))
        else {
            continue;
        };
        if NATIVE_BUILD_PACKAGES.contains(&name) {
            return Err(format!(
                "Rust-only guest lock rejects native build package {name} in {}",
                lock.display()
            )
            .into());
        }
    }
    Ok(())
}

fn copy_guest_recipe(root: &Path, destination: &Path) -> Result<()> {
    let files = guest_recipe_files(root)?;
    for source in files {
        let relative = source.strip_prefix(root)?;
        let output = destination.join(relative);
        let parent = output
            .parent()
            .ok_or_else(|| format!("guest stage output has no parent: {}", output.display()))?;
        fs::create_dir_all(parent)?;
        fs::copy(&source, &output)?;
    }
    for (relative, contents) in GUEST_STAGE_WORKSPACES {
        let output = destination.join(relative);
        fs::create_dir_all(output.parent().unwrap())?;
        fs::write(output, contents)?;
    }
    Ok(())
}

fn guest_stage_marker(recipe: &GuestRecipe) -> String {
    format!(
        "schema={GUEST_STAGE_SCHEMA}\nsha256={}\nrust_toolchain_sha256={}\nrustc={}\ncargo={}\n",
        recipe.sha256,
        recipe.toolchain.rust_toolchain_sha256,
        json_escape(&recipe.toolchain.rustc_version),
        json_escape(&recipe.toolchain.cargo_version),
    )
}

fn verify_guest_stage(stage: &Path, recipe: &GuestRecipe) -> Result<()> {
    if fs::symlink_metadata(stage)?.file_type().is_symlink() {
        return Err(format!(
            "canonical guest stage rejects symlink {}; remove it and retry",
            stage.display()
        )
        .into());
    }
    let marker = stage.join(GUEST_STAGE_MARKER);
    let actual_marker = fs::read_to_string(&marker).map_err(|error| {
        format!(
            "canonical guest stage marker is missing or unreadable at {}: {error}",
            marker.display()
        )
    })?;
    let expected_marker = guest_stage_marker(recipe);
    if actual_marker != expected_marker {
        return Err(format!(
            "canonical guest stage marker failed integrity at {}; remove the stage and retry",
            stage.display()
        )
        .into());
    }
    for (relative, expected) in GUEST_STAGE_WORKSPACES {
        let path = stage.join(relative);
        let actual = fs::read_to_string(&path).map_err(|error| {
            format!(
                "canonical guest workspace is missing or unreadable at {}: {error}",
                path.display()
            )
        })?;
        if actual != *expected {
            return Err(format!(
                "canonical guest workspace failed integrity at {}; remove the stage and retry",
                path.display()
            )
            .into());
        }
    }
    let actual_hash = guest_recipe_hash(stage, &recipe.toolchain)?;
    if actual_hash != recipe.sha256 {
        return Err(format!(
            "canonical guest stage content failed integrity at {}: expected {}, got {}; remove the stage and retry",
            stage.display(), recipe.sha256, actual_hash
        )
        .into());
    }
    Ok(())
}

fn acquire_guest_stage_lock(base: &Path, recipe: &GuestRecipe) -> Result<GuestStageLock> {
    let path = base.join(format!(".{}.lock", recipe.sha256));
    let mut lock = File::options()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            format!(
                "guest recipe {} is already locked or has a stale lock at {}: {error}",
                recipe.sha256,
                path.display()
            )
        })?;
    writeln!(lock, "pid={}", std::process::id())?;
    lock.sync_all()?;
    Ok(GuestStageLock { path })
}

fn prepare_guest_stage_at(
    root: &Path,
    recipe: &GuestRecipe,
    base: &Path,
) -> Result<PreparedGuestStage> {
    fs::create_dir_all(base)?;
    let base = base.canonicalize()?;
    let lock = acquire_guest_stage_lock(&base, recipe)?;
    let stage = base.join(&recipe.sha256);
    if stage.exists() {
        verify_guest_stage(&stage, recipe)?;
        return Ok(PreparedGuestStage {
            path: stage,
            _lock: lock,
        });
    }

    let temporary = base.join(format!(".{}.stage-{}", recipe.sha256, std::process::id()));
    if temporary.exists() {
        return Err(format!(
            "stale atomic guest stage exists at {}; remove it and retry",
            temporary.display()
        )
        .into());
    }
    fs::create_dir(&temporary)?;
    let staged = (|| -> Result<()> {
        copy_guest_recipe(root, &temporary)?;
        let actual_hash = guest_recipe_hash(&temporary, &recipe.toolchain)?;
        if actual_hash != recipe.sha256 {
            return Err(format!(
                "atomic guest stage changed content: expected {}, got {actual_hash}",
                recipe.sha256
            )
            .into());
        }
        fs::write(
            temporary.join(GUEST_STAGE_MARKER),
            guest_stage_marker(recipe),
        )?;
        verify_guest_stage(&temporary, recipe)?;
        fs::rename(&temporary, &stage)?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    verify_guest_stage(&stage, recipe)?;
    Ok(PreparedGuestStage {
        path: stage,
        _lock: lock,
    })
}

fn prepare_guest_stage(root: &Path, recipe: &GuestRecipe) -> Result<PreparedGuestStage> {
    prepare_guest_stage_at(root, recipe, Path::new(GUEST_STAGE_ROOT))
}

fn reset_guest_target(stage: &Path) -> Result<()> {
    let target = stage.join("game/target");
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    Ok(())
}

fn prepare_shipping_cargo_home_at(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(format!(
            "shipping Cargo home rejects symlink {}; remove it and retry",
            path.display()
        )
        .into());
    }
    let path = path.canonicalize()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    for name in ["config", "config.toml", "credentials", "credentials.toml"] {
        let ambient = path.join(name);
        if ambient.exists() {
            return Err(format!(
                "shipping Cargo home rejects ambient config or credentials {}; remove it and retry",
                ambient.display()
            )
            .into());
        }
    }
    let marker = path.join(SHIPPING_CARGO_HOME_MARKER);
    let expected = format!("schema={SHIPPING_CARGO_HOME_SCHEMA}\n");
    if marker.exists() && fs::symlink_metadata(&marker)?.file_type().is_symlink() {
        return Err(format!(
            "shipping Cargo home marker rejects symlink {}; remove the Cargo home and retry",
            marker.display()
        )
        .into());
    }
    match fs::read_to_string(&marker) {
        Ok(actual) if actual == expected => {}
        Ok(_) => {
            return Err(format!(
            "shipping Cargo home marker failed integrity at {}; remove the Cargo home and retry",
            marker.display()
        )
            .into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut output = File::options().write(true).create_new(true).open(&marker)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                output.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            output.write_all(expected.as_bytes())?;
            output.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

fn reset_shipping_registry_sources(cargo_home: &Path) -> Result<()> {
    let sources = cargo_home.join("registry/src");
    if !sources.exists() {
        return Ok(());
    }
    if fs::symlink_metadata(&sources)?.file_type().is_symlink() {
        return Err(format!(
            "shipping Cargo registry source cache rejects symlink {}; remove it and retry",
            sources.display()
        )
        .into());
    }
    fs::remove_dir_all(&sources)?;
    Ok(())
}

fn prepare_shipping_cargo_home() -> Result<PathBuf> {
    prepare_shipping_cargo_home_at(Path::new(SHIPPING_CARGO_HOME))
}

fn reject_ambient_cargo_configs(current_dir: &Path, cargo_home: &Path) -> Result<()> {
    let mut directory = current_dir.parent();
    while let Some(path) = directory {
        for name in ["config", "config.toml"] {
            let config = path.join(".cargo").join(name);
            if config.exists() {
                return Err(format!(
                    "shipping guest build rejects ambient Cargo config {}; remove it and retry",
                    config.display()
                )
                .into());
            }
        }
        directory = path.parent();
    }
    for name in ["config", "config.toml"] {
        let config = cargo_home.join(name);
        if config.exists() {
            return Err(format!(
                "shipping guest build rejects Cargo home config {}; remove it and retry",
                config.display()
            )
            .into());
        }
    }
    Ok(())
}

/// Optional guest link map requested by `ship-boot`.
static GUEST_LINK_MAP: OnceLock<PathBuf> = OnceLock::new();

/// Ask the guest link to also write a symbol map to `map`.
///
/// `-Map` only makes rust-lld report the link it already performed; it adds
/// no code and no section. The flag reaches the guest cargo invocation alone
/// (see [`build_game`]) and never this process's environment, so the host
/// tools built along the way -- mkisopsx links with `cc`, which rejects
/// `-Map` outright -- are untouched.
fn request_guest_link_map(map: PathBuf) -> Result<()> {
    GUEST_LINK_MAP
        .set(map)
        .map_err(|_| "the guest link map was already requested".into())
}

fn build_game(root: &Path, feature: Option<&str>, fresh_target: bool) -> Result<()> {
    let recipe = guest_recipe(root)?;
    let stage = prepare_guest_stage(root, &recipe)?;
    audit_rust_only_guest_lock(&stage.path)?;
    if fresh_target {
        reset_guest_target(&stage.path)?;
    }
    let game = stage.path.join("game");
    let mut command = Command::new(require_tool(&["cargo"])?);
    command
        .current_dir(&game)
        .args(["build", "--release", "--locked"]);
    if fresh_target {
        let cargo_home = prepare_shipping_cargo_home()?;
        reject_ambient_cargo_configs(&game, &cargo_home)?;
        reset_shipping_registry_sources(&cargo_home)?;
        command.env("CARGO_HOME", cargo_home);
    }
    if let Some(feature) = feature {
        command.args(["--features", feature]);
    }
    if let Some(map) = GUEST_LINK_MAP.get() {
        command.env("RUSTFLAGS", format!("-C link-arg=-Map={}", map.display()));
    }
    run(&mut command)?;
    verify_guest_stage(&stage.path, &recipe)?;
    let staged_exe = game_exe(&stage.path);
    audit_console_image(&staged_exe)?;
    let destination = game_exe(root);
    fs::create_dir_all(
        destination
            .parent()
            .ok_or_else(|| format!("PS1 executable has no parent: {}", destination.display()))?,
    )?;
    fs::copy(&staged_exe, &destination)?;
    audit_console_image(&destination)?;
    println!(
        "guest recipe: {} (canonical stage {})",
        recipe.sha256,
        stage.path.display()
    );
    Ok(())
}

fn stage_world_chunks(root: &Path, build: &Path) -> Result<PathBuf> {
    let stage = build.join("world-chunks");
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;
    fs::copy(root.join("id1psx/gfx.dat"), stage.join("chunk_1.dat"))?;
    fs::copy(
        root.join("id1psx/maps/episode.qidx"),
        stage.join("chunk_2.qidx"),
    )?;
    fs::copy(
        root.join("id1psx/sounds/global.qsb"),
        stage.join("chunk_3.qsb"),
    )?;
    fs::copy(
        root.join("id1psx/maps/start.psb"),
        stage.join("chunk_100.psb"),
    )?;
    for index in 1..=8 {
        fs::copy(
            root.join(format!("id1psx/maps/e1m{index}.psb")),
            stage.join(format!("chunk_{}.psb", 100 + index)),
        )?;
    }
    Ok(stage)
}

fn build_disc(
    root: &Path,
    build: &Path,
    feature: Option<&str>,
    fresh_guest_target: bool,
) -> Result<()> {
    if fresh_guest_target && feature.is_some() {
        return Err(
            "shipping provenance requires the release profile with no guest features".into(),
        );
    }
    if !cooked_assets_complete(root) {
        return Err("Episode 1 assets are not cooked".into());
    }
    validate_cooked_episode(root)?;
    fs::create_dir_all(build)?;
    build_game(root, feature, fresh_guest_target)?;
    let exe = build.join("quake-psx.exe");
    fs::copy(game_exe(root), &exe)?;
    let chunks = stage_world_chunks(root, build)?;
    let image = build.join("quake-psx.bin");
    if image.exists() {
        fs::remove_file(&image)?;
    }
    let cue = image.with_extension("cue");
    if cue.exists() {
        fs::remove_file(&cue)?;
    }

    let mut command = Command::new(require_tool(&["cargo"])?);
    command
        .current_dir(root)
        .args(["run", "--quiet", "--release", "--manifest-path"])
        .arg(root.join(".psoxide/tools/mkisopsx/Cargo.toml"))
        .arg("--")
        .arg("--exe")
        .arg(&exe)
        .arg("--out")
        .arg(&image)
        .args(["--volume", "QUAKE_PSX", "--world-pack-extra-dir"])
        .arg(&chunks);
    run(&mut command)?;
    for path in [&exe, &image, &cue] {
        if !path.is_file() {
            return Err(format!("missing build output {}", path.display()).into());
        }
    }
    println!("PSoXide disc: {}", cue.display());
    Ok(())
}

fn audit_console_image(exe: &Path) -> Result<()> {
    let data = fs::read(exe)?;
    if !data.starts_with(b"PS-X EXE") {
        return Err(format!("{} is not a PSX-EXE", exe.display()).into());
    }
    if data.windows(7).any(|w| w == b"/Users/") {
        return Err(format!("{} contains an absolute host path", exe.display()).into());
    }
    if data.len() > 1_300_000 {
        return Err(format!("{} exceeds 1.3 MiB", exe.display()).into());
    }
    Ok(())
}

fn package_dist(
    root: &Path,
    build: &Path,
    sdk: &PsoxideSource,
    pak: &Path,
    expected_inputs: &ShippingInputs,
) -> Result<()> {
    let current_inputs = capture_shipping_inputs(root, sdk, pak)?;
    if &current_inputs != expected_inputs {
        return Err("shipping inputs changed while the disc was built".into());
    }

    let dist = root.join("dist");
    fs::create_dir_all(&dist)?;
    let provenance = dist.join(PROVENANCE_FILE);
    invalidate_shipping_provenance(root)?;
    for name in ["quake-psx.bin", "quake-psx.cue", "quake-psx.exe"] {
        fs::copy(build.join(name), dist.join(name))?;
    }

    let final_inputs = capture_shipping_inputs(root, sdk, pak)?;
    if &final_inputs != expected_inputs {
        return Err("shipping inputs changed while the distribution was packaged".into());
    }
    let cue = artifact_provenance(&dist.join("quake-psx.cue"), "quake-psx.cue")?;
    let bin = artifact_provenance(&dist.join("quake-psx.bin"), "quake-psx.bin")?;
    let exe = artifact_provenance(&dist.join("quake-psx.exe"), "quake-psx.exe")?;
    write_shipping_provenance(&provenance, &final_inputs, &cue, &bin, &exe)?;
    println!("demo disc: {}", dist.join("quake-psx.cue").display());
    println!("shipping provenance: {}", provenance.display());
    Ok(())
}

fn invalidate_shipping_provenance(root: &Path) -> Result<()> {
    let provenance = root.join("dist").join(PROVENANCE_FILE);
    match fs::remove_file(&provenance) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not invalidate stale shipping provenance {}: {error}",
            provenance.display()
        )
        .into()),
    }
}

/// Require the pinned PSoXide revision to be reachable from PSoXide `main`.
///
/// `QUAKE_PSX_ALLOW_PSOXIDE_OFF_MAIN=1` permits an intentional local test
/// against an unpublished SDK revision.
fn verify_psoxide_rev_on_main() -> Result<()> {
    if env::var_os("QUAKE_PSX_ALLOW_PSOXIDE_OFF_MAIN").is_some() {
        println!("PSoXide pin {PSOXIDE_REV}: main-ancestry check SKIPPED by request");
        return Ok(());
    }
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/EBonura/PSoXide/compare/{PSOXIDE_REV}...main"),
            "--jq",
            ".status",
        ])
        .output()
        .map_err(|error| format!("cannot run gh to check the PSoXide pin against main: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot compare PSoXide pin {PSOXIDE_REV} with main via gh api ({}); set QUAKE_PSX_ALLOW_PSOXIDE_OFF_MAIN=1 only for a deliberate side-branch build",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match status.as_str() {
        "identical" | "ahead" => {
            println!("PSoXide pin {PSOXIDE_REV} is on main ({status})");
            Ok(())
        }
        other => Err(format!(
            "PSoXide pin {PSOXIDE_REV} is not on PSoXide main (compare status {other:?}); merge it before shipping, or set QUAKE_PSX_ALLOW_PSOXIDE_OFF_MAIN=1 for a deliberate side-branch build"
        )
        .into()),
    }
}

fn capture_shipping_inputs(root: &Path, sdk: &PsoxideSource, pak: &Path) -> Result<ShippingInputs> {
    reject_shipping_environment()?;
    let quake_revision = git_capture(root, &["rev-parse", "HEAD"])?;
    let quake_status = git_capture(root, &["status", "--porcelain", "--untracked-files=normal"])?;
    require_clean_revision("Quake source", &quake_revision, &quake_status, None)?;

    let (declared_revision, psoxide_source_kind, local_checkout) = declared_psoxide_contract(sdk)?;
    let psoxide_revision = match local_checkout {
        Some(path) => {
            let current_revision = git_capture(path, &["rev-parse", "HEAD"])?;
            let current_status =
                git_capture(path, &["status", "--porcelain", "--untracked-files=normal"])?;
            require_clean_revision(
                "PSoXide checkout",
                &current_revision,
                &current_status,
                Some(PSOXIDE_REV),
            )?;
            if current_revision != declared_revision {
                return Err("PSoXide checkout revision changed after hydration".into());
            }
            current_revision
        }
        None => declared_revision.to_string(),
    };

    let expected_stamp = sdk.describe();
    let actual_stamp = fs::read_to_string(root.join(".psoxide").join(HYDRATION_STAMP))
        .map_err(|_| "shipping provenance requires a verified PSoXide hydration stamp")?;
    require_exact_hydration_stamp(&expected_stamp, &actual_stamp)?;

    let pak0_sha256 = sha256_path(pak)?;
    require_sha256("shipping Quake 1.06 PAK0.PAK", &pak0_sha256, PAK0_SHA256)?;
    let pak0_bytes = fs::metadata(pak)?.len();
    let recipe = guest_recipe(root)?;

    Ok(ShippingInputs {
        quake_revision,
        psoxide_revision,
        psoxide_source_kind,
        pak0_sha256,
        pak0_bytes,
        guest_recipe_sha256: recipe.sha256,
        rust_toolchain_sha256: recipe.toolchain.rust_toolchain_sha256,
        rustc_version: recipe.toolchain.rustc_version,
        cargo_version: recipe.toolchain.cargo_version,
    })
}

fn reject_shipping_environment() -> Result<()> {
    let conflicts = shipping_environment_conflicts(env::vars_os());
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(format!(
        "shipping provenance rejects output-affecting Cargo or rustc environment variables: {}; unset them and retry",
        conflicts.join(", ")
    )
    .into())
}

fn shipping_environment_conflicts(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<String> {
    let mut conflicts = variables
        .into_iter()
        .filter_map(|(name, _)| {
            let name = name.to_string_lossy();
            shipping_environment_variable_is_unsafe(&name).then(|| name.to_string())
        })
        .collect::<Vec<_>>();
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

fn shipping_environment_variable_is_unsafe(name: &str) -> bool {
    matches!(
        name,
        "RUSTFLAGS"
            | "CARGO_ENCODED_RUSTFLAGS"
            | "RUSTC"
            | "RUSTC_BOOTSTRAP"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "CARGO_INCREMENTAL"
            | "PSOXIDE"
            | "QUAKE_PSX_RUST_DEBUG"
    ) || name.starts_with("CARGO_BUILD_")
        || name.starts_with("CARGO_PROFILE_")
        || name.starts_with("CARGO_TARGET_")
}

fn require_exact_hydration_stamp(expected: &str, actual: &str) -> Result<()> {
    if actual.trim() != expected {
        return Err(format!(
            "effective .psoxide hydration stamp drifted: expected {expected_stamp:?}, got {:?}",
            actual.trim(),
            expected_stamp = expected,
        )
        .into());
    }
    Ok(())
}

fn declared_psoxide_contract(sdk: &PsoxideSource) -> Result<(&str, &'static str, Option<&Path>)> {
    match sdk {
        PsoxideSource::Pinned { rev } => {
            require_clean_revision("PSoXide pin", rev, "", Some(PSOXIDE_REV))?;
            Ok((rev, "pinned_hydration", None))
        }
        PsoxideSource::LocalCheckout {
            path,
            revision,
            dirty_files,
        } => {
            if *dirty_files != 0 {
                return Err(format!(
                    "shipping provenance requires a clean PSoXide checkout, but hydration observed {dirty_files} changed files"
                )
                .into());
            }
            require_clean_revision(
                "PSoXide checkout at hydration",
                revision,
                "",
                Some(PSOXIDE_REV),
            )?;
            Ok((revision, "local_checkout", Some(path)))
        }
        PsoxideSource::FrontendBinary { .. } => Err(
            "shipping provenance rejects --psoxide frontend binaries; pass a clean checkout at the pinned revision"
                .into(),
        ),
    }
}

fn require_clean_revision(
    label: &str,
    revision: &str,
    porcelain_status: &str,
    expected: Option<&str>,
) -> Result<()> {
    if revision.len() != 40 || !revision.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("{label} has an invalid Git revision {revision:?}").into());
    }
    if let Some(expected) = expected {
        if revision != expected {
            return Err(
                format!("{label} revision drifted: expected {expected}, got {revision}").into(),
            );
        }
    }
    if !porcelain_status.trim().is_empty() {
        return Err(format!(
            "{label} is dirty; shipping provenance refuses changed or untracked source files:\n{}",
            porcelain_status.trim()
        )
        .into());
    }
    Ok(())
}

fn require_sha256(label: &str, actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        return Err(format!("{label} checksum mismatch: expected {expected}, got {actual}").into());
    }
    Ok(())
}

fn artifact_provenance(path: &Path, file: &'static str) -> Result<ArtifactProvenance> {
    if !path.is_file() {
        return Err(format!("shipping artifact is missing: {}", path.display()).into());
    }
    if path.file_name().and_then(|name| name.to_str()) != Some(file) {
        return Err(format!(
            "shipping artifact name does not match {file}: {}",
            path.display()
        )
        .into());
    }
    Ok(ArtifactProvenance {
        file,
        sha256: sha256_path(path)?,
        bytes: fs::metadata(path)?.len(),
    })
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", ch as u32).unwrap();
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn write_shipping_provenance(
    path: &Path,
    inputs: &ShippingInputs,
    cue: &ArtifactProvenance,
    bin: &ArtifactProvenance,
    exe: &ArtifactProvenance,
) -> Result<()> {
    let json = format!(
        "{{\n  \"schema\": 1,\n  \"quake_source\": {{\n    \"revision\": \"{}\",\n    \"tree_clean\": true\n  }},\n  \"psoxide\": {{\n    \"revision\": \"{}\",\n    \"tree_clean\": true,\n    \"source_kind\": \"{}\"\n  }},\n  \"shareware\": {{\n    \"pak0_sha256\": \"{}\",\n    \"pak0_bytes\": {}\n  }},\n  \"build\": {{\n    \"guest_stage_schema\": {},\n    \"guest_recipe_sha256\": \"{}\",\n    \"rust_toolchain_sha256\": \"{}\",\n    \"rustc_version\": \"{}\",\n    \"cargo_version\": \"{}\",\n    \"profile\": \"{}\",\n    \"features\": []\n  }},\n  \"artifacts\": {{\n    \"cue\": {{\"file\": \"{}\", \"sha256\": \"{}\", \"bytes\": {}}},\n    \"bin\": {{\"file\": \"{}\", \"sha256\": \"{}\", \"bytes\": {}}},\n    \"exe\": {{\"file\": \"{}\", \"sha256\": \"{}\", \"bytes\": {}}}\n  }}\n}}\n",
        inputs.quake_revision,
        inputs.psoxide_revision,
        inputs.psoxide_source_kind,
        inputs.pak0_sha256,
        inputs.pak0_bytes,
        GUEST_STAGE_SCHEMA,
        inputs.guest_recipe_sha256,
        inputs.rust_toolchain_sha256,
        json_escape(&inputs.rustc_version),
        json_escape(&inputs.cargo_version),
        SHIPPING_GUEST_PROFILE,
        cue.file,
        cue.sha256,
        cue.bytes,
        bin.file,
        bin.sha256,
        bin.bytes,
        exe.file,
        exe.sha256,
        exe.bytes,
    );
    let temporary = path.with_file_name(format!(".{PROVENANCE_FILE}.tmp"));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = output
        .write_all(json.as_bytes())
        .and_then(|_| output.sync_all())
    {
        drop(output);
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(output);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

/// Locate a replay frontend built from the selected PSoXide source.
///
/// Do not fall back to a sibling checkout: that could test a different SDK
/// revision from the one used to build the disc.
fn resolve_frontend(root: &Path, requested: Option<&Path>) -> Result<PathBuf> {
    let source = match requested {
        Some(path) => {
            if path.is_file() {
                return if is_runnable(path) {
                    Ok(path.to_path_buf())
                } else {
                    Err(format!(
                        "requested PSoXide frontend {} is not runnable",
                        path.display()
                    )
                    .into())
                };
            }
            path.to_path_buf()
        }
        None => root.join(".psoxide"),
    };
    frontend_from_checkout(&source)
}

/// Reuse a checkout's built frontend, or build one there. Never elsewhere.
fn frontend_from_checkout(source: &Path) -> Result<PathBuf> {
    let built = source.join("target/release/frontend");
    for candidate in [built.clone(), source.join("emu/target/release/frontend")] {
        if is_runnable(&candidate) {
            return Ok(candidate);
        }
    }
    // The frontend crate lives in the `emu` workspace in a full checkout and
    // at the root of the hydrated SDK tree; try both before failing.
    let mut last: Option<String> = None;
    for dir in [source.join("emu"), source.to_path_buf()] {
        if !dir.is_dir() {
            continue;
        }
        let mut command = Command::new(require_tool(&["cargo"])?);
        command
            .current_dir(&dir)
            .args(["build", "--release", "-p", "frontend"]);
        match run(&mut command) {
            Ok(()) => {
                if is_runnable(&built) {
                    return Ok(built);
                }
            }
            Err(error) => last = Some(error.to_string()),
        }
    }
    Err(format!(
        "PSoXide checkout {} supplied no runnable frontend and building one there failed{}",
        source.display(),
        last.map(|e| format!(": {e}")).unwrap_or_default()
    )
    .into())
}

fn run_regression(root: &Path, frontend: &Path, shipping: &Path, regression: &Path) -> Result<()> {
    let captures = root.join("captures");
    let shipping_capture = captures.join("shipping-smoke");
    let gameplay_capture = captures.join("episode1-regression");
    fs::create_dir_all(&shipping_capture)?;
    fs::create_dir_all(&gameplay_capture)?;

    let shipping_output = run_frontend(
        frontend,
        &shipping.join("quake-psx.cue"),
        &shipping_capture,
        850_000_000,
        false,
        None,
    )?;
    let shipping_log = combined_output(&shipping_output);
    fs::write(shipping_capture.join("console.log"), &shipping_log)?;
    let shipping_display = require_visible_display(&shipping_log, "shipping smoke")?;
    let shipping_polls = final_port1_polls(&shipping_capture.join("route.csv"))?;
    if shipping_polls == 0 {
        return Err("shipping smoke observed no controller polls".into());
    }

    let steps = env::var("QUAKE_PSX_REGRESSION_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8_000_000_000);
    let guest_frames = env::var("QUAKE_PSX_REGRESSION_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_500);
    let gameplay_output = run_frontend(
        frontend,
        &regression.join("quake-psx.cue"),
        &gameplay_capture,
        steps,
        true,
        Some(guest_frames),
    )?;
    let gameplay_log = combined_output(&gameplay_output);
    fs::write(gameplay_capture.join("console.log"), &gameplay_log)?;
    const MISSING_SOUND: &[u8] = b"Snd_FindSound: sound";
    if gameplay_log
        .windows(MISSING_SOUND.len())
        .any(|window| window == MISSING_SOUND)
    {
        return Err("Episode 1 requested a sound missing from its cooked SPU bank".into());
    }
    let gameplay_display = require_visible_display(&gameplay_log, "Episode 1 run")?;
    let gameplay_polls = final_port1_polls(&gameplay_capture.join("route.csv"))?;
    let probe = read_probe(&gameplay_capture.join("ram.bin"))?;
    validate_probe(&probe)?;
    if gameplay_polls == 0 {
        return Err("Episode 1 run observed no controller polls".into());
    }

    let summary = format!(
        "quake-psx PSoXide regression: PASS\n\
         shipping_display_fnv1a_64=0x{:016x}\n\
         shipping_port1_polls={shipping_polls}\n\
         episode1_display_fnv1a_64=0x{:016x}\n\
         episode1_port1_polls={gameplay_polls}\n\
         episode1_frames={}\n\
         episode1_map_loads={}\n\
         episode1_maps=0x{:03x}\n\
         episode1_transitions=0x{:03x}\n\
         episode1_weapons=0x{:02x}\n\
         episode1_weapon_acquisition=0x{:02x}\n\
         episode1_target_edges={}\n\
         episode1_monsters=0x{:03x}\n\
         episode1_monster_states={}/{}\n\
         episode1_boss=0x{:03x}\n\
         episode1_boss_shocks={}\n\
         episode1_health={}\n",
        shipping_display.hash,
        gameplay_display.hash,
        probe.total_frames,
        probe.map_loads,
        probe.maps_loaded,
        probe.transitions,
        probe.weapon_fired,
        probe.weapon_pickups,
        probe.target_edges,
        probe.monster_present,
        probe.valid_state_ranges,
        probe.state_ranges,
        probe.boss,
        probe.shock_count,
        probe.last_health,
    );
    fs::write(gameplay_capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_map_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/map-regression");
    fs::create_dir_all(&capture)?;
    let steps = env::var("QUAKE_PSX_MAP_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_000_000_000u64);
    // Re-pinned from 130 frames / 3e9 steps when the end-of-level panel landed
    // between maps: this probe crosses eleven authored transitions and each one
    // now holds the intermission camera for `INTERMISSION_TICKS`. The probe's
    // own frame count is unchanged at 698; the budget covers the panels.
    let guest_frames = env::var("QUAKE_PSX_MAP_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_200);
    let output = run_frontend(
        frontend,
        &regression.join("quake-psx.cue"),
        &capture,
        steps,
        true,
        Some(guest_frames),
    )?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    if log
        .windows(b"cd-sectors-dropped=".len())
        .any(|window| window == b"cd-sectors-dropped=")
    {
        return Err("map regression dropped CD sectors during bounded loading".into());
    }
    let route = fs::read_to_string(capture.join("route.csv"))?;
    let cd_commands = fs::read_to_string(capture.join("cd.csv"))?;
    let (initial_load_cycles, initial_read_sessions) = initial_load_metrics(&route, &cd_commands)?;
    const MAX_INITIAL_LOAD_CYCLES: u64 = 420_000_000;
    // Re-pinned from 66 after the original bubble sprite and the completed
    // ambient/secret-door sound set became resident. The added cooked payload
    // raised the measured cold-load high-water to 73 bounded ReadN sessions.
    // PSB5 Nodes now deliberately pause ReadN while their compact records are
    // expanded, then reopen at ClipNodes, adding exactly one bounded session
    // while retaining the independent cycle ceiling and dropped-sector check.
    const MAX_INITIAL_READ_SESSIONS: usize = 74;
    if initial_load_cycles > MAX_INITIAL_LOAD_CYCLES
        || initial_read_sessions > MAX_INITIAL_READ_SESSIONS
    {
        return Err(format!(
            "initial map load exceeded its bounded I/O budget: cycles={initial_load_cycles} \
             (max {MAX_INITIAL_LOAD_CYCLES}), ReadN sessions={initial_read_sessions} \
             (max {MAX_INITIAL_READ_SESSIONS})"
        )
        .into());
    }
    let probe = read_probe(&capture.join("ram.bin"))?;
    validate_map_probe(&probe)?;
    let display = require_visible_display(&log, "map regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("map regression observed no controller polls".into());
    }
    let summary = format!(
        "quake-psx Rust map regression: PASS\n\
         frames={}\n\
         map_loads={}\n\
         maps=0x{:03x}\n\
         validated=0x{:03x}\n\
         transitions=0x{:03x}\n\
         route_index={}\n\
         guest_train_leg_ticks={}\n\
         guest_train_travel_units={}\n\
         packet_arena_high_water_words={}\n\
         packet_arena_margin_words={}\n\
         packet_overflow_frames={}\n\
         initial_load_bus_cycles={}\n\
         initial_load_readn_sessions={}\n\
         display_fnv1a_64=0x{:016x}\n\
         port1_polls={}\n",
        probe.total_frames,
        probe.map_loads,
        probe.maps_loaded,
        probe.maps_validated,
        probe.transitions,
        probe.route_index,
        probe.state_ranges,
        probe.valid_state_ranges,
        probe.weapon_pickups,
        32_768u32 - probe.weapon_pickups,
        probe.target_edges,
        initial_load_cycles,
        initial_read_sessions,
        display.hash,
        polls,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_start_route_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/start-route-regression");
    fs::create_dir_all(&capture)?;
    let steps: u64 = env::var("QUAKE_PSX_START_ROUTE_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_START_ROUTE_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(regression.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--dump-ram")
        .arg(capture.join("ram.bin"))
        .arg("--guest-debug-log")
        .arg("--guest-frames")
        .arg(guest_frames.to_string())
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Start route emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    for marker in [
        b"quake-psx: loading frame presented".as_slice(),
        b"quake-psx: HUD graphical packets ready".as_slice(),
    ] {
        if !log.windows(marker.len()).any(|window| window == marker) {
            return Err(format!(
                "Start route regression lacks guest presentation marker {}",
                String::from_utf8_lossy(marker)
            )
            .into());
        }
    }
    let probe = read_probe_version(&capture.join("ram.bin"), 4)?;
    validate_start_route_probe(&probe)?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("Start route regression observed no controller polls".into());
    }
    let vram_hash = parse_frontend_hash(&log, "vram_fnv1a_64=")?;
    let display_hash = parse_frontend_hash(&log, "display_fnv1a_64=")?;
    let summary = format!(
        "quake-psx normal-mechanism Start route: PASS\n\
         frames={}\n\
         map_loads={}\n\
         maps=0x{:03x}\n\
         transitions=0x{:03x}\n\
         player_mechanisms=0x{:02x}\n\
         target_edges={}\n\
         port1_polls={}\n\
         vram_fnv1a_64=0x{vram_hash:016x}\n\
         display_fnv1a_64=0x{display_hash:016x}\n",
        probe.total_frames,
        probe.map_loads,
        probe.maps_loaded,
        probe.transitions,
        probe.player_state,
        probe.target_edges,
        polls,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

/// One `VMA LMA Size Align Name` row of a rust-lld link map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinkMapSymbol {
    address: u32,
    size: u32,
}

/// Resolve a link-map row by its exact, whitespace-normalised name column.
///
/// rust-lld emits two rows per symbol: the input-section row, whose name
/// column is the object file plus section, and the symbol row, whose name
/// column is the demangled symbol. Matching the whole name column exactly
/// therefore picks the symbol and can never pick its object file, and linker
/// script assignments such as `__heap_start = .` are addressable by the same
/// rule.
fn link_map_symbol(map: &str, name: &str) -> Result<LinkMapSymbol> {
    for line in map.lines() {
        let mut fields = line.split_whitespace();
        let (Some(address), Some(_lma), Some(size), Some(_align)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if fields.collect::<Vec<_>>().join(" ") != name {
            continue;
        }
        return Ok(LinkMapSymbol {
            address: u32::from_str_radix(address, 16)?,
            size: u32::from_str_radix(size, 16)?,
        });
    }
    Err(format!("link map has no symbol {name}").into())
}

/// Read a little-endian word out of a `--dump-ram` main-RAM image. KUSEG,
/// KSEG0 and KSEG1 all alias the same 2 MiB, so the segment bits are masked.
fn ram_word(ram: &[u8], address: u32) -> Result<u32> {
    let offset = (address & 0x001f_ffff) as usize;
    let word = ram
        .get(offset..offset + 4)
        .ok_or_else(|| format!("RAM dump is too short to hold 0x{address:08x}"))?;
    Ok(u32::from_le_bytes(word.try_into()?))
}

/// Read `prefix<value>` out of a frontend summary line.
///
/// The launch summary packs several of these onto one line, so this searches
/// within lines rather than only at their start.
fn frontend_field<'a>(log: &'a str, prefix: &str) -> Result<&'a str> {
    log.lines()
        .find_map(|line| line.split(prefix).nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(|| format!("frontend log lacks {prefix}<value>").into())
}

/// Boot the release image and check its main loop and remaining heap.
///
/// A live image continues polling the controller. The final PC distinguishes
/// that loop from `psx_rt::halt`; the link map locates the allocator cursor
/// used for the heap measurement.
fn run_ship_boot(root: &Path, frontend: &Path, build: &Path, map: &Path) -> Result<()> {
    let capture = root.join("captures/ship-boot");
    fs::create_dir_all(&capture)?;
    let steps: u64 = env::var("QUAKE_PSX_SHIP_BOOT_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SHIP_BOOT_STEPS);
    let ram = capture.join("ram.bin");
    let route = capture.join("route.csv");
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(build.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(&route)
        .arg("--dump-ram")
        .arg(&ram)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "shipping boot emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let text = String::from_utf8_lossy(&log);
    let route_ticks: u64 = frontend_field(&text, "route-ticks=")?.parse()?;
    let pc = u32::from_str_radix(frontend_field(&text, "pc=0x")?, 16)?;
    let polls = final_port1_polls(&route)?;

    let map_text = fs::read_to_string(map).map_err(|error| {
        format!(
            "shipping boot needs the guest link map at {}: {error}",
            map.display()
        )
    })?;
    let halt = link_map_symbol(&map_text, "psx_rt::halt")?;
    let heap_start = link_map_symbol(&map_text, "__heap_start = .")?.address;
    let cursor_address = link_map_symbol(&map_text, "psx_rt::heap::ALLOCATOR (.0)")?.address;
    let cursor = ram_word(&fs::read(&ram)?, cursor_address)?;
    if !(heap_start..=SHIP_BOOT_HEAP_END).contains(&cursor) {
        return Err(format!(
            "shipping boot read a bump cursor of 0x{cursor:08x}, outside the linked heap \
             0x{heap_start:08x}..=0x{SHIP_BOOT_HEAP_END:08x}; the link map and the RAM dump \
             disagree, so re-check SHIP_BOOT_HEAP_END against .psoxide/sdk/psoxide.ld"
        )
        .into());
    }
    let heap_total = SHIP_BOOT_HEAP_END - heap_start;
    let heap_used = cursor - heap_start;
    let heap_free = SHIP_BOOT_HEAP_END - cursor;

    let halted = (halt.address..halt.address.saturating_add(halt.size)).contains(&pc);
    let mut failures = Vec::new();
    if halted {
        failures.push(format!(
            "the image halted: final pc 0x{pc:08x} is inside psx_rt::halt \
             (0x{:08x}, {} bytes), so the guest stopped running before the route ended",
            halt.address, halt.size
        ));
    }
    if polls < SHIP_BOOT_MIN_PORT1_POLLS {
        failures.push(format!(
            "the image polled the pad {polls} times over {route_ticks} frames, under the \
             {SHIP_BOOT_MIN_PORT1_POLLS} a running main loop reaches"
        ));
    }
    if heap_free < SHIP_BOOT_MIN_HEAP_FREE {
        failures.push(format!(
            "the shipping heap has {heap_free} bytes left, under the \
             {SHIP_BOOT_MIN_HEAP_FREE}-byte floor; the next allocation increase makes the \
             boot fail outright"
        ));
    }

    let summary = format!(
        "quake-psx shipping boot: {}\n\
         route_ticks={route_ticks}\n\
         port1_polls={polls} (minimum {SHIP_BOOT_MIN_PORT1_POLLS})\n\
         final_pc=0x{pc:08x} ({})\n\
         psx_rt::halt=0x{:08x}+0x{:x}\n\
         heap_start=0x{heap_start:08x}\n\
         heap_end=0x{SHIP_BOOT_HEAP_END:08x}\n\
         heap_used={heap_used} of {heap_total} bytes\n\
         heap_free={heap_free} bytes (floor {SHIP_BOOT_MIN_HEAP_FREE})\n\
         link_map={}\n",
        if failures.is_empty() { "PASS" } else { "FAIL" },
        if halted { "halted" } else { "running" },
        halt.address,
        halt.size,
        map.display(),
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the shipping configuration does not boot:\n  {}",
        failures.join("\n  ")
    )
    .into())
}

#[derive(Debug, Eq, PartialEq)]
struct VisualCapture {
    probe: VisualProbe,
    display_hash: u64,
    world_hash: u64,
    hud_hash: u64,
}

fn run_visual_parity_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/visual-parity-regression");
    fs::create_dir_all(&capture)?;
    let first = run_visual_capture(frontend, regression, &capture.join("run-a"))?;
    let second = run_visual_capture(frontend, regression, &capture.join("run-b"))?;
    if first != second {
        return Err(format!(
            "visual parity capture is nondeterministic:\nrun-a={first:?}\nrun-b={second:?}"
        )
        .into());
    }
    if EXPECTED_VISUAL_WORLD_FNV1A64 != 0 && first.world_hash != EXPECTED_VISUAL_WORLD_FNV1A64 {
        return Err(format!(
            "E1M1 owner-camera world region drifted: 0x{:016x}, expected 0x{EXPECTED_VISUAL_WORLD_FNV1A64:016x}",
            first.world_hash,
        )
        .into());
    }
    if first.hud_hash != EXPECTED_VISUAL_HUD_FNV1A64 {
        return Err(format!(
            "E1M1 graphical HUD region drifted: 0x{:016x}, expected 0x{EXPECTED_VISUAL_HUD_FNV1A64:016x}",
            first.hud_hash,
        )
        .into());
    }
    let frames = u64::from(first.probe.frames.max(1));
    let summary = format!(
        "quake-psx E1M1 owner-camera visual parity: PASS\n\
         origin_q12=888798,3824884,-728959\n\
         angles=43,1088,0\n\
         frames={}\n\
         packets={}\n\
         hardware_triangles={}\n\
         mean_packets_x100={}\n\
         mean_hardware_triangles_x100={}\n\
         scoped_window_packets={}\n\
         scoped_window_resets={}\n\
         reset_failures={}\n\
         overflow_frames={}\n\
         view_model_packets={}\n\
         view_model_registered_packets={}\n\
         hud_packets={}\n\
         hud_registered_packets={}\n\
         crosshair_registered_packets={}\n\
         screen_registered_packets={}\n\
         world_region=0,0,320,184\n\
         world_rgb_fnv1a_64=0x{:016x}\n\
         hud_region=0,184,320,56\n\
         hud_rgb_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.frames,
        first.probe.packets,
        first.probe.hardware_triangles,
        u64::from(first.probe.packets) * 100 / frames,
        u64::from(first.probe.hardware_triangles) * 100 / frames,
        first.probe.windowed_packets,
        first.probe.window_resets,
        first.probe.reset_failures,
        first.probe.overflow_frames,
        first.probe.view_model_packets,
        first.probe.view_model_registered_packets,
        first.probe.hud_packets,
        first.probe.hud_registered_packets,
        first.probe.crosshair_registered_packets,
        first.probe.screen_registered_packets,
        first.world_hash,
        first.hud_hash,
        first.display_hash,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_visual_capture(frontend: &Path, regression: &Path, capture: &Path) -> Result<VisualCapture> {
    fs::create_dir_all(capture)?;
    let steps: u64 = env::var("QUAKE_PSX_VISUAL_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_VISUAL_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(180);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(regression.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--gpu-frame-stats-log")
        .arg(capture.join("gpu.csv"))
        .arg("--dump-display")
        .arg(capture.join("frame.ppm"))
        .arg("--dump-ram")
        .arg(capture.join("ram.bin"))
        .arg("--guest-debug-log")
        .arg("--guest-frames")
        .arg(guest_frames.to_string())
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "visual parity emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let display = require_visible_display(&log, "visual parity")?;
    let probe = read_visual_probe(&capture.join("ram.bin"))?;
    validate_visual_probe(&probe)?;
    let image = read_ppm(&capture.join("frame.ppm"))?;
    let world = crop_ppm(&image, VISUAL_WORLD_REGION)?;
    let hud = crop_ppm(&image, VISUAL_HUD_REGION)?;
    write_ppm(&capture.join("world.ppm"), &world)?;
    write_ppm(&capture.join("hud.ppm"), &hud)?;
    Ok(VisualCapture {
        probe,
        display_hash: display.hash,
        world_hash: fnv1a64(&world.rgb),
        hud_hash: fnv1a64(&hud.rgb),
    })
}

fn run_e1m1_chain_regression(
    root: &Path,
    frontend: &Path,
    regression: &Path,
    capture_name: &str,
) -> Result<()> {
    let capture = root.join("captures").join(capture_name);
    fs::create_dir_all(&capture)?;
    // Two runs of one binary agree exactly (the determinism check below is an
    // equality), so re-running never narrows this. The spread is a property of
    // the BUILD: where the linker happened to put the code decides how the hot
    // loops sit against a 4 KB I-cache, and byte-neutral edits move it. Five
    // functionally identical builds of one change measured 21156, 21168,
    // 21185, 21266 and 21278. A median of N would return the same number N
    // times, so the only honest fix is to say what counts as signal.
    let noise_note = if capture_name == "e1m1-chain-bench" {
        "fps_layout_noise_x1000=122 (spread across byte-identical builds of one \
change; a delta under this is code placement, not work)\n"
    } else {
        ""
    };
    let steps: u64 = env::var("QUAKE_PSX_E1M1_CHAIN_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_E1M1_CHAIN_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_800);
    let cue = regression.join("quake-psx.cue");
    let first = run_e1m1_chain_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_e1m1_chain_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "E1M1 per-map route is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let probe = first.probe;
    let polls = first.polls;
    let vram_hash = first.vram_hash;
    let display_hash = first.display_hash;
    let first_performance = full_level_render_metrics(
        &fs::read_to_string(capture.join("run-a/route.csv"))?,
        &fs::read_to_string(capture.join("run-a/cd.csv"))?,
    )?;
    let second_performance = full_level_render_metrics(
        &fs::read_to_string(capture.join("run-b/route.csv"))?,
        &fs::read_to_string(capture.join("run-b/cd.csv"))?,
    )?;
    if first_performance != second_performance {
        return Err(format!(
            "E1M1 full-level render timing is nondeterministic:\nrun-a={first_performance:#?}\nrun-b={second_performance:#?}"
        )
        .into());
    }
    let subdivision_cache_note = if capture_name.contains("subdivision-cache")
        || capture_name.contains("resident-stream")
        || capture_name.contains("resident-level2-stream")
        || capture_name.contains("resident-level2-scatter")
        || capture_name.contains("resident-level2-cold-cache")
        || capture_name.contains("resident-base-cache")
    {
        format!(
            "subdivision_cache_hits={}\n\
             subdivision_cache_allocations={}\n\
             subdivision_cache_replacements={}\n\
             subdivision_cache_fallbacks={}\n\
             subdivision_cache_initializations={}\n\
             subdivision_cache_packets={}\n",
            probe.monster_present,
            probe.monster_animated,
            probe.monster_state_bounds,
            probe.monster_attack,
            probe.monster_pain,
            probe.monster_death,
        )
    } else {
        String::new()
    };
    let summary = format!(
        "quake-psx canonical E1M1 per-map route: PASS\n\
         deterministic_runs=2\n\
         route=e1m1:info_player_start,t1_lift,t2_bridge,spiral_lamps,\
three_button_counter,t10_door,slipgate_corridor,t15_trigger,\
trigger_changelevel -> e1m2\n\
         frames={}\n\
         route_waypoints={}\n\
         player_mechanisms=0x{:04x}\n\
         mover_sounds=0x{:02x}\n\
         maps=0x{:03x}\n\
         transitions={}\n\
         target_edges={}\n\
         port1_polls={}\n\
         full_level_presentations={}\n\
         full_level_elapsed_bus_cycles={}\n\
         full_level_fps_x1000={}\n\
         topology_cache_batch_hits={}\n\
         topology_cache_batch_misses={}\n\
         topology_invariant_hit_slots={}\n\
         topology_invariant_miss_slots={}\n\
         indexed_projection_corners={}\n\
         indexed_projection_unique={}\n\
         {subdivision_cache_note}\
         {noise_note}\
         vram_fnv1a_64=0x{vram_hash:016x}\n\
         display_fnv1a_64=0x{display_hash:016x}\n",
        probe.total_frames,
        probe.route_index,
        probe.player_state,
        probe.weapon_selected,
        probe.maps_loaded,
        probe.transitions,
        probe.target_edges,
        polls,
        first_performance.presentations,
        first_performance.elapsed_bus_cycles,
        first_performance.fps_x1000,
        probe.monster_present,
        probe.monster_animated,
        probe.monster_state_bounds,
        probe.monster_attack,
        probe.monster_pain,
        probe.monster_death,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

/// One image-free run of the E1M1 per-map route. Two of them have to agree on
/// the complete probe, the controller poll count and both frame hashes.
fn run_e1m1_chain_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 9)?;
    validate_e1m1_chain_probe(&probe)?;
    let display = require_visible_display(&log, "E1M1 per-map route")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("E1M1 per-map route observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

/// Capture the frontend's direct GP0 frame counters for the complete fixed
/// E1M1 traversal. This is diagnostic evidence, not a timing build: the GPU
/// log is emulator-owned and does not modify guest RAM, but writing it adds
/// host work. `tools/analyze_psoxide_gpu.py` derives the gameplay window from
/// the same CD-session boundaries as the canonical FPS metric.
fn run_e1m1_gpu_census(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    run_e1m1_gpu_census_named(root, frontend, regression, "e1m1-gpu-census")
}

fn run_e1m1_gpu_census_named(
    root: &Path,
    frontend: &Path,
    regression: &Path,
    capture_name: &str,
) -> Result<()> {
    let capture = root.join("captures").join(capture_name);
    fs::create_dir_all(&capture)?;
    let steps: u64 = env::var("QUAKE_PSX_E1M1_CHAIN_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_E1M1_CHAIN_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_800);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(regression.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--cd-command-log")
        .arg(capture.join("cd.csv"))
        .arg("--gpu-frame-stats-log")
        .arg(capture.join("gpu.csv"))
        .arg("--dump-ram")
        .arg(capture.join("ram.bin"))
        .arg("--guest-debug-log")
        .arg("--guest-frames")
        .arg(guest_frames.to_string())
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "E1M1 GPU census failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 9)?;
    validate_e1m1_chain_probe(&probe)?;
    let display = require_visible_display(&log, "E1M1 GPU census")?;
    let vram_hash = parse_frontend_hash(&log, "vram_fnv1a_64=")?;
    let summary = format!(
        "quake-psx E1M1 GP0 census capture: PASS\n\
         frames={}\n\
         gpu_csv={}\n\
         vram_fnv1a_64=0x{vram_hash:016x}\n\
         display_fnv1a_64=0x{:016x}\n\
         analyze=python3 tools/analyze_psoxide_gpu.py {} {} {}\n",
        probe.total_frames,
        capture.join("gpu.csv").display(),
        display.hash,
        capture.join("gpu.csv").display(),
        capture.join("route.csv").display(),
        capture.join("cd.csv").display(),
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_e1m2_e1m3_route_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/e1m2-e1m3-route-regression");
    let steps: u64 = env::var("QUAKE_PSX_E1M2_E1M3_ROUTE_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_E1M2_E1M3_ROUTE_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(14_000);
    let cue = regression.join("quake-psx.cue");
    let first =
        run_e1m2_e1m3_route_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second =
        run_e1m2_e1m3_route_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "E1M2/E1M3 per-map route is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let probe = first.probe;
    let summary = format!(
        "quake-psx canonical E1M2/E1M3 per-map route: PASS\n\
         deterministic_runs=2\n\
         route=e1m2:spawn,lift192,shootable243,target77,gate52,button71,key42,target68,silver_door,floorplate80,trigger154,door152,changelevel155 -> e1m3:gold_key104,lift87,button303,stairs24-28,gold_doors37-38,secret_doors54-55,train53,trapdoor60,gate66,button115,end_button14,end_lift4-5,changelevel237 -> e1m4\n\
         frames={}\n\
         mechanisms=0x{:08x}\n\
         maps=0x{:03x}\n\
         transitions={}\n\
         weapon_shots={}\n\
         target_edges={}\n\
         port1_polls={}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        probe.total_frames,
        probe.player_state,
        probe.maps_loaded,
        probe.transitions,
        probe.weapon_fired,
        probe.target_edges,
        first.polls,
        first.vram_hash,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_e1m2_e1m3_route_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 14)?;
    validate_e1m2_route_probe(&probe)?;
    let display = require_visible_display(&log, "E1M2/E1M3 per-map route")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("E1M2/E1M3 per-map route observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn run_systems_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/systems-regression");
    fs::create_dir_all(&capture)?;
    let steps: u64 = env::var("QUAKE_PSX_SYSTEMS_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_SYSTEMS_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(regression.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--dump-ram")
        .arg(capture.join("ram.bin"))
        .arg("--guest-debug-log")
        .arg("--guest-frames")
        .arg(guest_frames.to_string())
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "systems emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 8)?;
    validate_systems_probe(&probe)?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("systems regression observed no controller polls".into());
    }
    let vram_hash = parse_frontend_hash(&log, "vram_fnv1a_64=")?;
    let display_hash = parse_frontend_hash(&log, "display_fnv1a_64=")?;
    let summary = format!(
        "quake-psx authored entity systems: PASS\n\
         frames={}\n\
         maps=0x{:03x}\n\
         secrets_total_asserted=0\n\
         fireballs_launched={}\n\
         fireball_impacts={}\n\
         target_edges={}\n\
         port1_polls={}\n\
         vram_fnv1a_64=0x{vram_hash:016x}\n\
         display_fnv1a_64=0x{display_hash:016x}\n",
        probe.total_frames,
        probe.maps_loaded,
        probe.intermission_state,
        probe.boss,
        probe.target_edges,
        polls,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

/// The survival route depends on three pieces of authored E1M1 data. Assert
/// them host-side against the cooked lumps before the guest claims them, the
/// same shape as the ambient probe's authored-source check.
fn assert_authored_survival_sources(root: &Path) -> Result<()> {
    const SLIME_CONTENTS: i16 = -4;
    const WATER_CONTENTS: i16 = -3;
    const ARTIFACT_SUPER_DAMAGE: u8 = 0x20;
    const SPAWNFLAG_NOT_EASY: u16 = 256;
    let path = root.join("id1psx/maps/e1m1.psb");
    let bytes = fs::read(&path)?;
    let mut reader = SliceReader::new(&bytes);
    let index = PsbIndex::read(&mut reader).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {error}", path.display()),
        )
    })?;
    let leaves = RecordSlice::<Leaf>::new(cooked_lump(&bytes, &index, LumpKind::Leaves))
        .ok_or_else(|| format!("{} has malformed leaves", path.display()))?;
    let slime = leaves
        .iter()
        .filter(|leaf| leaf.contents == SLIME_CONTENTS)
        .count();
    let water = leaves
        .iter()
        .filter(|leaf| leaf.contents == WATER_CONTENTS)
        .count();
    if slime == 0 || water == 0 {
        return Err(format!(
            "cooked E1M1 must author both hazards for the survival route: slime={slime} water={water}"
        )
        .into());
    }
    let entities = RecordSlice::<MapEntity>::new(cooked_lump(&bytes, &index, LumpKind::Entities))
        .ok_or_else(|| format!("{} has malformed entities", path.display()))?;
    let artifacts = entities
        .iter()
        .filter(|entity| {
            entity.class_name == ARTIFACT_SUPER_DAMAGE
                && entity.spawn_flags & SPAWNFLAG_NOT_EASY == 0
        })
        .count();
    if artifacts != 1 {
        return Err(format!(
            "cooked E1M1 must author exactly one easy item_artifact_super_damage, found {artifacts}"
        )
        .into());
    }
    println!(
        "authored E1M1 survival sources: slime_leaves={slime} water_leaves={water} easy_quad={artifacts}"
    );
    Ok(())
}

fn run_survival_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/survival-regression");
    let steps: u64 = env::var("QUAKE_PSX_SURVIVAL_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24_000_000_000);
    let guest_frames: u64 = env::var("QUAKE_PSX_SURVIVAL_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(9_200);
    let cue = regression.join("quake-psx.cue");
    let first = run_survival_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_survival_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "survival regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let summary = format!(
        "quake-psx authored E1M1 survival regression: PASS\n\
         deterministic_runs=2\n\
         frames={}\n\
         map_loads={}\n\
         route_waypoints={}\n\
         mechanisms=0x{:04x}\n\
         hazard_damage={}\n\
         fall_damage={}\n\
         drown_damage={}\n\
         deaths={}\n\
         respawns={}\n\
         respawn_health={} respawn_shells={}\n\
         quad_seconds_armed={} quad_seconds_half={}\n\
         water_levels=0x{:02x} water_types=0x{:02x}\n\
         authored_hazard_leaf={} authored_quad_entity={}\n\
         motor_health=0x{:08x}\n\
         map_residency_misses=1\n\
         map_residency_hits=1\n\
         same_map_respawn_readn_sessions=0\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.map_loads,
        first.probe.route_index,
        first.probe.player_state,
        first.probe.weapon_selected,
        first.probe.weapon_fired,
        first.probe.weapon_animated,
        first.probe.monster_attack,
        first.probe.monster_pain,
        first.probe.monster_death,
        first.probe.boss,
        first.probe.monster_present,
        first.probe.monster_animated,
        first.probe.shock_count,
        first.probe.intermission_state,
        first.probe.monster_state_bounds,
        first.probe.transitions,
        first.probe.target_edges,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn validate_systems_probe(probe: &Probe) -> Result<()> {
    if probe.version != 8 {
        return Err(format!("unsupported systems probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "authored entity systems failed: code={} map={} detail=0x{:08x} phase={} frames={} stage_frames={} launched={} impacts={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
            probe.stage_frames,
            probe.intermission_state,
            probe.boss,
        )
        .into());
    }
    if probe.complete != 1
        || probe.phase != 0x81
        || probe.maps_loaded != 0x001
        || probe.maps_validated != 0x001
        || probe.map_loads != 1
        || probe.route_index != 1
        || probe.current_map != 0
        || probe.intermission_state < 3
        || probe.boss == 0
    {
        return Err(format!(
            "authored entity systems incomplete: complete={} phase={} maps=0x{:03x} validated=0x{:03x} loads={} stage={} map={} launched={} impacts={}",
            probe.complete,
            probe.phase,
            probe.maps_loaded,
            probe.maps_validated,
            probe.map_loads,
            probe.route_index,
            probe.current_map,
            probe.intermission_state,
            probe.boss,
        )
        .into());
    }
    Ok(())
}

fn run_survival_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    const MISSING_SOUND: &[u8] = b"Snd_FindSound: sound";
    if log
        .windows(MISSING_SOUND.len())
        .any(|window| window == MISSING_SOUND)
    {
        return Err("survival regression requested a sound missing from E1M1's cooked bank".into());
    }
    const MAP_HIT: &[u8] = b"quake-psx: map residency hit";
    const MAP_MISS: &[u8] = b"quake-psx: map residency miss loaded";
    let hits = log
        .windows(MAP_HIT.len())
        .filter(|line| *line == MAP_HIT)
        .count();
    let misses = log
        .windows(MAP_MISS.len())
        .filter(|line| *line == MAP_MISS)
        .count();
    let probe = read_probe_version(&capture.join("ram.bin"), 9)?;
    validate_survival_probe(&probe)?;
    let expected_hits = probe.monster_pain as usize;
    if hits != expected_hits || misses != 1 {
        return Err(format!(
            "survival residency path drifted: hits={hits}, misses={misses}, expected {expected_hits}/1 from the validated respawn count"
        )
        .into());
    }
    let display = require_visible_display(&log, "survival regression")?;
    let route = fs::read_to_string(capture.join("route.csv"))?;
    let cd_commands = fs::read_to_string(capture.join("cd.csv"))?;
    // Controller polling begins while the loading screen is still live, so a
    // changed display row with a poll is not a sound residency boundary.  The
    // cold local-bank telemetry is emitted only after the initial map, model,
    // and local-audio publication has finished.  Anything read after that
    // marker belongs to gameplay or a same-map reset and must be resident.
    let telemetry = String::from_utf8_lossy(&log);
    let publication_cycle = cold_local_audio_publication_cycle(&telemetry)?;
    require_display_flip_after_cycle(&route, publication_cycle)?;
    let later_reads = readn_sessions_after_cycle(&cd_commands, publication_cycle)?;
    if later_reads != 0 {
        return Err(format!(
            "same-map survival respawn performed {later_reads} ReadN sessions after cold level publication"
        )
        .into());
    }
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("survival regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

/// Survival fields stored in the shared 136-byte gameplay probe.
const SURVIVAL_HAZARD_DAMAGE: u32 = 1 << 0;
const SURVIVAL_FALL_DAMAGE: u32 = 1 << 1;
const SURVIVAL_DROWN_DAMAGE: u32 = 1 << 2;
const SURVIVAL_HAZARD_DEATH: u32 = 1 << 3;
const SURVIVAL_RESPAWN_LOADOUT: u32 = 1 << 4;
const SURVIVAL_POWERUP_TAKEN: u32 = 1 << 5;
const SURVIVAL_POWERUP_HALF_SPENT: u32 = 1 << 6;
const SURVIVAL_POWERUP_EXPIRED: u32 = 1 << 7;
const SURVIVAL_REQUIRED: u32 = SURVIVAL_HAZARD_DAMAGE
    | SURVIVAL_FALL_DAMAGE
    | SURVIVAL_DROWN_DAMAGE
    | SURVIVAL_HAZARD_DEATH
    | SURVIVAL_RESPAWN_LOADOUT
    | SURVIVAL_POWERUP_TAKEN
    | SURVIVAL_POWERUP_HALF_SPENT
    | SURVIVAL_POWERUP_EXPIRED;

fn validate_survival_probe(probe: &Probe) -> Result<()> {
    const E1M1_BIT: u32 = 1 << 1;
    if probe.version != 9 {
        return Err(format!("unsupported survival probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "survival regression failed: code={} stage={} detail=0x{:08x} phase={} frames={} pos=({},{},{})",
            probe.failure_code,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    if probe.complete != 1 || probe.phase != 0x59 {
        return Err(format!(
            "survival route incomplete: complete={} phase={} waypoint={} stage={} mechanisms=0x{:04x}",
            probe.complete, probe.phase, probe.route_index, probe.stage_frames, probe.player_state,
        )
        .into());
    }
    if probe.maps_loaded != E1M1_BIT || probe.current_map != 1 {
        return Err(format!(
            "survival regression left E1M1: maps=0x{:03x} current={}",
            probe.maps_loaded, probe.current_map,
        )
        .into());
    }
    if probe.player_state & SURVIVAL_REQUIRED != SURVIVAL_REQUIRED {
        return Err(format!(
            "survival mechanisms incomplete: 0x{:04x}, expected 0x{SURVIVAL_REQUIRED:04x}",
            probe.player_state,
        )
        .into());
    }
    for (label, actual) in [
        ("hazard damage", probe.weapon_selected),
        ("fall damage", probe.weapon_fired),
        ("drowning damage", probe.weapon_animated),
        ("deaths", probe.monster_attack),
        ("respawns", probe.monster_pain),
    ] {
        if actual == 0 {
            return Err(format!("survival regression recorded no {label}").into());
        }
    }
    // The respawn loadout is SetNewParms exactly.
    if probe.monster_death != 100 || probe.boss != 25 {
        return Err(format!(
            "respawn loadout drifted: health={} shells={}, expected 100/25",
            probe.monster_death, probe.boss,
        )
        .into());
    }
    // A hard landing is worth the original's flat five points every time.
    if probe.weapon_fired % 5 != 0 {
        return Err(format!(
            "fall damage {} is not a multiple of the original's five points",
            probe.weapon_fired,
        )
        .into());
    }
    // The quad arms at thirty seconds and is observed inside its second half.
    if probe.monster_present != 30 || probe.monster_animated == 0 || probe.monster_animated > 15 {
        return Err(format!(
            "quad timer drifted: armed={} half={}",
            probe.monster_present, probe.monster_animated,
        )
        .into());
    }
    // The route must have stood in both authored liquids at full submersion.
    if probe.shock_count & (1 << 3) == 0 {
        return Err(format!(
            "survival route never submerged: water levels 0x{:02x}",
            probe.shock_count,
        )
        .into());
    }
    if probe.intermission_state & 0x03 != 0x03 {
        return Err(format!(
            "survival route missed an authored liquid: types 0x{:02x}",
            probe.intermission_state,
        )
        .into());
    }
    if probe.monster_state_bounds == u32::MAX || probe.transitions == u32::MAX {
        return Err("survival regression never resolved its authored sources".into());
    }
    // A shipping map must never stall the player motor. The low byte is the
    // `MovementStalls` set the motor had to assume; the rest is the longest
    // run of frames where the motor did not run at all.
    if probe.target_edges != 0 {
        return Err(format!(
            "player motor was not healthy: stalls=0x{:02x} stalled_frames={}",
            probe.target_edges & 0xff,
            probe.target_edges >> 8,
        )
        .into());
    }
    Ok(())
}

fn run_combat_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    // This checkpoint now carries the authored explobox and trigger-multiple
    // setup. Both additions are independently probe-pinned below. The final
    // MIPS layout moved the absolute frame timing, so the replacement image
    // was inspected and is still required to match twice.
    // Previous pin: 0x6e666cc1fb6deb43/0x1f3df31c89a63bbf.
    const EXPECTED_VRAM_FNV1A64: u64 = 0x4aba_9996_901d_f75c;
    const EXPECTED_DISPLAY_FNV1A64: u64 = 0xf5c0_fea0_4b3a_324e;
    let capture = root.join("captures/combat-regression");
    let steps = env::var("QUAKE_PSX_COMBAT_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000_000_000);
    let guest_frames = env::var("QUAKE_PSX_COMBAT_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);
    let cue = regression.join("quake-psx.cue");
    let first = run_combat_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_combat_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "combat regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    if first.vram_hash != EXPECTED_VRAM_FNV1A64 || first.display_hash != EXPECTED_DISPLAY_FNV1A64 {
        return Err(format!(
            "combat visual checkpoint drifted: vram=0x{:016x} display=0x{:016x} (expected 0x{EXPECTED_VRAM_FNV1A64:016x}/0x{EXPECTED_DISPLAY_FNV1A64:016x})",
            first.vram_hash, first.display_hash,
        )
        .into());
    }
    let summary = format!(
        "quake-psx Rust combat regression: PASS\n\
         deterministic_runs=2\n\
         frames={}\n\
         map_loads={}\n\
         maps=0x{:03x}\n\
         weapon_selected=0x{:02x}\n\
         weapon_fired=0x{:02x}\n\
         weapon_animated=0x{:02x}\n\
         monster_present=0x{:03x}\n\
         monster_pain=0x{:03x}\n\
         monster_death=0x{:03x}\n\
         splash_trigger_source={}\n\
         solid_explobox_source={}\n\
         final_health={}\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.map_loads,
        first.probe.maps_loaded,
        first.probe.weapon_selected,
        first.probe.weapon_fired,
        first.probe.weapon_animated,
        first.probe.monster_present,
        first.probe.monster_pain,
        first.probe.monster_death,
        first.probe.transitions,
        first.probe.boss,
        first.probe.last_health,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_monster_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/monster-regression");
    let steps = env::var("QUAKE_PSX_MONSTER_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000_000_000);
    let guest_frames = env::var("QUAKE_PSX_MONSTER_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_300);
    let cue = regression.join("quake-psx.cue");
    let first = run_monster_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_monster_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "monster regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let summary = format!(
        "quake-psx Rust E1M1 Soldier and Dog regression: PASS\n\
         deterministic_runs=2\n\
         frames={}\n\
         map_loads={}\n\
         authored_sources=21,82,115,122,124\n\
         monster_present=0x{:02x}\n\
         player_acquisition=0x{:02x}\n\
         world_mover_motion=0x{:02x}\n\
         player_damage=0x{:02x}\n\
         player_death={}\n\
         monster_pain={}\n\
         monster_death={}\n\
         monster_gib={}\n\
         state_ranges={}/{}\n\
         final_player_health={}\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.map_loads,
        first.probe.monster_present,
        first.probe.target_edges,
        first.probe.monster_animated,
        first.probe.monster_attack,
        first.probe.player_state,
        first.probe.monster_pain,
        first.probe.monster_death,
        first.probe.boss,
        first.probe.valid_state_ranges,
        first.probe.state_ranges,
        first.probe.last_health,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_bestiary_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/bestiary-regression");
    let steps = env::var("QUAKE_PSX_BESTIARY_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12_000_000_000u64);
    let guest_frames = env::var("QUAKE_PSX_BESTIARY_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5_500u64);
    let cue = regression.join("quake-psx.cue");
    let first = run_bestiary_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_bestiary_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "bestiary regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let summary = format!(
        "quake-psx Rust authored bestiary regression: PASS\n\
         deterministic_runs=2\n\
         stages=e1m2:monster_ogre,e1m4:monster_knight\n\
         frames={}\n\
         last_authored_source={}\n\
         last_authored_candidate={}\n\
         stages_proved=0x{:02x}\n\
         stages_required=0x{:02x}\n\
         monster_attack=0x{:02x}\n\
         monster_pain=0x{:02x}\n\
         monster_death=0x{:02x}\n\
         body_block_source={}\n\
         final_player_health={}\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.failure_entity,
        first.probe.route_index,
        first.probe.player_state,
        first.probe.valid_state_ranges,
        first.probe.monster_attack,
        first.probe.monster_pain,
        first.probe.monster_death,
        first.probe.target_edges,
        first.probe.last_health,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_monsterjump_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/monsterjump-regression");
    let steps = env::var("QUAKE_PSX_MONSTERJUMP_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000_000_000u64);
    let guest_frames = env::var("QUAKE_PSX_MONSTERJUMP_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60u64);
    let cue = regression.join("quake-psx.cue");
    let first = run_monsterjump_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_monsterjump_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "monster-jump regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let summary = format!(
        "quake-psx Rust E1M6 monster-jump regression: PASS\n\
         deterministic_runs=2\n\
         authored_trigger_source=192\n\
         authored_ogre_source={}\n\
         frames={}\n\
         evidence=0x{:02x}\n\
         required=0x{:02x}\n\
         final_z={}\n\
         final_frame={}\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.route_index,
        first.probe.total_frames,
        first.probe.monster_animated,
        first.probe.valid_state_ranges,
        first.probe.monster_state_bounds as i32,
        first.probe.state_ranges,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_monsterjump_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 13)?;
    validate_monsterjump_probe(&probe)?;
    let display = require_visible_display(&log, "monster-jump regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("monster-jump regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn validate_monsterjump_probe(probe: &Probe) -> Result<()> {
    if probe.version != 13 {
        return Err(format!("unsupported monster-jump probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "monster-jump regression failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
        )
        .into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, 1 << 6),
        ("maps validated", probe.maps_validated, 1 << 6),
        ("authored ogre present", probe.monster_present, 1),
        ("authored ogre source", probe.route_index, 81),
        ("authored trigger source", probe.transitions, 192),
        ("flight evidence", probe.monster_animated, 0x07),
        ("required evidence", probe.valid_state_ranges, 0x07),
        ("completed evidence", probe.player_state, 0x07),
    ] {
        if actual != expected {
            return Err(format!(
                "monster-jump regression {label} = 0x{actual:08x}, expected 0x{expected:08x}"
            )
            .into());
        }
    }
    if probe.complete != 1 || probe.phase != 0xb1 || probe.current_map != 6 || probe.map_loads != 1
    {
        return Err(format!(
            "monster-jump regression did not complete: phase={} complete={} map={} loads={}",
            probe.phase, probe.complete, probe.current_map, probe.map_loads,
        )
        .into());
    }
    Ok(())
}

fn run_bestiary_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe(&capture.join("ram.bin"))?;
    validate_bestiary_probe(&probe)?;
    let display = require_visible_display(&log, "bestiary regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("bestiary regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn validate_bestiary_probe(probe: &Probe) -> Result<()> {
    // One bit per authored stage: E1M2 ogre, E1M4 knight.
    const STAGE_MAPS: u32 = (1 << 2) | (1 << 4);
    const STAGES: u32 = 0b11;
    if probe.version != 10 {
        return Err(format!("unsupported bestiary probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "bestiary regression failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={} player=({},{}) distance={} target=({},{}) candidate={} detour={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.stage_frames,
            probe.transitions as i32,
            probe.weapon_selected as i32,
            probe.weapon_fired,
            probe.monster_state_bounds as i32,
            probe.intermission_state as i32,
            probe.boss,
            probe.shock_count,
        )
        .into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, STAGE_MAPS),
        ("maps validated", probe.maps_validated, STAGE_MAPS),
        ("authored monsters present", probe.monster_present, STAGES),
        ("monsters attacked", probe.monster_attack, STAGES),
        ("monsters took pain", probe.monster_pain, STAGES),
        ("monsters died", probe.monster_death, STAGES),
        ("stage contracts", probe.player_state, STAGES),
        ("validated contracts", probe.valid_state_ranges, STAGES),
    ] {
        if actual != expected {
            return Err(format!(
                "bestiary regression {label} = 0x{actual:08x}, expected 0x{expected:08x}"
            )
            .into());
        }
    }
    if probe.target_edges == u32::from(u16::MAX) {
        return Err("bestiary regression never observed a monster body blocking the player".into());
    }
    if probe.complete != 1 || probe.phase != 0xaf {
        return Err(format!(
            "bestiary regression did not complete: phase={} complete={}",
            probe.phase, probe.complete
        )
        .into());
    }
    Ok(())
}

/// Test E1M7's Chthon sequence and the resulting episode-completion state.
///
/// This starts directly in E1M7; it is not a complete episode playthrough.
fn run_episode1_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/episode1-regression");
    let steps = env::var("QUAKE_PSX_EPISODE1_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24_000_000_000u64);
    let guest_frames = env::var("QUAKE_PSX_EPISODE1_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000u64);
    let cue = regression.join("quake-psx.cue");
    let first = run_episode1_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_episode1_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "episode 1 regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let summary = format!(
        "quake-psx Rust Episode 1 completion regression: PASS\n\
         deterministic_runs=2\n\
         route=e1m7:item_sigil,monster_boss,arena,func_plat,event_lightning,\
lava_bridge,shaft,trigger_changelevel -> start\n\
         frames={}\n\
         map_loads={}\n\
         maps=0x{:03x}\n\
         transitions={}\n\
         episode_state=0x{:04x}\n\
         boss_frame={}\n\
         boss_active={}\n\
         boss_throw_frames={}\n\
         intermission_counters=0x{:08x}\n\
         final_health={}\n\
         target_edges={}\n\
         port1_polls={}\n\
         display={}x{}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.map_loads,
        first.probe.maps_loaded,
        first.probe.transitions,
        first.probe.player_state,
        first.probe.boss,
        first.probe.monster_present,
        first.probe.shock_count,
        first.probe.intermission_state,
        first.probe.last_health,
        first.probe.target_edges,
        first.polls,
        first.display_width,
        first.display_height,
        first.vram_hash,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_episode1_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let probe = read_probe_version(&capture.join("ram.bin"), 13)?;
    validate_episode1_probe(&probe)?;
    let display = require_visible_display(&log, "episode 1 regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("episode 1 regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

/// Named episode-completion bits used in failure messages.
const EPISODE1_STATE: [(&str, u32); 18] = [
    ("item_sigil taken", 1 << 0),
    ("sigil target woke monster_boss", 1 << 1),
    ("Chthon rose out of the lava", 1 << 2),
    ("Chthon threw", 1 << 3),
    ("arena walked on foot to the map's own lift", 1 << 4),
    ("authored trigger_changelevel fired", 1 << 5),
    ("intermission reported the episode finished", 1 << 6),
    ("the rune survived the map load", 1 << 7),
    ("Start spawned on info_player_start2", 1 << 8),
    ("the rune-1 func_episodegate is solid", 1 << 9),
    ("func_bossgate is still shut", 1 << 10),
    (
        "the func_plat carried the player to the button ring",
        1 << 11,
    ),
    ("event_lightning delivered a shock", 1 << 12),
    ("Chthon died to the authored shock chain", 1 << 13),
    ("walked onto the lava bridge his death closed", 1 << 14),
    (
        "fell the shaft his death opened, into the exit chamber",
        1 << 15,
    ),
    ("E1M7 changelevel fired its authored targets", 1 << 17),
    ("began at E1M7's authored player spawn", 1 << 16),
];

fn validate_episode1_probe(probe: &Probe) -> Result<()> {
    // E1M7 then Start.
    const MAPS: u32 = (1 << 7) | 1;
    if probe.version != 13 {
        return Err(format!("unsupported episode 1 probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "episode 1 regression failed: code={} map={} detail=0x{:08x} phase={} \
             stage_frames={} player=({},{},{}) mover_activations={} state=0x{:04x}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_detail,
            probe.phase,
            probe.stage_frames,
            probe.state_ranges as i32,
            probe.monster_state_bounds as i32,
            probe.weapon_selected as i32,
            probe.weapon_fired,
            probe.player_state,
        )
        .into());
    }
    if probe.maps_loaded != MAPS {
        return Err(format!(
            "episode 1 regression loaded 0x{:03x}, expected 0x{MAPS:03x}",
            probe.maps_loaded
        )
        .into());
    }
    for (label, bit) in EPISODE1_STATE {
        if probe.player_state & bit == 0 {
            return Err(format!("episode 1 regression never proved: {label}").into());
        }
    }
    if probe.transitions != 1 {
        return Err(format!(
            "episode 1 regression saw {} intermissions, expected exactly one",
            probe.transitions
        )
        .into());
    }
    if probe.last_health == 0 {
        return Err("episode 1 regression finished with the player dead".into());
    }
    if probe.complete != 1 || probe.phase != 0xe1 {
        return Err(format!(
            "episode 1 regression did not complete: phase={} complete={}",
            probe.phase, probe.complete
        )
        .into());
    }
    Ok(())
}

fn run_arsenal_regression(root: &Path, frontend: &Path, regression: &Path) -> Result<()> {
    let capture = root.join("captures/arsenal-regression");
    let steps = env::var("QUAKE_PSX_ARSENAL_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000_000_000);
    let guest_frames = env::var("QUAKE_PSX_ARSENAL_REGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(210);
    let cue = regression.join("quake-psx.cue");
    let first = run_arsenal_once(frontend, &cue, &capture.join("run-a"), steps, guest_frames)?;
    let second = run_arsenal_once(frontend, &cue, &capture.join("run-b"), steps, guest_frames)?;
    if first != second {
        return Err(format!(
            "arsenal regression is nondeterministic:\nrun-a={first:#?}\nrun-b={second:#?}"
        )
        .into());
    }
    let self_damage = first.probe.player_state >> 16;
    let player_health = first.probe.player_state & 0xffff;
    let summary = format!(
        "quake-psx Rust arsenal regression: PASS\n\
         deterministic_runs=2\n\
         frames={}\n\
         route=E1M1-E1M5\n\
         weapon_pickup=0x{:02x}\n\
         weapon_selected=0x{:02x}\n\
         weapon_fired=0x{:02x}\n\
         weapon_animated=0x{:02x}\n\
         projectile_models=0x{:02x}\n\
         projectile_alias_packets={}\n\
         rocket_model=0x{:02x}\n\
         rocket_alias_packets={}\n\
         rocket_impacts={}\n\
         explosion_flash={}\n\
         rocket_self_damage={}\n\
         player_health={}\n\
         target_health={}\n\
         nail_pool_capacity={}\n\
         nail_pool_denial={}\n\
         lightning_wall_trace=0x{:02x}\n\
         nail_runtime=0x{:02x}\n\
         grenade_runtime=0x{:02x}\n\
         vram_fnv1a_64=0x{:016x}\n\
         display={}x{}\n\
         display_fnv1a_64=0x{:016x}\n",
        first.probe.total_frames,
        first.probe.weapon_pickups,
        first.probe.weapon_selected,
        first.probe.weapon_fired,
        first.probe.weapon_animated,
        first.probe.shock_count,
        first.probe.intermission_state,
        first.probe.state_ranges,
        first.probe.valid_state_ranges,
        first.probe.target_edges & 0xffff,
        (first.probe.target_edges >> 31) & 1,
        self_damage,
        player_health,
        first.probe.last_health,
        first.probe.boss & 0xffff,
        first.probe.boss >> 16,
        first.probe.monster_attack,
        first.probe.monster_death,
        first.probe.monster_animated,
        first.vram_hash,
        first.display_width,
        first.display_height,
        first.display_hash,
    );
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct CombatRun {
    probe: Probe,
    polls: u64,
    vram_hash: u64,
    display_hash: u64,
    display_width: u32,
    display_height: u32,
}

fn run_combat_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    const MISSING_SOUND: &[u8] = b"Snd_FindSound: sound";
    if log
        .windows(MISSING_SOUND.len())
        .any(|window| window == MISSING_SOUND)
    {
        return Err("combat regression requested a sound missing from E1M1's cooked bank".into());
    }
    let probe = read_probe(&capture.join("ram.bin"))?;
    validate_combat_probe(&probe)?;
    let display = require_visible_display(&log, "combat regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("combat regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn run_monster_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    const MISSING_SOUND: &[u8] = b"Snd_FindSound: sound";
    if log
        .windows(MISSING_SOUND.len())
        .any(|window| window == MISSING_SOUND)
    {
        return Err("monster regression requested a sound missing from E1M1's cooked bank".into());
    }
    let probe = read_probe(&capture.join("ram.bin"))?;
    validate_monster_probe(&probe)?;
    let display = require_visible_display(&log, "monster regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("monster regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn run_arsenal_once(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    guest_frames: u64,
) -> Result<CombatRun> {
    fs::create_dir_all(capture)?;
    let output = run_frontend(frontend, cue, capture, steps, true, Some(guest_frames))?;
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    const MISSING_SOUND: &[u8] = b"Snd_FindSound: sound";
    if log
        .windows(MISSING_SOUND.len())
        .any(|window| window == MISSING_SOUND)
    {
        return Err("arsenal regression requested a sound missing from E1M5's cooked bank".into());
    }
    let probe = read_probe(&capture.join("ram.bin"))?;
    validate_arsenal_probe(&probe)?;
    let display = require_visible_display(&log, "arsenal regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls == 0 {
        return Err("arsenal regression observed no controller polls".into());
    }
    Ok(CombatRun {
        probe,
        polls,
        vram_hash: parse_frontend_hash(&log, "vram_fnv1a_64=")?,
        display_hash: display.hash,
        display_width: display.width,
        display_height: display.height,
    })
}

fn parse_frontend_hash(log: &[u8], prefix: &str) -> Result<u64> {
    let text = String::from_utf8_lossy(log);
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(|| format!("frontend log lacks {prefix}<hash>"))?;
    Ok(u64::from_str_radix(
        value.strip_prefix("0x").unwrap_or(value),
        16,
    )?)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DisplayHash {
    hash: u64,
    width: u32,
    height: u32,
}

fn parse_frontend_display_hash(log: &[u8]) -> Result<DisplayHash> {
    let text = String::from_utf8_lossy(log);
    let line = text
        .lines()
        .find(|line| line.starts_with("display_fnv1a_64="))
        .ok_or("frontend log lacks display hash")?;
    let mut fields = line.split_whitespace();
    let hash = fields
        .next()
        .and_then(|field| field.strip_prefix("display_fnv1a_64="))
        .ok_or("display hash field is malformed")?;
    let width = fields
        .find_map(|field| field.strip_prefix("w="))
        .ok_or("display hash line lacks width")?
        .parse()?;
    let height = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("h="))
        .ok_or("display hash line lacks height")?
        .parse()?;
    Ok(DisplayHash {
        hash: u64::from_str_radix(hash.strip_prefix("0x").unwrap_or(hash), 16)?,
        width,
        height,
    })
}

fn fnv1a_zero_bytes(len: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for _ in 0..len {
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn require_visible_display(log: &[u8], label: &str) -> Result<DisplayHash> {
    let display = parse_frontend_display_hash(log)?;
    if (display.width, display.height) != (320, 240) {
        return Err(format!(
            "{label} display is {}x{}, expected 320x240",
            display.width, display.height
        )
        .into());
    }
    let byte_len = usize::try_from(display.width)?
        .checked_mul(usize::try_from(display.height)?)
        .and_then(|pixels| pixels.checked_mul(2))
        .ok_or("display byte length overflow")?;
    if display.hash == fnv1a_zero_bytes(byte_len) {
        return Err(format!("{label} produced an all-black 15bpp display").into());
    }
    Ok(display)
}

fn read_ppm(path: &Path) -> Result<PpmImage> {
    let bytes = fs::read(path)?;
    let mut line_ends = bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index));
    let first = line_ends.next().ok_or("PPM lacks magic line")?;
    let second = line_ends.next().ok_or("PPM lacks dimensions line")?;
    let third = line_ends.next().ok_or("PPM lacks maximum-value line")?;
    if &bytes[..first] != b"P6" || &bytes[second + 1..third] != b"255" {
        return Err("capture must be an 8-bit binary P6 PPM".into());
    }
    let dimensions = core::str::from_utf8(&bytes[first + 1..second])?;
    let mut fields = dimensions.split_whitespace();
    let width: usize = fields.next().ok_or("PPM lacks width")?.parse()?;
    let height: usize = fields.next().ok_or("PPM lacks height")?.parse()?;
    if fields.next().is_some() {
        return Err("PPM dimensions line has extra fields".into());
    }
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or("PPM dimensions overflow")?;
    let rgb = bytes[third + 1..].to_vec();
    if rgb.len() != expected {
        return Err(format!("PPM has {} RGB bytes, expected {expected}", rgb.len()).into());
    }
    Ok(PpmImage { width, height, rgb })
}

fn crop_ppm(image: &PpmImage, region: ImageRegion) -> Result<PpmImage> {
    let right = region
        .x
        .checked_add(region.width)
        .ok_or("crop x overflow")?;
    let bottom = region
        .y
        .checked_add(region.height)
        .ok_or("crop y overflow")?;
    if right > image.width || bottom > image.height || region.width == 0 || region.height == 0 {
        return Err(format!(
            "crop {},{} {}x{} is outside {}x{}",
            region.x, region.y, region.width, region.height, image.width, image.height
        )
        .into());
    }
    let mut rgb = Vec::with_capacity(region.width * region.height * 3);
    for y in region.y..bottom {
        let start = (y * image.width + region.x) * 3;
        let end = start + region.width * 3;
        rgb.extend_from_slice(&image.rgb[start..end]);
    }
    Ok(PpmImage {
        width: region.width,
        height: region.height,
        rgb,
    })
}

fn write_ppm(path: &Path, image: &PpmImage) -> Result<()> {
    let header = format!("P6\n{} {}\n255\n", image.width, image.height);
    let mut bytes = Vec::with_capacity(header.len() + image.rgb.len());
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(&image.rgb);
    fs::write(path, bytes)?;
    Ok(())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn run_audio_regression(root: &Path, frontend: &Path, shipping: &Path) -> Result<()> {
    // Cold boot and NEW GAME both stream assets. Fire after controller polling
    // resumes from the second load.
    //
    // Route ticks place the input consistently. Find the shot in the captured
    // audio, then measure silence on either side rather than using wall time.
    const ACCEPT_TICK: u64 = 2_200;
    const PRESS_TICK: u64 = 3_400;
    const PRESS_HOLD_TICKS: u64 = 240;
    const ACTIVE_THRESHOLD: u16 = 512;
    /// Silence required before the press and after the shot's decay.
    const SILENCE_SECONDS: u64 = 5;
    /// The shot train is a burst of separate blasts; gaps shorter than this
    /// belong to the same train.
    const SHOT_GAP_SECONDS: u64 = 1;

    let capture = root.join("captures/audio-regression");
    fs::create_dir_all(&capture)?;
    // Instructions, not emulated time: a busier guest burns the budget sooner,
    // so this is set for the emulated seconds the gate needs plus headroom and
    // the capture length is checked below rather than assumed.
    let steps = env::var("QUAKE_PSX_AUDIO_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_500_000_000u64);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(shipping.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--guest-debug-log")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--press")
        // Long holds because attract/menu frames poll the pad only a few
        // times per second; the fire edge still lands at the first in-game
        // poll after PRESS_TICK.
        .arg(format!(
            "{ACCEPT_TICK}:cross:240,{PRESS_TICK}:r2:{PRESS_HOLD_TICKS}"
        ))
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--dump-audio")
        .arg(capture.join("shot.wav"))
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "audio regression emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;
    let telemetry = String::from_utf8_lossy(&log);
    let global_phase = telemetry
        .lines()
        .find(|line| line.contains("quake-psx: audio-global"))
        .ok_or("audio regression emitted no global-bank telemetry")?;
    if !global_phase.contains("hit=0") || global_phase.contains("upload-bytes=0x00000000") {
        return Err("audio regression did not upload the cold global bank".into());
    }
    let local_phases = telemetry
        .lines()
        .filter(|line| line.contains("quake-psx: audio-local"))
        .collect::<Vec<_>>();
    if local_phases.len() < 2
        || !local_phases[0].contains("hit=0")
        || local_phases[0].contains("upload-bytes=0x00000000")
        || !local_phases.iter().skip(1).any(|line| {
            line.contains("source-bytes=0x00000000")
                && line.contains("upload-bytes=0x00000000")
                && line.contains("sessions=0x00000000")
                && line.contains("hit=1")
        })
    {
        return Err(format!(
            "audio residency telemetry did not prove cold-load then zero-I/O same-map reset: {local_phases:?}"
        )
        .into());
    }

    let display = require_visible_display(&log, "audio regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls < 2 {
        return Err(format!("audio regression observed only {polls} controller polls").into());
    }

    let route_ticks = final_route_tick(&capture.join("route.csv"))?;
    let wav_bytes = fs::read(capture.join("shot.wav"))?;
    let wav = parse_wav_pcm16_stereo(&wav_bytes)?;
    let rate = u64::from(wav.sample_rate);
    let frames = wav.frame_count() as u64;

    // The capture and the route log cover the same run, so the run's own
    // sample-per-route-tick rate converts the scripted press tick into a
    // capture position without hard-coding any clock.
    if route_ticks == 0 {
        return Err("audio regression route log recorded no ticks".into());
    }
    let press_frame = PRESS_TICK
        .checked_mul(frames)
        .ok_or("press position overflow")?
        / route_ticks;
    let required_frames =
        press_frame + (PRESS_HOLD_TICKS * frames / route_ticks) + SILENCE_SECONDS * rate;
    if frames < required_frames {
        return Err(format!(
            "audio regression capture is {:.1}s ({frames} frames) but the R2 press at route \
             tick {PRESS_TICK}, its {PRESS_HOLD_TICKS}-tick hold and {SILENCE_SECONDS}s of tail \
             need {:.1}s; raise QUAKE_PSX_AUDIO_REGRESSION_STEPS",
            frames as f64 / rate as f64,
            required_frames as f64 / rate as f64,
        )
        .into());
    }

    // Silence going in, anchored to the press rather than to a wall clock
    // second: the NEW GAME reload is quiet, so nothing may be sounding when
    // the fire button goes down.
    let quiet = wav.range_stats(
        press_frame.saturating_sub(SILENCE_SECONDS * rate),
        press_frame,
        ACTIVE_THRESHOLD,
    )?;
    if quiet.peak >= ACTIVE_THRESHOLD || quiet.active_frames != 0 {
        return Err(format!(
            "audio regression had unexpected audio in the {SILENCE_SECONDS}s before the R2 press: \
             peak={} active_frames={}",
            quiet.peak, quiet.active_frames
        )
        .into());
    }

    // The shot train: the first thing heard at or after the press, through the
    // last thing heard in the whole capture.
    let Some(shot_start) = wav.first_active_frame(press_frame, frames, ACTIVE_THRESHOLD)? else {
        return Err("R2 shot was never audible after the press".into());
    };
    let Some(shot_end) = wav.last_active_frame(shot_start, frames, ACTIVE_THRESHOLD)? else {
        return Err("R2 shot was never audible after the press".into());
    };
    let shot = wav.range_stats(shot_start, shot_end + 1, ACTIVE_THRESHOLD)?;
    if shot.peak < 2_048 || shot.active_frames < rate / 20 {
        return Err(format!(
            "R2 shot was not audible: peak={} active_frames={}",
            shot.peak, shot.active_frames
        )
        .into());
    }
    // A shot train is separate blasts. Anything sounding again after a gap
    // longer than one second is not part of it and must not be dismissed as
    // decay, so the tail is measured from the last sound in the capture.
    let gap = wav.longest_silence(shot_start, shot_end + 1, ACTIVE_THRESHOLD)?;
    if gap >= SHOT_GAP_SECONDS * rate {
        return Err(format!(
            "audio regression heard a second sound {:.2}s after the shot train went quiet",
            gap as f64 / rate as f64
        )
        .into());
    }

    // Returning to silence: the capture must still run for the full silent
    // window after the last sound, and that window must be silent. The first
    // half is what makes this a measurement instead of running out of tape.
    let tail_frames = frames - (shot_end + 1);
    if tail_frames < SILENCE_SECONDS * rate {
        return Err(format!(
            "R2 shot did not return to silence: the capture ends {:.2}s after the last audible \
             frame, less than the {SILENCE_SECONDS}s of silence the gate requires",
            tail_frames as f64 / rate as f64
        )
        .into());
    }
    let tail = wav.range_stats(
        shot_end + 1,
        shot_end + 1 + SILENCE_SECONDS * rate,
        ACTIVE_THRESHOLD,
    )?;
    if tail.peak >= ACTIVE_THRESHOLD || tail.active_frames != 0 {
        return Err(format!(
            "R2 shot did not return to silence: peak={} active_frames={}",
            tail.peak, tail.active_frames
        )
        .into());
    }

    let summary = format!(
        "quake-psx Rust audio regression: PASS\n\
         controller_polls={polls}\n\
         display_fnv1a_64=0x{:016x}\n\
         capture_seconds={:.2}\n\
         press_second={:.2}\n\
         shot_start_second={:.2}\n\
         shot_end_second={:.2}\n\
         pre_shot_peak={}\n\
         shot_peak={}\n\
         shot_active_frames={}\n\
         tail_seconds={:.2}\n\
         tail_peak={}\n",
        display.hash,
        frames as f64 / rate as f64,
        press_frame as f64 / rate as f64,
        shot_start as f64 / rate as f64,
        shot_end as f64 / rate as f64,
        quiet.peak,
        shot.peak,
        shot.active_frames,
        tail_frames as f64 / rate as f64,
        tail.peak
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

fn run_ambient_regression(root: &Path, frontend: &Path, build: &Path) -> Result<()> {
    const ACTIVE_THRESHOLD: u16 = 128;
    const MINIMUM_PEAK: u16 = 384;

    let capture = root.join("captures/ambient-regression");
    fs::create_dir_all(&capture)?;
    let steps = env::var("QUAKE_PSX_AMBIENT_REGRESSION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        // Allow enough emulated time to reach the final 52-second sample.
        .unwrap_or(900_000_000);
    let mut command = Command::new(frontend);
    let output = command
        .arg("launch")
        .arg("--path")
        .arg(build.join("quake-psx.cue"))
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--dump-audio")
        .arg(capture.join("ambient.wav"))
        .arg("--dump-hash")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "ambient regression emulator failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&combined_output(&output))
        )
        .into());
    }
    let log = combined_output(&output);
    fs::write(capture.join("console.log"), &log)?;

    let display = require_visible_display(&log, "ambient regression")?;
    let polls = final_port1_polls(&capture.join("route.csv"))?;
    if polls < 2 {
        return Err(format!("ambient regression observed only {polls} controller polls").into());
    }

    let wav_bytes = fs::read(capture.join("ambient.wav"))?;
    let wav = parse_wav_pcm16_stereo(&wav_bytes)?;
    let early = wav.window_stats(36, 39, ACTIVE_THRESHOLD)?;
    // `drip1.wav` is a sparse 12.470-second loop. Sampling again thirteen
    // seconds later proves the SPU voice wrapped instead of merely playing a
    // one-shot tail.
    let late = wav.window_stats(49, 52, ACTIVE_THRESHOLD)?;
    // The source contains isolated drops separated by silence, so require at
    // least ten milliseconds of real signal in each three-second window.
    let minimum_active = u64::from(wav.sample_rate) / 100;
    if early.peak < MINIMUM_PEAK || early.active_frames < minimum_active {
        return Err(format!(
            "spatial drip loop was not audible: peak={} active_frames={}",
            early.peak, early.active_frames
        )
        .into());
    }
    if late.peak < MINIMUM_PEAK || late.active_frames < minimum_active {
        return Err(format!(
            "spatial drip loop did not wrap: peak={} active_frames={}",
            late.peak, late.active_frames
        )
        .into());
    }

    let summary = format!(
        "quake-psx Rust ambient regression: PASS\n\
         controller_polls={polls}\n\
         display_fnv1a_64=0x{:016x}\n\
         early_peak={}\n\
         early_active_frames={}\n\
         late_peak={}\n\
         late_active_frames={}\n",
        display.hash, early.peak, early.active_frames, late.peak, late.active_frames
    );
    fs::write(capture.join("summary.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}

struct Pcm16Stereo<'a> {
    sample_rate: u32,
    data: &'a [u8],
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AudioWindowStats {
    peak: u16,
    active_frames: u64,
}

impl Pcm16Stereo<'_> {
    const FRAME_BYTES: usize = 4;

    fn frame_count(&self) -> usize {
        self.data.len() / Self::FRAME_BYTES
    }

    fn frame_peak(&self, frame: usize) -> u16 {
        let base = frame * Self::FRAME_BYTES;
        let left = i16::from_le_bytes([self.data[base], self.data[base + 1]]).unsigned_abs();
        let right = i16::from_le_bytes([self.data[base + 2], self.data[base + 3]]).unsigned_abs();
        left.max(right)
    }

    fn checked_range(&self, start: u64, end: u64) -> Result<(usize, usize)> {
        if start >= end {
            return Err("audio window is empty".into());
        }
        let start = usize::try_from(start)?;
        let end = usize::try_from(end)?;
        let frame_count = self.frame_count();
        if end > frame_count {
            return Err(
                format!("audio capture has {frame_count} frames, window needs {end}").into(),
            );
        }
        Ok((start, end))
    }

    /// Peak and active-frame count over a half-open second range. Only the
    /// ambient loop uses this: it asserts a continuously running emitter, so it
    /// has no decaying edge for guest pacing to move.
    fn window_stats(
        &self,
        start_seconds: usize,
        end_seconds: usize,
        active_threshold: u16,
    ) -> Result<AudioWindowStats> {
        let rate = u64::from(self.sample_rate);
        self.range_stats(
            start_seconds as u64 * rate,
            end_seconds as u64 * rate,
            active_threshold,
        )
    }

    /// Peak and active-frame count over a half-open frame range.
    fn range_stats(&self, start: u64, end: u64, active_threshold: u16) -> Result<AudioWindowStats> {
        let (start, end) = self.checked_range(start, end)?;
        let mut peak = 0u16;
        let mut active_frames = 0u64;
        for frame in start..end {
            let frame_peak = self.frame_peak(frame);
            peak = peak.max(frame_peak);
            if frame_peak >= active_threshold {
                active_frames += 1;
            }
        }
        Ok(AudioWindowStats {
            peak,
            active_frames,
        })
    }

    fn first_active_frame(
        &self,
        start: u64,
        end: u64,
        active_threshold: u16,
    ) -> Result<Option<u64>> {
        let (start, end) = self.checked_range(start, end)?;
        Ok((start..end)
            .find(|&frame| self.frame_peak(frame) >= active_threshold)
            .map(|frame| frame as u64))
    }

    fn last_active_frame(
        &self,
        start: u64,
        end: u64,
        active_threshold: u16,
    ) -> Result<Option<u64>> {
        let (start, end) = self.checked_range(start, end)?;
        Ok((start..end)
            .rev()
            .find(|&frame| self.frame_peak(frame) >= active_threshold)
            .map(|frame| frame as u64))
    }

    /// Longest run of below-threshold frames inside a half-open range.
    fn longest_silence(&self, start: u64, end: u64, active_threshold: u16) -> Result<u64> {
        let (start, end) = self.checked_range(start, end)?;
        let mut longest = 0u64;
        let mut run = 0u64;
        for frame in start..end {
            if self.frame_peak(frame) >= active_threshold {
                run = 0;
            } else {
                run += 1;
                longest = longest.max(run);
            }
        }
        Ok(longest)
    }
}

fn parse_wav_pcm16_stereo(bytes: &[u8]) -> Result<Pcm16Stereo<'_>> {
    if bytes.len() < 12 || bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("audio capture is not RIFF/WAVE".into());
    }
    let mut sample_rate = None;
    let mut data = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let kind = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.checked_add(len).ok_or("WAV chunk overflow")?;
        let chunk = bytes.get(start..end).ok_or("WAV chunk extends past EOF")?;
        if kind == b"fmt " {
            if chunk.len() < 16
                || u16::from_le_bytes(chunk[0..2].try_into().unwrap()) != 1
                || u16::from_le_bytes(chunk[2..4].try_into().unwrap()) != 2
                || u16::from_le_bytes(chunk[14..16].try_into().unwrap()) != 16
            {
                return Err("audio capture is not PCM16 stereo".into());
            }
            sample_rate = Some(u32::from_le_bytes(chunk[4..8].try_into().unwrap()));
        } else if kind == b"data" {
            if chunk.len() % 4 != 0 {
                return Err("audio capture has a partial stereo frame".into());
            }
            data = Some(chunk);
        }
        offset = end.checked_add(len & 1).ok_or("WAV alignment overflow")?;
    }
    Ok(Pcm16Stereo {
        sample_rate: sample_rate.ok_or("audio capture has no format chunk")?,
        data: data.ok_or("audio capture has no data chunk")?,
    })
}

fn run_frontend(
    frontend: &Path,
    cue: &Path,
    capture: &Path,
    steps: u64,
    dump_ram: bool,
    guest_frames: Option<u64>,
) -> Result<Output> {
    let mut command = Command::new(frontend);
    command
        .arg("launch")
        .arg("--path")
        .arg(cue)
        .arg("--digital-pad")
        .arg("--steps")
        .arg(steps.to_string())
        .arg("--route-log")
        .arg(capture.join("route.csv"))
        .arg("--cd-command-log")
        .arg(capture.join("cd.csv"))
        .arg("--dump-hash");
    if dump_ram {
        command.arg("--dump-ram").arg(capture.join("ram.bin"));
    }
    if dump_ram {
        command.arg("--guest-debug-log");
    }
    if let Some(frames) = guest_frames {
        command.arg("--guest-frames").arg(frames.to_string());
    }
    output(&mut command)
}

fn initial_load_metrics(route_csv: &str, cd_csv: &str) -> Result<(u64, usize)> {
    let mut route_rows = route_csv.lines().skip(1).filter(|line| !line.is_empty());
    let first = route_rows.next().ok_or("route log has no samples")?;
    let first_fields = first.split(',').collect::<Vec<_>>();
    let baseline = first_fields
        .get(3)
        .ok_or("route log is missing bus_cycles")?
        .parse::<u64>()?;
    let gameplay_flip = first_gameplay_flip_cycle(route_rows)?;
    let readn_cycles = cd_csv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split(',');
            let cycle = fields.next()?.parse::<u64>().ok()?;
            (fields.next()? == "0x06").then_some(cycle)
        })
        .collect::<Vec<_>>();
    // A loading image may present without changing display start when buffer
    // zero is already selected. Controller polling starts only after the map,
    // entities, and audio are resident, so the first changed display row with
    // at least one poll is the durable gameplay-ready boundary. Loading-order
    // proof remains a separate present-before-payload assertion.
    let elapsed = gameplay_flip
        .checked_sub(baseline)
        .ok_or("gameplay presentation precedes route baseline")?;
    let reads = readn_cycles
        .iter()
        .filter(|&&cycle| cycle <= gameplay_flip)
        .count();
    Ok((elapsed, reads))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FullLevelRenderMetrics {
    presentations: u64,
    elapsed_bus_cycles: u64,
    fps_x1000: u64,
}

/// Measure rendered cadence from the first gameplay presentation until the
/// next map begins reading. This excludes both cold loading and post-route
/// idle time while covering the entire authored level traversal.
fn full_level_render_metrics(route_csv: &str, cd_csv: &str) -> Result<FullLevelRenderMetrics> {
    const PS1_BUS_CLOCK_HZ: u64 = 33_868_800;
    let rows = route_csv
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let readn_cycles = cd_csv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split(',');
            let cycle = fields.next()?.parse::<u64>().ok()?;
            (fields.next()? == "0x06").then_some(cycle)
        })
        .collect::<Vec<_>>();
    let (initial_last_read, transition_start) = readn_cycles
        .windows(2)
        .max_by_key(|pair| pair[1].saturating_sub(pair[0]))
        .map(|pair| (pair[0], pair[1]))
        .ok_or("CD log has fewer than two ReadN sessions")?;
    let flips = rows
        .iter()
        .filter_map(|line| {
            let fields = line.split(',').collect::<Vec<_>>();
            let cycle = fields.get(3)?.parse::<u64>().ok()?;
            (fields.get(10) == Some(&"1") && cycle > initial_last_read && cycle < transition_start)
                .then_some(cycle)
        })
        .collect::<Vec<_>>();
    if flips.len() < 2 {
        return Err("full-level render window contains fewer than two presentations".into());
    }
    let intervals = flips.len() as u64 - 1;
    let elapsed_bus_cycles = flips[flips.len() - 1]
        .checked_sub(flips[0])
        .ok_or("full-level presentation cycles are not monotonic")?;
    if elapsed_bus_cycles == 0 {
        return Err("full-level presentation window has zero elapsed cycles".into());
    }
    let numerator = intervals
        .checked_mul(PS1_BUS_CLOCK_HZ)
        .and_then(|value| value.checked_mul(1_000))
        .ok_or("full-level FPS numerator overflow")?;
    Ok(FullLevelRenderMetrics {
        presentations: flips.len() as u64,
        elapsed_bus_cycles,
        fps_x1000: numerator / elapsed_bus_cycles,
    })
}

fn cold_local_audio_publication_cycle(telemetry: &str) -> Result<u64> {
    let line = telemetry
        .lines()
        .find(|line| {
            line.contains("quake-psx: audio-local")
                && line.contains("hit=0")
                && !line.contains("source-bytes=0x00000000")
                && !line.contains("upload-bytes=0x00000000")
        })
        .ok_or("survival regression emitted no cold local-audio publication marker")?;
    line.split_whitespace()
        .find_map(|field| {
            field
                .strip_prefix('c')?
                .strip_suffix(']')?
                .parse::<u64>()
                .ok()
        })
        .ok_or_else(|| "cold local-audio publication marker has no guest cycle".into())
}

fn require_display_flip_after_cycle(route_csv: &str, boundary: u64) -> Result<u64> {
    route_csv
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let fields = line.split(',').collect::<Vec<_>>();
            let cycle = fields.get(3)?.parse::<u64>().ok()?;
            (cycle > boundary && fields.get(10) == Some(&"1")).then_some(cycle)
        })
        .next()
        .ok_or_else(|| {
            "survival regression never presented gameplay after cold level publication".into()
        })
}

fn readn_sessions_after_cycle(cd_csv: &str, boundary: u64) -> Result<usize> {
    Ok(cd_csv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split(',');
            let cycle = fields.next()?.parse::<u64>().ok()?;
            (fields.next()? == "0x06" && cycle > boundary).then_some(())
        })
        .count())
}

fn first_gameplay_flip_cycle<'a>(route_rows: impl Iterator<Item = &'a str>) -> Result<u64> {
    route_rows
        .filter_map(|line| {
            let fields = line.split(',').collect::<Vec<_>>();
            let polls = fields.get(11)?.parse::<u64>().ok()?;
            (fields.get(10) == Some(&"1") && polls > 0)
                .then(|| fields.get(3)?.parse::<u64>().ok())
                .flatten()
        })
        .next()
        .ok_or_else(|| "route log never presented gameplay after controller polling began".into())
}

fn read_probe(ram_path: &Path) -> Result<Probe> {
    let ram = fs::read(ram_path)?;
    let magic = PROBE_MAGIC.to_le_bytes();
    let candidates = ram
        .windows(PROBE_BYTES)
        .enumerate()
        .filter_map(|(offset, data)| {
            if offset & 3 == 0
                && data[..4] == magic
                && matches!(read_u32(data, 4), 3 | 4 | 5 | 6 | 7 | 10 | 11)
            {
                Some((offset, probe_from_bytes(data)))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "expected one Episode 1 probe in RAM, found {}",
            candidates.len()
        )
        .into());
    }
    let (offset, probe) = candidates.into_iter().next().unwrap();
    println!("Episode 1 probe: RAM+0x{offset:06x}");
    Ok(probe)
}

fn read_probe_version(ram_path: &Path, version: u32) -> Result<Probe> {
    let ram = fs::read(ram_path)?;
    let magic = PROBE_MAGIC.to_le_bytes();
    let candidates = ram
        .windows(PROBE_BYTES)
        .enumerate()
        .filter_map(|(offset, data)| {
            if offset & 3 == 0 && data[..4] == magic && read_u32(data, 4) == version {
                Some((offset, probe_from_bytes(data)))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "expected one version {version} gameplay probe in RAM, found {}",
            candidates.len(),
        )
        .into());
    }
    let (offset, probe) = candidates.into_iter().next().unwrap();
    println!("gameplay probe v{version}: RAM+0x{offset:06x}");
    Ok(probe)
}

fn probe_from_bytes(data: &[u8]) -> Probe {
    Probe {
        version: read_u32(data, 4),
        complete: read_u32(data, 8),
        phase: read_u32(data, 12),
        failure_code: read_u32(data, 16),
        failure_map: read_u32(data, 20),
        failure_entity: read_u32(data, 24),
        failure_detail: read_u32(data, 28),
        total_frames: read_u32(data, 32),
        maps_loaded: read_u32(data, 36),
        maps_validated: read_u32(data, 40),
        transitions: read_u32(data, 44),
        weapon_selected: read_u32(data, 48),
        weapon_fired: read_u32(data, 52),
        weapon_animated: read_u32(data, 56),
        monster_present: read_u32(data, 60),
        monster_animated: read_u32(data, 64),
        monster_state_bounds: read_u32(data, 68),
        monster_attack: read_u32(data, 72),
        monster_pain: read_u32(data, 76),
        monster_death: read_u32(data, 80),
        boss: read_u32(data, 84),
        current_map: read_u32(data, 88),
        route_index: read_u32(data, 92),
        last_health: read_u32(data, 96),
        state_ranges: read_u32(data, 100),
        valid_state_ranges: read_u32(data, 104),
        map_loads: read_u32(data, 108),
        stage_frames: read_u32(data, 112),
        shock_count: read_u32(data, 116),
        intermission_state: read_u32(data, 120),
        player_state: read_u32(data, 124),
        weapon_pickups: read_u32(data, 128),
        target_edges: read_u32(data, 132),
    }
}

fn read_visual_probe(ram_path: &Path) -> Result<VisualProbe> {
    let ram = fs::read(ram_path)?;
    let magic = VISUAL_PROBE_MAGIC.to_le_bytes();
    let candidates = ram
        .windows(VISUAL_PROBE_BYTES)
        .enumerate()
        .filter_map(|(offset, data)| {
            if offset & 3 == 0 && data[..4] == magic && read_u32(data, 4) == VISUAL_PROBE_VERSION {
                Some((
                    offset,
                    VisualProbe {
                        frames: read_u32(data, 8),
                        packets: read_u32(data, 12),
                        hardware_triangles: read_u32(data, 16),
                        windowed_packets: read_u32(data, 20),
                        window_resets: read_u32(data, 24),
                        reset_failures: read_u32(data, 28),
                        overflow_frames: read_u32(data, 32),
                        view_model_packets: read_u32(data, 36),
                        view_model_registered_packets: read_u32(data, 40),
                        hud_packets: read_u32(data, 44),
                        hud_registered_packets: read_u32(data, 48),
                        crosshair_registered_packets: read_u32(data, 52),
                        screen_registered_packets: read_u32(data, 56),
                    },
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "expected one visual parity probe in RAM, found {}",
            candidates.len()
        )
        .into());
    }
    let (offset, probe) = candidates.into_iter().next().unwrap();
    println!("visual parity probe v2: RAM+0x{offset:06x}");
    Ok(probe)
}

fn validate_visual_probe(probe: &VisualProbe) -> Result<()> {
    let window_resets_valid = probe.window_resets == probe.windowed_packets
        || probe.windowed_packets == probe.window_resets.saturating_add(probe.frames);
    if probe.frames < 120
        || probe.packets == 0
        || probe.packets > VISUAL_MAX_WORLD_PACKETS
        || probe.hardware_triangles == 0
        || probe.hardware_triangles > VISUAL_MAX_HARDWARE_TRIANGLES
        || probe.windowed_packets == 0
        || probe.windowed_packets > 12_000
        || !window_resets_valid
        || probe.reset_failures != 0
        || probe.overflow_frames != 0
        || probe.view_model_packets == 0
        || probe.view_model_registered_packets != probe.view_model_packets
        || probe.hud_packets == 0
        || probe.hud_registered_packets + probe.crosshair_registered_packets != probe.hud_packets
        || probe.screen_registered_packets
            != probe.view_model_registered_packets
                + probe.hud_registered_packets
                + probe.crosshair_registered_packets
    {
        return Err(format!(
            "visual packet audit failed: frames={} packets={} triangles={} windows={} resets={} failures={} overflow_frames={} view_model={}/{} hud={}/{} crosshair={} screen={}",
            probe.frames,
            probe.packets,
            probe.hardware_triangles,
            probe.windowed_packets,
            probe.window_resets,
            probe.reset_failures,
            probe.overflow_frames,
            probe.view_model_registered_packets,
            probe.view_model_packets,
            probe.hud_registered_packets,
            probe.hud_packets,
            probe.crosshair_registered_packets,
            probe.screen_registered_packets,
        )
        .into());
    }
    Ok(())
}

fn validate_start_route_probe(probe: &Probe) -> Result<()> {
    if probe.version != 4 {
        return Err(format!("unsupported Start route probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "Start route failed: code={} map={} detail=0x{:08x} phase={} frames={} player=0x{:02x} position=({}, {}, {})",
            probe.failure_code,
            probe.failure_map,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
            probe.player_state,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    if probe.complete != 1
        || probe.phase != 0x31
        || probe.maps_loaded != 0x003
        || probe.maps_validated != 0x003
        || probe.transitions != 0x001
        || probe.current_map != 1
        || probe.route_index != 1
        || probe.map_loads != 2
        || probe.player_state != 0x07
        || probe.target_edges == 0
    {
        return Err(format!(
            "Start route incomplete: complete={} phase={} map={} route={} loads={} maps=0x{:03x} validated=0x{:03x} transitions=0x{:03x} player=0x{:02x} edges={}",
            probe.complete,
            probe.phase,
            probe.current_map,
            probe.route_index,
            probe.map_loads,
            probe.maps_loaded,
            probe.maps_validated,
            probe.transitions,
            probe.player_state,
            probe.target_edges,
        )
        .into());
    }
    Ok(())
}

fn validate_e1m1_chain_probe(probe: &Probe) -> Result<()> {
    if probe.version != 9 {
        return Err(format!("unsupported E1M1 chain probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "E1M1 chain failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={} waypoint={} mechanisms=0x{:02x} position=({}, {}, {})",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
            probe.route_index,
            probe.player_state,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    // The route now finishes on E1M1's own change-level edge, so the pins
    // move with it: both maps loaded, E1M2 current, the whole waypoint list
    // retired, and the mechanism mask carrying `EXIT_E1M2` on top of the
    // authored chain.
    if probe.complete != 1
        || probe.phase != 0x51
        || probe.maps_loaded != 0x006
        || probe.maps_validated != 0x006
        || probe.current_map != 2
        || probe.route_index != 60
        || probe.map_loads != 2
        || probe.transitions != 1
        || probe.player_state != 0x7fff
        || probe.weapon_selected & 0x07 != 0x07
        || probe.target_edges < 4
    {
        return Err(format!(
            "E1M1 chain incomplete: complete={} phase={} map={} waypoint={} loads={} maps=0x{:03x} validated=0x{:03x} mechanisms=0x{:02x} mover_sounds=0x{:02x} edges={} position=({}, {}, {})",
            probe.complete,
            probe.phase,
            probe.current_map,
            probe.route_index,
            probe.map_loads,
            probe.maps_loaded,
            probe.maps_validated,
            probe.player_state,
            probe.weapon_selected,
            probe.target_edges,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    Ok(())
}

fn validate_e1m2_route_probe(probe: &Probe) -> Result<()> {
    if probe.version != 14 {
        return Err(format!(
            "unsupported E1M2/E1M3 route probe version {}",
            probe.version
        )
        .into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "E1M2/E1M3 route failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={} stage={} waypoint={} mechanisms=0x{:08x} position=({}, {}, {})",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.total_frames,
            probe.intermission_state,
            probe.route_index,
            probe.player_state,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    if probe.complete != 1
        || probe.phase != 0x23
        || probe.maps_loaded != 0x01c
        || probe.maps_validated != 0x01c
        || probe.current_map != 4
        || probe.map_loads != 3
        || probe.transitions != 2
        || probe.player_state != 0x1fff_ffff
        || probe.weapon_fired != 2
        || probe.weapon_pickups != 2
        || probe.target_edges < 10
    {
        return Err(format!(
            "E1M2/E1M3 route incomplete: complete={} phase={} map={} stage={} waypoint={} loads={} maps=0x{:03x} validated=0x{:03x} transitions={} mechanisms=0x{:08x} shots={} pickups={} edges={} position=({}, {}, {})",
            probe.complete,
            probe.phase,
            probe.current_map,
            probe.intermission_state,
            probe.route_index,
            probe.map_loads,
            probe.maps_loaded,
            probe.maps_validated,
            probe.transitions,
            probe.player_state,
            probe.weapon_fired,
            probe.weapon_pickups,
            probe.target_edges,
            probe.last_health as i32,
            probe.state_ranges as i32,
            probe.valid_state_ranges as i32,
        )
        .into());
    }
    Ok(())
}

fn validate_probe(probe: &Probe) -> Result<()> {
    if probe.version != 3 {
        return Err(format!("unsupported probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "Episode 1 failed: code={} map={} entity={} detail=0x{:08x} phase={} route={} stage_frames={} intermission={} player=0x{:08x}",
            probe.failure_code, probe.failure_map, probe.failure_entity, probe.failure_detail,
            probe.phase, probe.route_index, probe.stage_frames, probe.intermission_state, probe.player_state,
        ).into());
    }
    if probe.complete != 1 {
        return Err(format!(
            "Episode 1 did not finish: phase={} map={} route={} frames={} maps=0x{:03x} transitions=0x{:03x} weapons=0x{:02x} monsters=0x{:03x} boss=0x{:03x}",
            probe.phase, probe.current_map, probe.route_index, probe.total_frames, probe.maps_loaded,
            probe.transitions, probe.weapon_fired, probe.monster_present, probe.boss,
        ).into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, 0x01ff),
        ("maps validated", probe.maps_validated, 0x01ff),
        ("transitions", probe.transitions, 0x03ff),
        ("weapons selected", probe.weapon_selected, 0x00ff),
        ("weapons fired", probe.weapon_fired, 0x00ff),
        ("weapons animated", probe.weapon_animated, 0x00ff),
        ("monsters present", probe.monster_present, 0x01ff),
        ("monsters animated", probe.monster_animated, 0x01ff),
        ("monster state bounds", probe.monster_state_bounds, 0x01ff),
        ("monster attacks", probe.monster_attack, 0x01ff),
        ("monster pain", probe.monster_pain, 0x01ff),
        ("monster deaths", probe.monster_death, 0x01ff),
        ("boss encounter", probe.boss, 0x01ff),
        ("weapon acquisition", probe.weapon_pickups, 0x007f),
    ] {
        if actual != expected {
            return Err(format!("{label}: 0x{actual:03x}, expected 0x{expected:03x}").into());
        }
    }
    if probe.state_ranges == 0 || probe.state_ranges != probe.valid_state_ranges {
        return Err(format!(
            "animation ranges: {} total, {} valid",
            probe.state_ranges, probe.valid_state_ranges
        )
        .into());
    }
    if probe.target_edges == 0 {
        return Err("regression validated no progression target edges".into());
    }
    Ok(())
}

fn validate_map_probe(probe: &Probe) -> Result<()> {
    const GPU_ARENA_WORDS: u32 = 128 * 1024 / 4;
    const MIN_PACKET_MARGIN_WORDS: u32 = 8 * 1024 / 4;
    if probe.version != 3 {
        return Err(format!("unsupported probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "map regression failed: code={} map={} detail={} phase={} route={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_detail,
            probe.phase,
            probe.route_index,
        )
        .into());
    }
    // Guest-side `func_train` evidence from E1M5's four self-starting trains.
    // The host measures every authored leg at 87 ticks or fewer over the same
    // cooked lumps (`crates/quake-core/tests/e1m5_train_legs.rs`); a guest that
    // saturates `travel_ticks` produced 27804 for one of them. A host number is
    // not evidence about the guest, so the guest's own longest leg is checked
    // here, together with the distance it actually moved the trains.
    if probe.state_ranges == 0 || probe.state_ranges >= 3_600 {
        return Err(format!(
            "guest func_train leg length {} ticks is outside the believable band \
             for a map a few thousand units across (host measures 87 or fewer); \
             offset=({},{},{}) distance={} leg_end=({},{},{}) from=({},{},{}) \
             corner_x_as_read={} mins_yz=({},{}), all in whole units",
            probe.state_ranges,
            probe.weapon_selected as i32,
            probe.weapon_fired as i32,
            probe.weapon_animated as i32,
            probe.monster_present as i32,
            probe.monster_animated as i32,
            probe.monster_state_bounds as i32,
            probe.monster_attack as i32,
            probe.monster_pain as i32,
            probe.monster_death as i32,
            probe.boss as i32,
            probe.last_health as i32,
            probe.shock_count as i32,
            probe.intermission_state as i32,
        )
        .into());
    }
    if probe.valid_state_ranges == 0 {
        return Err("guest func_train never moved on E1M5".into());
    }
    if probe.weapon_pickups == 0
        || probe.weapon_pickups > GPU_ARENA_WORDS.saturating_sub(MIN_PACKET_MARGIN_WORDS)
    {
        return Err(format!(
            "packet arena high-water is {} of {} words ({} words free; require {}): \
             the 128 KiB arena no longer has its fixed 8 KiB safety reserve",
            probe.weapon_pickups,
            GPU_ARENA_WORDS,
            GPU_ARENA_WORDS.saturating_sub(probe.weapon_pickups),
            MIN_PACKET_MARGIN_WORDS,
        )
        .into());
    }
    if probe.target_edges != 0 {
        return Err(format!(
            "packet arena overflow avoidance fired on {} map-route frames",
            probe.target_edges,
        )
        .into());
    }
    if probe.phase != 0x40
        || probe.complete != 0
        || probe.maps_loaded != 0x01ff
        || probe.maps_validated != 0x01ff
        || probe.transitions != 0x03ff
        || probe.map_loads != 12
        || probe.route_index != 11
        || probe.current_map != 5
    {
        return Err(format!(
            "map route incomplete: phase={} complete={} map={} route={} loads={} maps=0x{:03x} validated=0x{:03x} transitions=0x{:03x}",
            probe.phase,
            probe.complete,
            probe.current_map,
            probe.route_index,
            probe.map_loads,
            probe.maps_loaded,
            probe.maps_validated,
            probe.transitions,
        )
        .into());
    }
    Ok(())
}

fn validate_combat_probe(probe: &Probe) -> Result<()> {
    const E1M1_BIT: u32 = 1 << 1;
    const SHOTGUN_BIT: u32 = 1 << 1;
    const SPLASH_TRIGGER_SOURCE: u32 = 31;
    const SOLID_EXPLOBOX_SOURCE: u32 = 150;
    if probe.version != 3 {
        return Err(format!("unsupported probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "combat regression failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.stage_frames,
        )
        .into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, E1M1_BIT),
        ("maps validated", probe.maps_validated, E1M1_BIT),
        ("weapon selected", probe.weapon_selected, SHOTGUN_BIT),
        ("weapon fired", probe.weapon_fired, SHOTGUN_BIT),
        ("weapon animated", probe.weapon_animated, SHOTGUN_BIT),
        ("monster present", probe.monster_present, E1M1_BIT),
        ("monster hitbox", probe.monster_state_bounds, E1M1_BIT),
        ("monster pain", probe.monster_pain, E1M1_BIT),
        ("monster death", probe.monster_death, E1M1_BIT),
        ("splash trigger", probe.transitions, SPLASH_TRIGGER_SOURCE),
        ("solid explobox", probe.boss, SOLID_EXPLOBOX_SOURCE),
        ("blood particles", probe.target_edges, 1),
    ] {
        if actual != expected {
            return Err(format!("{label}: 0x{actual:03x}, expected 0x{expected:03x}").into());
        }
    }
    if probe.complete != 1
        || probe.phase != 0x50
        || probe.current_map != 1
        || probe.map_loads != 1
        || probe.last_health != 0
    {
        return Err(format!(
            "combat route incomplete: complete={} phase={} map={} loads={} health={}",
            probe.complete, probe.phase, probe.current_map, probe.map_loads, probe.last_health,
        )
        .into());
    }
    Ok(())
}

fn validate_monster_probe(probe: &Probe) -> Result<()> {
    const E1M1_BIT: u32 = 1 << 1;
    if probe.version != 6 {
        return Err(format!("unsupported monster probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "monster regression failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.stage_frames,
        )
        .into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, E1M1_BIT),
        ("maps validated", probe.maps_validated, E1M1_BIT),
        ("Soldier and Dog present", probe.monster_present, 0x03),
        ("Soldier and Dog hitboxes", probe.monster_state_bounds, 0x03),
        ("Soldier and Dog acquired player", probe.target_edges, 0x03),
        ("Soldier and Dog moved", probe.monster_animated, 0x03),
        ("Soldier and Dog damaged player", probe.monster_attack, 0x03),
        ("monster pain", probe.monster_pain, 1),
        ("monster death", probe.monster_death, 1),
        ("monster gib", probe.boss, 1),
        ("player death", probe.player_state, 1),
        ("state ranges", probe.state_ranges, 3),
        ("valid state ranges", probe.valid_state_ranges, 3),
    ] {
        if actual != expected {
            return Err(format!("{label}: 0x{actual:03x}, expected 0x{expected:03x}").into());
        }
    }
    if probe.complete != 1
        || probe.phase != 0x80
        || probe.current_map != 1
        || probe.map_loads != 1
        || probe.route_index != 2
        || probe.last_health != 0
    {
        return Err(format!(
            "monster route incomplete: complete={} phase={} map={} loads={} route={} health={}",
            probe.complete,
            probe.phase,
            probe.current_map,
            probe.map_loads,
            probe.route_index,
            probe.last_health,
        )
        .into());
    }
    Ok(())
}

fn validate_arsenal_probe(probe: &Probe) -> Result<()> {
    const ROUTE_MAP_BITS: u32 = 0x3e;
    const ROUTE_TRANSITION_BITS: u32 = 0x3c;
    const E1M5_BIT: u32 = 1 << 5;
    const WEAPONS: u32 = 0x7e;
    const PICKUPS: u32 = 0x7c;
    const PROJECTILES: u32 = 0x78;
    const EXPLOSION_PRESENTATION_BIT: u32 = 1 << 31;
    if probe.version != 5 {
        return Err(format!("unsupported probe version {}", probe.version).into());
    }
    if probe.failure_code != 0 {
        return Err(format!(
            "arsenal regression failed: code={} map={} entity={} detail=0x{:08x} phase={} frames={}",
            probe.failure_code,
            probe.failure_map,
            probe.failure_entity,
            probe.failure_detail,
            probe.phase,
            probe.stage_frames,
        )
        .into());
    }
    for (label, actual, expected) in [
        ("maps loaded", probe.maps_loaded, ROUTE_MAP_BITS),
        ("maps validated", probe.maps_validated, ROUTE_MAP_BITS),
        ("transitions", probe.transitions, ROUTE_TRANSITION_BITS),
        ("weapon pickups", probe.weapon_pickups, PICKUPS),
        ("weapons selected", probe.weapon_selected, WEAPONS),
        ("weapons fired", probe.weapon_fired, WEAPONS),
        ("weapons animated", probe.weapon_animated, WEAPONS),
        ("projectile models", probe.shock_count, PROJECTILES),
        ("monster present", probe.monster_present, E1M5_BIT),
        ("monster hitbox", probe.monster_state_bounds, E1M5_BIT),
        ("monster pain", probe.monster_pain, E1M5_BIT),
        ("lightning wall trace", probe.monster_attack, 0x0f),
        ("nail impact and damage", probe.monster_death, 0x03),
        ("grenade physics and fuse", probe.monster_animated, 0x0f),
        ("nail pool admission", probe.boss, (1 << 16) | 60),
        ("rocket world model", probe.state_ranges, 0x40),
    ] {
        if actual != expected {
            return Err(format!("{label}: 0x{actual:03x}, expected 0x{expected:03x}").into());
        }
    }
    let self_damage = probe.player_state >> 16;
    let player_health = probe.player_state & 0xffff;
    if probe.complete != 1
        || probe.phase != 0x70
        || probe.current_map != 5
        || probe.map_loads != 5
        || probe.target_edges & 0xffff == 0
        || probe.target_edges & EXPLOSION_PRESENTATION_BIT == 0
        || probe.valid_state_ranges == 0
        || probe.intermission_state == 0
        || self_damage == 0
        || player_health >= 100
    {
        return Err(format!(
            "arsenal route incomplete: complete={} phase={} map={} loads={} rocket_packets={} impacts={} explosion_flash={} self_damage={} player_health={} target_health={}",
            probe.complete,
            probe.phase,
            probe.current_map,
            probe.map_loads,
            probe.valid_state_ranges,
            probe.target_edges & 0xffff,
            (probe.target_edges >> 31) & 1,
            self_damage,
            player_health,
            probe.last_health,
        )
        .into());
    }
    Ok(())
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Last `route_tick` in the route log: the run's own emulated-time length in
/// the same units the scripted presses are scheduled in.
fn final_route_tick(route: &Path) -> Result<u64> {
    let csv = fs::read_to_string(route)?;
    let mut lines = csv.lines();
    let header = lines.next().ok_or("route log has no header")?;
    let index = header
        .split(',')
        .position(|name| name == "route_tick")
        .ok_or("route log lacks route_tick")?;
    let last = lines.last().ok_or("route log has no samples")?;
    Ok(last
        .split(',')
        .nth(index)
        .ok_or("last route row is short")?
        .parse()?)
}

fn final_port1_polls(route: &Path) -> Result<u64> {
    let csv = fs::read_to_string(route)?;
    let mut lines = csv.lines();
    let header = lines.next().ok_or("route log has no header")?;
    let index = header
        .split(',')
        .position(|name| name == "port1_polls")
        .ok_or("route log lacks port1_polls")?;
    let last = lines.last().ok_or("route log has no samples")?;
    Ok(last
        .split(',')
        .nth(index)
        .ok_or("last route row is short")?
        .parse()?)
}

fn audit_sources(root: &Path) -> Result<()> {
    let mut native_sources = Vec::new();
    audit_native_sources(root, root, &mut native_sources)?;
    if !native_sources.is_empty() {
        return Err(format!(
            "Rust-only source gate found native implementation files:\n{}",
            native_sources.join("\n")
        )
        .into());
    }
    let mut foreign_bindings = Vec::new();
    audit_foreign_bindings(root, root, &mut foreign_bindings)?;
    if !foreign_bindings.is_empty() {
        return Err(format!(
            "Rust-only source gate found foreign compatibility bindings:\n{}",
            foreign_bindings.join("\n")
        )
        .into());
    }
    let forbidden = [
        ["ps", "n00b"].concat(),
        ["ps", "noob"].concat(),
        ["mk", "psxiso"].concat(),
        ["elf", "2x"].concat(),
        ["lib", "psn"].concat(),
    ];
    let mut hits = Vec::new();
    audit_directory(root, root, &forbidden, &mut hits)?;
    if !hits.is_empty() {
        return Err(format!("non-PSoXide SDK references remain:\n{}", hits.join("\n")).into());
    }
    Ok(())
}

fn audit_foreign_bindings(root: &Path, path: &Path, hits: &mut Vec<String>) -> Result<()> {
    let abi = ["extern ", "\"C\""].concat();
    let ffi = ["core::", "ffi::", "c_"].concat();
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let relative = child.strip_prefix(root)?;
        let top = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .unwrap_or("");
        if child.is_dir() {
            let name = child
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name != "target" && !audit_ignored_top(top) {
                audit_foreign_bindings(root, &child, hits)?;
            }
        } else if child.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            let source = fs::read_to_string(&child)?;
            if source.contains(&abi) || source.contains(&ffi) {
                hits.push(relative.display().to_string());
            }
        }
    }
    Ok(())
}

fn audit_native_sources(root: &Path, path: &Path, hits: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let relative = child.strip_prefix(root)?;
        let top = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .unwrap_or("");
        if child.is_dir() {
            let name = child
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == "target" || audit_ignored_top(top) {
                continue;
            }
            audit_native_sources(root, &child, hits)?;
        } else if child.is_file()
            && child
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "c" | "h" | "cc" | "cpp" | "cxx" | "m" | "mm" | "s" | "asm"
                    )
                })
        {
            hits.push(relative.display().to_string());
        }
    }
    Ok(())
}

fn audit_ignored_top(top: &str) -> bool {
    matches!(
        top,
        ".git"
            | ".psoxide"
            | ".quakepsx"
            | "target"
            | "build"
            | "build-regression"
            | "build-psoxide"
            | "build-psoxide-ship-boot"
            | "build-psoxide-ambient-regression"
            | "build-psoxide-audio-regression"
            | "build-psoxide-arsenal-regression"
            | "build-psoxide-combat-regression"
            | "build-psoxide-monster-regression"
            | "build-psoxide-monsterjump-regression"
            | "build-psoxide-bestiary-regression"
            | "build-psoxide-episode1-regression"
            | "build-psoxide-map-regression"
            | "build-psoxide-start-route-regression"
            | "build-psoxide-e1m1-chain-regression"
            | "build-psoxide-e1m1-selection-cache-bench"
            | "build-psoxide-e1m1-topology-cache-bench"
            | "build-psoxide-e1m1-indexed-projection-bench"
            | "build-psoxide-e1m1-aabb-offsets-bench"
            | "build-psoxide-e1m1-shared-subdivision-edges-bench"
            | "build-psoxide-e1m1-level0-fast-path-bench"
            | "build-psoxide-e1m1-speculative-level0-bench"
            | "build-psoxide-e1m1-depth-only-subdivision-bench"
            | "build-psoxide-e1m1-gte-otz-bench"
            | "build-psoxide-e1m1-compact-subdivision-emitters-bench"
            | "build-psoxide-e1m1-compact-subdivision-kernels-bench"
            | "build-psoxide-e1m1-compact-level2-kernel-bench"
            | "build-psoxide-e1m1-compact-world-level2-kernel-bench"
            | "build-psoxide-e1m1-gpu-lattice-clip-bench"
            | "build-psoxide-e1m1-gpu-polygon-clip-bench"
            | "build-psoxide-e1m1-gpu-polygon-depth-only-bench"
            | "build-psoxide-e1m1-gpu-polygon-compact-ot-bench"
            | "build-psoxide-e1m1-gpu-polygon-fused-projection-bench"
            | "build-psoxide-e1m1-gpu-polygon-plane-index-bench"
            | "build-psoxide-e1m1-gpu-polygon-window-runs-bench"
            | "build-psoxide-e1m1-gpu-polygon-window-insert-bench"
            | "build-psoxide-e1m1-gpu-polygon-window-range-bench"
            | "build-psoxide-e1m1-gpu-polygon-cell-stream-bench"
            | "build-psoxide-e1m1-gpu-polygon-cell-policy-bench"
            | "build-psoxide-gpu-polygon-cell-policy-playable"
            | "build-psoxide-e1m1-gpu-polygon-quake-kernel-bench"
            | "build-psoxide-e1m1-gpu-polygon-level0-run-bench"
            | "build-psoxide-e1m1-gpu-polygon-cold-adaptive-bench"
            | "build-psoxide-e1m1-gpu-polygon-cold-level2-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-stream-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-level2-stream-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-level2-scatter-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-level2-cold-cache-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-base-cache-bench"
            | "build-psoxide-e1m1-gpu-polygon-resident-base-cache-fast-bench"
            | "build-psoxide-e1m1-gpu-surface-clip-bench"
            | "build-psoxide-gpu-polygon-clip-visual-regression"
            | "build-psoxide-gpu-polygon-clip-route-regression"
            | "build-psoxide-gpu-polygon-clip-ship-boot"
            | "build-psoxide-e1m1-gpu-polygon-census"
            | "build-psoxide-e1m1-static-world-reuse-bench"
            | "build-psoxide-e1m1-hoisted-indexed-world-bench"
            | "build-psoxide-e1m1-fixed-fan-quads-bench"
            | "build-psoxide-e1m1-fixed-fan-guarded-bench"
            | "build-psoxide-e1m1-fixed-fan-level2-bench"
            | "build-psoxide-e1m1-subdivision-cache-bench"
            | "build-psoxide-e1m1-subdivision-cache-level2-bench"
            | "build-psoxide-e1m1-subdivision-cache-level2-small-bench"
            | "build-psoxide-e1m1-subdivision-cache-level2-layout-control-bench"
            | "build-psoxide-e1m1-hierarchical-block-frustum-bench"
            | "build-psoxide-selection-cache-visual-regression"
            | "build-psoxide-selection-cache-route-regression"
            | "build-psoxide-selection-cache-ship-boot"
            | "build-psoxide-e1m1-renderer-census"
            | "build-psoxide-e1m2-e1m3-route-regression"
            | "build-psoxide-survival-regression"
            | "build-psoxide-systems-regression"
            | "build-psoxide-regression"
            | "build-psoxide-hardware"
            | "dist"
            | "captures"
            | "graphify-out"
    )
}

fn audit_directory(
    root: &Path,
    path: &Path,
    forbidden: &[String],
    hits: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let relative = child.strip_prefix(root)?;
        let top = relative
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("");
        if child.is_dir() {
            let name = child
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == "target" || audit_ignored_top(top) {
                continue;
            }
            audit_directory(root, &child, forbidden, hits)?;
        } else if child.is_file() {
            let data = fs::read(&child)?;
            if let Ok(text) = std::str::from_utf8(&data) {
                let lower = text.to_ascii_lowercase();
                for token in forbidden {
                    if lower.contains(token) {
                        hits.push(format!("{}: {token}", relative.display()));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_hash(path: &Path, expected: &str, label: &str) -> Result<()> {
    let actual = sha256_path(path)?;
    if actual != expected {
        return Err(format!("{label} checksum mismatch: expected {expected}, got {actual}").into());
    }
    Ok(())
}

fn sha256_path(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn executable_name(name: &str) -> OsString {
    if cfg!(windows) {
        format!("{name}.exe").into()
    } else {
        name.into()
    }
}

fn require_tool(names: &[&str]) -> Result<PathBuf> {
    find_runnable(names)
        .ok_or_else(|| format!("required tool not found: {}", names.join(" or ")).into())
}

fn find_runnable(names: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(executable_name(name));
            if is_runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_runnable(path: &Path) -> bool {
    path.is_file()
        && Command::new(path)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
}

fn run(command: &mut Command) -> Result<()> {
    print_command(command);
    let status = command.status()?;
    if !status.success() {
        return Err(format!("command failed with {status}: {command:?}").into());
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<Output> {
    print_command(command);
    let result = command.output()?;
    if !result.status.success() {
        return Err(format!(
            "command failed with {}: {command:?}\n{}",
            result.status,
            String::from_utf8_lossy(&combined_output(&result))
        )
        .into());
    }
    Ok(result)
}

fn combined_output(output: &Output) -> Vec<u8> {
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    combined
}

fn print_command(command: &Command) {
    eprintln!("+ {command:?}");
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four real rows of a rust-lld map: the header, the input-section row
    /// that names the object file, and the two symbol rows the shipping boot
    /// gate reads.
    const SHIP_BOOT_MAP: &str = "     VMA      LMA     Size Align Out     In      Symbol\n\
         800670f4 800670f4        8     4         /tmp/quake-psx-guest-v1/deadbeef/game/target/mipsel-sony-psx/release/deps/quake_psx-3ec8bffd0f09f945.quake_psx-cgu.0.rcgu.o:(.text._RNvCs7z83p4GUNVw_6psx_rt4halt)\n\
         800670f4 800670f4        8     1                 psx_rt::halt\n\
         800ec54c 800ec54c        4     1                 psx_rt::heap::ALLOCATOR (.0)\n\
         800ec560 800ec560        0     1 __heap_start = .\n";

    #[test]
    fn link_map_reads_the_symbol_row_and_never_its_object_file() {
        // The object-file row shares `psx_rt::halt`'s address and would match
        // any substring search, so the whole-name rule is the contract here.
        assert_eq!(
            link_map_symbol(SHIP_BOOT_MAP, "psx_rt::halt").unwrap(),
            LinkMapSymbol {
                address: 0x8006_70f4,
                size: 8,
            }
        );
        assert_eq!(
            link_map_symbol(SHIP_BOOT_MAP, "psx_rt::heap::ALLOCATOR (.0)")
                .unwrap()
                .address,
            0x800e_c54c
        );
        assert_eq!(
            link_map_symbol(SHIP_BOOT_MAP, "__heap_start = .")
                .unwrap()
                .address,
            0x800e_c560
        );
        let error = link_map_symbol(SHIP_BOOT_MAP, "psx_rt::heap::ALLOCATOR")
            .unwrap_err()
            .to_string();
        assert!(error.contains("no symbol"), "{error}");
    }

    #[test]
    fn ram_word_masks_the_segment_bits_of_a_kseg0_address() {
        let mut ram = vec![0_u8; 0x0020_0000];
        ram[0x000e_c54c..0x000e_c550].copy_from_slice(&0x801f_6ad4_u32.to_le_bytes());
        assert_eq!(ram_word(&ram, 0x800e_c54c).unwrap(), 0x801f_6ad4);
        // Same physical word through KUSEG and KSEG1.
        assert_eq!(ram_word(&ram, 0x000e_c54c).unwrap(), 0x801f_6ad4);
        assert_eq!(ram_word(&ram, 0xa00e_c54c).unwrap(), 0x801f_6ad4);
        let error = ram_word(&ram[..16], 0x800e_c54c).unwrap_err().to_string();
        assert!(error.contains("too short"), "{error}");
    }

    #[test]
    fn frontend_field_reads_both_summary_lines() {
        let log = "[cli] mounted cue-backed disc quake-psx.cue\n\
                   tick=310000000  cycles=470799882  pc=0x800670f8\n\
                   route-ticks=669  port1-polls=152\n";
        assert_eq!(frontend_field(log, "pc=0x").unwrap(), "800670f8");
        assert_eq!(frontend_field(log, "route-ticks=").unwrap(), "669");
        assert_eq!(frontend_field(log, "port1-polls=").unwrap(), "152");
        let error = frontend_field(log, "guest-frames=")
            .unwrap_err()
            .to_string();
        assert!(error.contains("guest-frames="), "{error}");
    }

    #[test]
    fn initial_load_budget_ignores_the_early_loading_flip() {
        let route = "route_tick,tape_frame,cpu_tick,bus_cycles,cpu_tick_delta,bus_cycle_delta,display_x,display_y,display_width,display_height,display_start_changed,port1_polls\n\
                     0,0,1,100,0,0,0,0,320,0,0,0\n\
                     1,0,2,200,0,0,0,256,320,240,1,0\n\
                     2,0,3,500,0,0,0,0,320,240,1,1\n\
                     3,0,4,700,0,0,0,256,320,240,1,2\n";
        let cd = "cycle,command,param_len,params\n\
                  150,0x06,0,\n\
                  175,0x09,0,\n\
                  250,0x06,0,\n\
                  450,0x09,0,\n\
                  550,0x06,0,\n";
        assert_eq!(initial_load_metrics(route, cd).unwrap(), (400, 2));
    }

    #[test]
    fn initial_load_budget_rejects_a_vacuous_loading_only_capture() {
        let route = "route_tick,tape_frame,cpu_tick,bus_cycles,cpu_tick_delta,bus_cycle_delta,display_x,display_y,display_width,display_height,display_start_changed,port1_polls\n\
                     0,0,1,100,0,0,0,0,320,0,0,0\n\
                     1,0,2,200,0,0,0,256,320,240,1,0\n";
        let cd = "cycle,command,param_len,params\n150,0x06,0,\n175,0x09,0,\n";
        let error = initial_load_metrics(route, cd).unwrap_err().to_string();
        assert!(error.contains("controller polling began"), "{error}");
    }

    #[test]
    fn full_level_render_window_excludes_loading_and_next_map() {
        let route = "route_tick,tape_frame,cpu_tick,bus_cycles,cpu_tick_delta,bus_cycle_delta,display_x,display_y,display_width,display_height,display_start_changed,port1_polls\n\
                     0,0,1,100,0,0,0,0,320,0,0,0\n\
                     1,0,2,200,0,0,0,256,320,240,1,0\n\
                     2,0,3,500,0,0,0,0,320,240,1,1\n\
                     3,0,4,600,0,0,0,256,320,240,1,2\n\
                     4,0,5,700,0,0,0,0,320,240,1,3\n\
                     5,0,6,900,0,0,0,256,320,240,1,4\n";
        let cd = "cycle,command,param_len,params\n\
                  150,0x06,0,\n\
                  400,0x06,0,\n\
                  450,0x09,0,\n\
                  800,0x06,0,\n";
        assert_eq!(
            full_level_render_metrics(route, cd).unwrap(),
            FullLevelRenderMetrics {
                presentations: 3,
                elapsed_bus_cycles: 200,
                fps_x1000: 338_688_000,
            }
        );
    }

    #[test]
    fn same_map_respawn_budget_starts_after_cold_level_publication() {
        let route = "route_tick,tape_frame,cpu_tick,bus_cycles,cpu_tick_delta,bus_cycle_delta,display_x,display_y,display_width,display_height,display_start_changed,port1_polls\n\
                     0,0,1,100,0,0,0,0,320,0,0,0\n\
                     1,0,2,200,0,0,0,256,320,240,1,0\n\
                     2,0,3,300,0,0,0,0,320,240,1,1\n\
                     3,0,4,700,0,0,0,256,320,240,1,2\n";
        let telemetry = "[guest f2 c500] quake-psx: audio-local source-bytes=0x00000100 upload-bytes=0x000000f0 sessions=0x00000001 hit=0\n";
        let boundary = cold_local_audio_publication_cycle(telemetry).unwrap();
        assert_eq!(boundary, 500);
        assert_eq!(
            require_display_flip_after_cycle(route, boundary).unwrap(),
            700
        );
        let cached = "cycle,command,param_len,params\n\
                      150,0x06,0,\n\
                      450,0x09,0,\n";
        assert_eq!(readn_sessions_after_cycle(cached, boundary).unwrap(), 0);

        let reloaded = "cycle,command,param_len,params\n\
                        150,0x06,0,\n\
                        450,0x09,0,\n\
                        650,0x06,0,\n";
        assert_eq!(readn_sessions_after_cycle(reloaded, boundary).unwrap(), 1);
    }

    fn complete_combat_probe() -> Probe {
        Probe {
            version: 3,
            complete: 1,
            phase: 0x50,
            maps_loaded: 1 << 1,
            maps_validated: 1 << 1,
            weapon_selected: 1 << 1,
            weapon_fired: 1 << 1,
            weapon_animated: 1 << 1,
            monster_present: 1 << 1,
            monster_state_bounds: 1 << 1,
            monster_pain: 1 << 1,
            monster_death: 1 << 1,
            transitions: 31,
            boss: 150,
            target_edges: 1,
            current_map: 1,
            map_loads: 1,
            ..Probe::default()
        }
    }

    fn complete_monster_probe() -> Probe {
        Probe {
            version: 6,
            complete: 1,
            phase: 0x80,
            maps_loaded: 1 << 1,
            maps_validated: 1 << 1,
            monster_present: 0x03,
            monster_animated: 0x03,
            monster_state_bounds: 0x03,
            monster_attack: 0x03,
            monster_pain: 1,
            monster_death: 1,
            boss: 1,
            current_map: 1,
            route_index: 2,
            state_ranges: 3,
            valid_state_ranges: 3,
            map_loads: 1,
            player_state: 1,
            target_edges: 0x03,
            ..Probe::default()
        }
    }

    fn complete_monsterjump_probe() -> Probe {
        Probe {
            version: 13,
            complete: 1,
            phase: 0xb1,
            maps_loaded: 1 << 6,
            maps_validated: 1 << 6,
            transitions: 192,
            monster_present: 1,
            monster_animated: 0x07,
            route_index: 81,
            current_map: 6,
            valid_state_ranges: 0x07,
            map_loads: 1,
            player_state: 0x07,
            ..Probe::default()
        }
    }

    fn complete_episode1_probe() -> Probe {
        Probe {
            version: 13,
            complete: 1,
            phase: 0xe1,
            maps_loaded: (1 << 7) | 1,
            current_map: 0,
            map_loads: 2,
            transitions: 1,
            // 95, not 100: the shaft drop costs Quake's own five points of
            // fall damage, which is the descent's fingerprint on the probe.
            last_health: 95,
            player_state: 0x3ffff,
            valid_state_ranges: 0x3ffff,
            boss: 20,
            monster_present: 1,
            shock_count: 6,
            ..Probe::default()
        }
    }

    #[test]
    fn the_episode_probe_requires_every_link_of_the_completion_contract() {
        validate_episode1_probe(&complete_episode1_probe()).expect("complete episode proof");
        for (label, bit) in EPISODE1_STATE {
            let probe = Probe {
                player_state: complete_episode1_probe().player_state & !bit,
                ..complete_episode1_probe()
            };
            let error = validate_episode1_probe(&probe)
                .expect_err("a missing link must fail")
                .to_string();
            assert!(error.contains(label), "{label}: {error}");
        }
    }

    #[test]
    fn the_episode_probe_rejects_a_stale_version_a_dead_player_and_a_short_route() {
        for probe in [
            Probe {
                version: 12,
                ..complete_episode1_probe()
            },
            Probe {
                last_health: 0,
                ..complete_episode1_probe()
            },
            Probe {
                maps_loaded: 1 << 7,
                ..complete_episode1_probe()
            },
            Probe {
                transitions: 0,
                ..complete_episode1_probe()
            },
            Probe {
                complete: 0,
                ..complete_episode1_probe()
            },
            Probe {
                failure_code: 4,
                ..complete_episode1_probe()
            },
        ] {
            validate_episode1_probe(&probe).expect_err("an incomplete episode must fail");
        }
    }

    fn complete_bestiary_probe() -> Probe {
        Probe {
            version: 10,
            complete: 1,
            phase: 0xaf,
            maps_loaded: (1 << 2) | (1 << 4),
            maps_validated: (1 << 2) | (1 << 4),
            monster_present: 0b11,
            monster_attack: 0b11,
            monster_pain: 0b11,
            monster_death: 0b11,
            current_map: 4,
            map_loads: 2,
            player_state: 0b11,
            valid_state_ranges: 0b11,
            failure_entity: 42,
            target_edges: 42,
            ..Probe::default()
        }
    }

    fn complete_arsenal_probe() -> Probe {
        Probe {
            version: 5,
            complete: 1,
            phase: 0x70,
            maps_loaded: 0x3e,
            maps_validated: 0x3e,
            transitions: 0x3c,
            weapon_selected: 0x7e,
            weapon_fired: 0x7e,
            weapon_animated: 0x7e,
            monster_present: 1 << 5,
            monster_state_bounds: 1 << 5,
            monster_pain: 1 << 5,
            monster_attack: 0x0f,
            monster_death: 0x03,
            monster_animated: 0x0f,
            boss: (1 << 16) | 60,
            current_map: 5,
            map_loads: 5,
            player_state: (20 << 16) | 80,
            weapon_pickups: 0x7c,
            target_edges: (1 << 31) | 1,
            state_ranges: 0x40,
            valid_state_ranges: 12,
            shock_count: 0x78,
            intermission_state: 24,
            last_health: 76,
            ..Probe::default()
        }
    }

    fn complete_start_route_probe() -> Probe {
        Probe {
            version: 4,
            complete: 1,
            phase: 0x31,
            maps_loaded: 0x003,
            maps_validated: 0x003,
            transitions: 0x001,
            current_map: 1,
            route_index: 1,
            map_loads: 2,
            player_state: 0x07,
            target_edges: 1,
            ..Probe::default()
        }
    }

    fn complete_e1m1_chain_probe() -> Probe {
        Probe {
            version: 9,
            complete: 1,
            phase: 0x51,
            // The route now ends on E1M1's own change-level edge, so the
            // finished state is both maps loaded and E1M2 current.
            maps_loaded: 0x006,
            maps_validated: 0x006,
            current_map: 2,
            route_index: 60,
            map_loads: 2,
            transitions: 1,
            player_state: 0x7fff,
            weapon_selected: 0x07,
            target_edges: 4,
            ..Probe::default()
        }
    }

    fn complete_e1m2_route_probe() -> Probe {
        Probe {
            version: 14,
            complete: 1,
            phase: 0x23,
            maps_loaded: 0x01c,
            maps_validated: 0x01c,
            current_map: 4,
            map_loads: 3,
            transitions: 2,
            player_state: 0x1fff_ffff,
            weapon_fired: 2,
            weapon_pickups: 2,
            target_edges: 48,
            ..Probe::default()
        }
    }

    fn complete_survival_probe() -> Probe {
        Probe {
            version: 9,
            complete: 1,
            phase: 0x59,
            maps_loaded: 0x002,
            maps_validated: 0x002,
            current_map: 1,
            route_index: 24,
            map_loads: 3,
            transitions: 141,
            monster_state_bounds: 812,
            player_state: SURVIVAL_REQUIRED,
            weapon_selected: 96,
            weapon_fired: 5,
            weapon_animated: 18,
            monster_present: 30,
            monster_animated: 14,
            monster_attack: 2,
            monster_pain: 2,
            monster_death: 100,
            boss: 25,
            shock_count: 0x0e,
            intermission_state: 0x03,
            ..Probe::default()
        }
    }

    #[test]
    fn survival_probe_requires_every_authored_survival_outcome() {
        validate_survival_probe(&complete_survival_probe()).expect("complete survival route");

        for (label, probe) in [
            (
                "no hazard damage",
                Probe {
                    weapon_selected: 0,
                    ..complete_survival_probe()
                },
            ),
            (
                "no fall damage",
                Probe {
                    weapon_fired: 0,
                    ..complete_survival_probe()
                },
            ),
            (
                "no drowning damage",
                Probe {
                    weapon_animated: 0,
                    ..complete_survival_probe()
                },
            ),
            (
                "no deaths",
                Probe {
                    monster_attack: 0,
                    ..complete_survival_probe()
                },
            ),
            (
                "no respawns",
                Probe {
                    monster_pain: 0,
                    ..complete_survival_probe()
                },
            ),
        ] {
            validate_survival_probe(&probe)
                .expect_err(&format!("{label} must fail the survival gate"));
        }
    }

    #[test]
    fn survival_probe_rejects_a_drifted_respawn_loadout() {
        let carried_health = Probe {
            monster_death: 60,
            ..complete_survival_probe()
        };
        let error = validate_survival_probe(&carried_health)
            .expect_err("a respawn must restore SetNewParms health");
        assert!(error.to_string().contains("respawn loadout"));

        let carried_shells = Probe {
            boss: 47,
            ..complete_survival_probe()
        };
        validate_survival_probe(&carried_shells)
            .expect_err("a respawn must restore SetNewParms shells");
    }

    #[test]
    fn survival_probe_rejects_partial_mechanisms_and_wrong_liquids() {
        for missing in [
            SURVIVAL_HAZARD_DAMAGE,
            SURVIVAL_FALL_DAMAGE,
            SURVIVAL_DROWN_DAMAGE,
            SURVIVAL_HAZARD_DEATH,
            SURVIVAL_RESPAWN_LOADOUT,
            SURVIVAL_POWERUP_TAKEN,
            SURVIVAL_POWERUP_HALF_SPENT,
            SURVIVAL_POWERUP_EXPIRED,
        ] {
            let probe = Probe {
                player_state: SURVIVAL_REQUIRED & !missing,
                ..complete_survival_probe()
            };
            let error =
                validate_survival_probe(&probe).expect_err("every survival mechanism is required");
            assert!(error.to_string().contains("mechanisms incomplete"));
        }

        let never_submerged = Probe {
            shock_count: 0x06,
            ..complete_survival_probe()
        };
        validate_survival_probe(&never_submerged).expect_err("the route must reach water level 3");

        let one_liquid = Probe {
            intermission_state: 0x01,
            ..complete_survival_probe()
        };
        validate_survival_probe(&one_liquid).expect_err("the route must enter both liquids");
    }

    #[test]
    fn survival_probe_rejects_drifted_fall_and_powerup_arithmetic() {
        let odd_fall = Probe {
            weapon_fired: 7,
            ..complete_survival_probe()
        };
        validate_survival_probe(&odd_fall).expect_err("fall damage is a multiple of five");

        let long_quad = Probe {
            monster_present: 45,
            ..complete_survival_probe()
        };
        validate_survival_probe(&long_quad).expect_err("the quad arms at thirty seconds");

        let unspent_quad = Probe {
            monster_animated: 22,
            ..complete_survival_probe()
        };
        validate_survival_probe(&unspent_quad).expect_err("the quad must be seen half spent");
    }

    #[test]
    fn survival_probe_rejects_an_unresolved_authored_source() {
        let no_leaf = Probe {
            monster_state_bounds: u32::MAX,
            ..complete_survival_probe()
        };
        validate_survival_probe(&no_leaf).expect_err("the authored hazard leaf is required");

        let no_artifact = Probe {
            transitions: u32::MAX,
            ..complete_survival_probe()
        };
        validate_survival_probe(&no_artifact).expect_err("the authored quad entity is required");

        let wrong_map = Probe {
            current_map: 2,
            ..complete_survival_probe()
        };
        validate_survival_probe(&wrong_map).expect_err("the route may not leave E1M1");

        let unfinished = Probe {
            complete: 0,
            phase: 1,
            ..complete_survival_probe()
        };
        validate_survival_probe(&unfinished).expect_err("an unfinished route must fail");
    }

    #[test]
    fn resolve_frontend_never_borrows_a_sibling_checkout() {
        // The regression this pins: a requested checkout with no built
        // frontend used to fall through to ../PSoXide/target/release/frontend,
        // so gate results were attributed to a revision that never built them.
        let temp = std::env::temp_dir().join(format!(
            "quake-frontend-contract-{}-{}",
            std::process::id(),
            line!()
        ));
        let root = temp.join("quake");
        let requested = temp.join("requested-psoxide");
        let sibling = temp.join("PSoXide/target/release");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&requested).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        // A runnable stand-in where the old fallback used to look.
        let decoy = sibling.join("frontend");
        fs::write(&decoy, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let error = resolve_frontend(&root, Some(requested.as_path()))
            .expect_err("an empty requested checkout must fail closed");
        let error = error.to_string();
        assert!(
            error.contains(&requested.display().to_string()),
            "the failure must name the requested checkout, got {error}"
        );
        assert!(
            !error.contains("PSoXide/target/release/frontend")
                || !decoy.exists()
                || error.contains(&requested.display().to_string()),
            "the sibling checkout must not be adopted, got {error}"
        );

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn resolve_frontend_rejects_an_unrunnable_explicit_binary() {
        let temp = std::env::temp_dir().join(format!(
            "quake-frontend-file-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&temp).unwrap();
        let not_a_frontend = temp.join("frontend");
        fs::write(&not_a_frontend, "not an executable").unwrap();

        let error = resolve_frontend(&temp, Some(not_a_frontend.as_path()))
            .expect_err("a non-runnable explicit frontend must fail closed");
        assert!(error.to_string().contains("not runnable"), "got {error}");

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn parses_probe_without_an_elf_symbol_table() {
        let mut data = [0u8; PROBE_BYTES];
        data[0..4].copy_from_slice(&PROBE_MAGIC.to_le_bytes());
        data[4..8].copy_from_slice(&3u32.to_le_bytes());
        let probe = probe_from_bytes(&data);
        assert_eq!(probe.version, 3);
    }

    #[test]
    fn start_route_probe_requires_real_skill_teleport_and_changelevel_evidence() {
        validate_start_route_probe(&complete_start_route_probe()).expect("complete Start route");

        let missing_teleport = Probe {
            player_state: 0x05,
            ..complete_start_route_probe()
        };
        let error = validate_start_route_probe(&missing_teleport)
            .expect_err("missing teleport evidence must fail")
            .to_string();
        assert!(error.contains("player=0x05"), "{error}");
    }

    #[test]
    fn start_route_probe_surfaces_failure_position_without_an_image() {
        let failed = Probe {
            failure_code: 2,
            failure_map: 0,
            failure_detail: 5,
            last_health: (-71i32) as u32,
            state_ranges: 1_775,
            valid_state_ranges: 126,
            ..complete_start_route_probe()
        };
        let error = validate_start_route_probe(&failed)
            .expect_err("guest failure must fail")
            .to_string();
        assert!(error.contains("position=(-71, 1775, 126)"), "{error}");
    }

    #[test]
    fn e1m1_chain_probe_requires_messages_buttons_counter_doors_and_crossing() {
        validate_e1m1_chain_probe(&complete_e1m1_chain_probe()).expect("complete E1M1 chain");

        let missing_button_sound = Probe {
            weapon_selected: 0x06,
            ..complete_e1m1_chain_probe()
        };
        let error = validate_e1m1_chain_probe(&missing_button_sound)
            .expect_err("missing mover sound must fail")
            .to_string();
        assert!(error.contains("mover_sounds=0x06"), "{error}");

        let messages_not_armed = Probe {
            player_state: 0x7ffe,
            ..complete_e1m1_chain_probe()
        };
        let error = validate_e1m1_chain_probe(&messages_not_armed)
            .expect_err("targeted-door messages must begin armed")
            .to_string();
        assert!(error.contains("mechanisms=0x7ffe"), "{error}");

        let stale_t15_message = Probe {
            player_state: 0x3fff,
            ..complete_e1m1_chain_probe()
        };
        let error = validate_e1m1_chain_probe(&stale_t15_message)
            .expect_err("t15 door message must disarm when the door fires")
            .to_string();
        assert!(error.contains("mechanisms=0x3fff"), "{error}");

        let never_left_e1m1 = Probe {
            player_state: 0x1fff,
            maps_loaded: 0x002,
            maps_validated: 0x002,
            current_map: 1,
            map_loads: 1,
            transitions: 0,
            ..complete_e1m1_chain_probe()
        };
        let error = validate_e1m1_chain_probe(&never_left_e1m1)
            .expect_err("stopping at the exit door must fail")
            .to_string();
        assert!(error.contains("mechanisms=0x1fff"), "{error}");
    }

    #[test]
    fn e1m1_chain_probe_rejects_a_partial_route() {
        let partial_route = Probe {
            route_index: 59,
            ..complete_e1m1_chain_probe()
        };
        let error = validate_e1m1_chain_probe(&partial_route)
            .expect_err("unfinished waypoint list must fail")
            .to_string();
        assert!(error.contains("waypoint=59"), "{error}");
    }

    #[test]
    fn e1m2_route_probe_requires_every_authored_mechanism_and_transition() {
        validate_e1m2_route_probe(&complete_e1m2_route_probe()).expect("complete E1M2/E1M3 route");

        for missing in 0..29 {
            let probe = Probe {
                player_state: 0x1fff_ffff & !(1 << missing),
                ..complete_e1m2_route_probe()
            };
            validate_e1m2_route_probe(&probe)
                .expect_err("every E1M2/E1M3 mechanism must be observed");
        }

        let stopped_at_exit = Probe {
            complete: 0,
            phase: 3,
            maps_loaded: 0x00c,
            maps_validated: 0x00c,
            current_map: 3,
            map_loads: 2,
            transitions: 1,
            player_state: 0x1fff_ffff & !(1 << 27),
            ..complete_e1m2_route_probe()
        };
        validate_e1m2_route_probe(&stopped_at_exit)
            .expect_err("reaching the E1M3 exit without loading E1M4 must fail");
    }

    #[test]
    fn e1m2_route_probe_surfaces_guest_failure_and_rejects_extra_shots() {
        let failed = Probe {
            failure_code: 5,
            failure_map: 2,
            failure_entity: 243,
            last_health: 1504,
            state_ranges: (-456i32) as u32,
            valid_state_ranges: 313,
            ..complete_e1m2_route_probe()
        };
        let error = validate_e1m2_route_probe(&failed)
            .expect_err("guest failure must fail")
            .to_string();
        assert!(error.contains("entity=243"), "{error}");
        assert!(error.contains("position=(1504, -456, 313)"), "{error}");

        let repeated_shot = Probe {
            weapon_fired: 3,
            ..complete_e1m2_route_probe()
        };
        validate_e1m2_route_probe(&repeated_shot)
            .expect_err("both wait=-1 buttons must need exactly one weapon activation");
    }

    #[test]
    fn e1m1_monster_source_assertion_pins_population_and_probe_entities() {
        let mut bytes = vec![0u8; 140 * MapEntity::SIZE];
        let army_indices = (2..35).chain(core::iter::once(40)).collect::<Vec<_>>();
        for (position, index) in army_indices.iter().copied().enumerate() {
            let record = &mut bytes[index * MapEntity::SIZE..(index + 1) * MapEntity::SIZE];
            record[0] = 0x36;
            let flags = if position < 8 { 0u16 } else { 0x0100 };
            record[2..4].copy_from_slice(&flags.to_le_bytes());
        }
        for index in [82usize, 90, 96, 103, 110, 117, 121, 123] {
            let record = &mut bytes[index * MapEntity::SIZE..(index + 1) * MapEntity::SIZE];
            record[0] = 0x39;
            let flags = if index == 82 { 0u16 } else { 0x0100 };
            record[2..4].copy_from_slice(&flags.to_le_bytes());
        }
        bytes[21 * MapEntity::SIZE] = 0x36;
        bytes[21 * MapEntity::SIZE + 2..21 * MapEntity::SIZE + 4]
            .copy_from_slice(&0u16.to_le_bytes());
        for (index, origin) in [(21usize, [248i32, 2_392, 40]), (82, [88i32, 1_520, -200])] {
            for (axis, value) in origin.into_iter().enumerate() {
                let offset = index * MapEntity::SIZE + 38 + axis * 4;
                bytes[offset..offset + 4].copy_from_slice(&(value << 12).to_le_bytes());
            }
        }
        validate_e1m1_monster_sources(&bytes).expect("canonical source layout passes");
        bytes[82 * MapEntity::SIZE] = 0x36;
        assert!(validate_e1m1_monster_sources(&bytes).is_err());
    }

    #[test]
    fn parses_and_measures_pcm16_stereo_windows() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&52u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&2u32.to_le_bytes());
        wav.extend_from_slice(&8u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16u32.to_le_bytes());
        for sample in [0i16, 0, 0, 0, 900, -1_200, 0, 0] {
            wav.extend_from_slice(&sample.to_le_bytes());
        }

        let capture = parse_wav_pcm16_stereo(&wav).unwrap();
        assert_eq!(capture.sample_rate, 2);
        assert_eq!(
            capture.window_stats(0, 2, 512).unwrap(),
            AudioWindowStats {
                peak: 1_200,
                active_frames: 1,
            }
        );
    }

    #[test]
    fn combat_probe_requires_every_observed_contract_bit() {
        let probe = complete_combat_probe();
        validate_combat_probe(&probe).expect("complete combat proof");

        let missing_pain = Probe {
            monster_pain: 0,
            ..complete_combat_probe()
        };
        let error = validate_combat_probe(&missing_pain)
            .expect_err("missing nonlethal hit must fail")
            .to_string();
        assert!(error.contains("monster pain"), "{error}");
    }

    #[test]
    fn combat_probe_surfaces_guest_failure_context() {
        let failed = Probe {
            failure_code: 7,
            failure_map: 1,
            failure_entity: 21,
            failure_detail: 0x0004_0012,
            ..complete_combat_probe()
        };
        let error = validate_combat_probe(&failed)
            .expect_err("guest failure must fail")
            .to_string();
        assert!(error.contains("code=7"), "{error}");
        assert!(error.contains("entity=21"), "{error}");
        assert!(error.contains("detail=0x00040012"), "{error}");
    }

    #[test]
    fn monsterjump_probe_requires_trigger_rise_and_landing() {
        validate_monsterjump_probe(&complete_monsterjump_probe())
            .expect("complete monster-jump proof");
        for missing in [0x01, 0x02, 0x04] {
            let probe = Probe {
                monster_animated: 0x07 & !missing,
                ..complete_monsterjump_probe()
            };
            let error = validate_monsterjump_probe(&probe)
                .expect_err("each monster-jump phase is required")
                .to_string();
            assert!(error.contains("flight evidence"), "{error}");
        }
    }

    #[test]
    fn monster_probe_requires_acquisition_motion_damage_death_and_state_progression() {
        validate_monster_probe(&complete_monster_probe()).expect("complete monster proof");

        for (label, probe) in [
            (
                "Soldier and Dog acquired player",
                Probe {
                    target_edges: 1,
                    ..complete_monster_probe()
                },
            ),
            (
                "Soldier and Dog moved",
                Probe {
                    monster_animated: 2,
                    ..complete_monster_probe()
                },
            ),
            (
                "Soldier and Dog damaged player",
                Probe {
                    monster_attack: 1,
                    ..complete_monster_probe()
                },
            ),
            (
                "player death",
                Probe {
                    player_state: 0,
                    ..complete_monster_probe()
                },
            ),
            (
                "monster gib",
                Probe {
                    boss: 0,
                    ..complete_monster_probe()
                },
            ),
        ] {
            let error = validate_monster_probe(&probe)
                .expect_err("missing monster runtime evidence must fail")
                .to_string();
            assert!(error.contains(label), "{error}");
        }
    }

    #[test]
    fn bestiary_probe_requires_the_whole_authored_fight_contract() {
        validate_bestiary_probe(&complete_bestiary_probe()).expect("complete bestiary proof");

        for (label, probe) in [
            (
                "maps loaded",
                Probe {
                    maps_loaded: 1 << 1,
                    ..complete_bestiary_probe()
                },
            ),
            (
                "authored monsters present",
                Probe {
                    monster_present: 0b01,
                    ..complete_bestiary_probe()
                },
            ),
            (
                "monsters attacked",
                Probe {
                    monster_attack: 0b01,
                    ..complete_bestiary_probe()
                },
            ),
            (
                "monsters took pain",
                Probe {
                    monster_pain: 0b10,
                    ..complete_bestiary_probe()
                },
            ),
            (
                "monsters died",
                Probe {
                    monster_death: 0b10,
                    ..complete_bestiary_probe()
                },
            ),
        ] {
            let error = validate_bestiary_probe(&probe)
                .expect_err("missing bestiary evidence must fail")
                .to_string();
            assert!(error.contains(label), "{error}");
        }
    }

    #[test]
    fn bestiary_probe_rejects_a_partial_contract_and_missing_body_block() {
        // Every authored stage is load bearing: dropping any one fails.
        for bit in 0..2u32 {
            let probe = Probe {
                player_state: 0b11 & !(1 << bit),
                valid_state_ranges: 0b11 & !(1 << bit),
                ..complete_bestiary_probe()
            };
            let error = validate_bestiary_probe(&probe)
                .expect_err("a partial stage set must fail")
                .to_string();
            assert!(error.contains("contracts"), "bit {bit}: {error}");
        }
        let no_block = Probe {
            target_edges: u32::from(u16::MAX),
            ..complete_bestiary_probe()
        };
        let error = validate_bestiary_probe(&no_block)
            .expect_err("a fight with no body block must fail")
            .to_string();
        assert!(error.contains("body blocking"), "{error}");
    }

    #[test]
    fn bestiary_probe_rejects_a_stale_version_and_surfaces_guest_failure() {
        let stale = Probe {
            version: 6,
            ..complete_bestiary_probe()
        };
        let error = validate_bestiary_probe(&stale)
            .expect_err("a stale probe version must fail")
            .to_string();
        assert!(error.contains("version 6"), "{error}");

        let failed = Probe {
            failure_code: 4,
            failure_map: 2,
            failure_entity: 42,
            failure_detail: 0x0000_0013,
            ..complete_bestiary_probe()
        };
        let error = validate_bestiary_probe(&failed)
            .expect_err("a guest failure must fail")
            .to_string();
        assert!(error.contains("code=4"), "{error}");
        assert!(error.contains("entity=42"), "{error}");

        let unfinished = Probe {
            complete: 0,
            phase: 0xa0,
            ..complete_bestiary_probe()
        };
        let error = validate_bestiary_probe(&unfinished)
            .expect_err("an unfinished stage must fail")
            .to_string();
        assert!(error.contains("did not complete"), "{error}");
    }

    #[test]
    fn arsenal_probe_requires_pickups_runtime_paths_pool_denial_and_self_damage() {
        validate_arsenal_probe(&complete_arsenal_probe()).expect("complete arsenal proof");
        let invisible_rocket = Probe {
            valid_state_ranges: 0,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&invisible_rocket)
            .expect_err("missing rocket alias packets must fail")
            .to_string();
        assert!(error.contains("rocket_packets=0"), "{error}");
        let occluded_self = Probe {
            player_state: 100,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&occluded_self)
            .expect_err("missing visible self-damage evidence must fail")
            .to_string();
        assert!(error.contains("self_damage=0"), "{error}");

        let no_explosion_flash = Probe {
            target_edges: 1,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&no_explosion_flash)
            .expect_err("missing explosion presentation must fail")
            .to_string();
        assert!(error.contains("explosion_flash=0"), "{error}");

        let open_lightning = Probe {
            monster_attack: 0,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&open_lightning)
            .expect_err("missing clipped lightning evidence must fail")
            .to_string();
        assert!(error.contains("lightning wall trace"), "{error}");

        let untested_pool = Probe {
            boss: 0,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&untested_pool)
            .expect_err("missing pool admission evidence must fail")
            .to_string();
        assert!(error.contains("nail pool admission"), "{error}");

        let moving_grenade = Probe {
            monster_animated: 0x0d,
            ..complete_arsenal_probe()
        };
        let error = validate_arsenal_probe(&moving_grenade)
            .expect_err("missing grenade rest evidence must fail")
            .to_string();
        assert!(error.contains("grenade physics and fuse"), "{error}");
    }

    #[test]
    fn parses_frontend_vram_and_display_hash_lines() {
        let log = b"vram_fnv1a_64=0x47eb8bc43677e41c\n\
                    display_fnv1a_64=0xb32e6e4f8f45d057  w=320  h=240\n";
        assert_eq!(
            parse_frontend_hash(log, "vram_fnv1a_64=").unwrap(),
            0x47eb_8bc4_3677_e41c,
        );
        assert_eq!(
            parse_frontend_hash(log, "display_fnv1a_64=").unwrap(),
            0xb32e_6e4f_8f45_d057,
        );
        assert_eq!(
            parse_frontend_display_hash(log).unwrap(),
            DisplayHash {
                hash: 0xb32e_6e4f_8f45_d057,
                width: 320,
                height: 240,
            }
        );
        assert_eq!(require_visible_display(log, "test").unwrap().width, 320);
    }

    #[test]
    fn headless_display_gate_rejects_empty_wrong_sized_and_all_black_frames() {
        assert!(require_visible_display(b"", "test")
            .unwrap_err()
            .to_string()
            .contains("lacks display hash"));
        assert!(
            require_visible_display(b"display_fnv1a_64=0x1234  w=640  h=480\n", "test",)
                .unwrap_err()
                .to_string()
                .contains("expected 320x240")
        );

        let black = fnv1a_zero_bytes(320 * 240 * 2);
        let log = format!("display_fnv1a_64=0x{black:016x}  w=320  h=240\n");
        assert!(require_visible_display(log.as_bytes(), "test")
            .unwrap_err()
            .to_string()
            .contains("all-black"));
    }

    #[test]
    fn visual_probe_requires_real_window_packets_exact_restores_and_no_overflow() {
        let complete = VisualProbe {
            frames: 178,
            packets: 347_056,
            hardware_triangles: 374_864,
            windowed_packets: 10_208,
            window_resets: 10_208,
            reset_failures: 0,
            overflow_frames: 0,
            view_model_packets: 58,
            view_model_registered_packets: 58,
            hud_packets: 13,
            hud_registered_packets: 9,
            crosshair_registered_packets: 4,
            screen_registered_packets: 71,
        };
        validate_visual_probe(&complete).expect("complete packet audit");
        validate_visual_probe(&VisualProbe {
            windowed_packets: complete.windowed_packets + complete.frames * 2,
            window_resets: complete.window_resets + complete.frames,
            ..complete
        })
        .expect("two sky selectors may share one terminal reset per frame");
        for partial in [
            VisualProbe {
                windowed_packets: 0,
                window_resets: 0,
                ..complete
            },
            VisualProbe {
                window_resets: complete.window_resets - 1,
                ..complete
            },
            VisualProbe {
                reset_failures: 1,
                ..complete
            },
            VisualProbe {
                overflow_frames: 1,
                ..complete
            },
            VisualProbe {
                hud_registered_packets: 0,
                ..complete
            },
            VisualProbe {
                view_model_registered_packets: complete.view_model_packets - 1,
                ..complete
            },
            VisualProbe {
                screen_registered_packets: complete.screen_registered_packets - 1,
                ..complete
            },
            VisualProbe {
                packets: VISUAL_MAX_WORLD_PACKETS + 1,
                ..complete
            },
            VisualProbe {
                hardware_triangles: VISUAL_MAX_HARDWARE_TRIANGLES + 1,
                ..complete
            },
            VisualProbe {
                windowed_packets: 12_001,
                window_resets: 12_001,
                ..complete
            },
        ] {
            validate_visual_probe(&partial).expect_err("partial visual proof must fail");
        }
    }

    #[test]
    fn world_and_hud_crops_are_disjoint_and_the_world_pin_is_live() {
        assert_eq!(VISUAL_WORLD_REGION.y + VISUAL_WORLD_REGION.height, 184);
        assert_eq!(VISUAL_HUD_REGION.y, 184);
        assert_eq!(VISUAL_HUD_REGION.y + VISUAL_HUD_REGION.height, 240);
        assert_ne!(EXPECTED_VISUAL_WORLD_FNV1A64, 0);
        assert_ne!(EXPECTED_VISUAL_HUD_FNV1A64, 0);

        let image = PpmImage {
            width: 2,
            height: 2,
            rgb: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let crop = crop_ppm(&image, ImageRegion::new(1, 0, 1, 2)).unwrap();
        assert_eq!(crop.width, 1);
        assert_eq!(crop.height, 2);
        assert_eq!(crop.rgb, [4, 5, 6, 10, 11, 12]);
        assert_ne!(fnv1a64(&crop.rgb), fnv1a64(&image.rgb));
    }

    #[test]
    fn tracked_visual_camera_pins_the_owner_e1m1_coordinates_and_regions() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let text = fs::read_to_string(root.join("tools/visual-parity-cameras.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let camera = &json["cameras"][0];
        assert_eq!(camera["map"], "E1M1");
        assert_eq!(
            camera["origin_q12"],
            serde_json::json!([888798, 3824884, -728959])
        );
        assert_eq!(camera["angles"], serde_json::json!([43, 1088, 0]));
        assert_eq!(camera["world_region"], serde_json::json!([0, 0, 320, 184]));
        assert_eq!(camera["hud_region"], serde_json::json!([0, 184, 320, 56]));
    }
}

#[cfg(test)]
mod source_contract_tests {
    use super::*;

    #[test]
    fn embedded_lockfile_resolves_psoxide_link_to_a_commit() {
        let rev = linked_psoxide_link_rev().expect("lockfile parses");
        assert_eq!(rev.len(), 40);
        assert!(rev.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mismatched_link_revision_fails_closed_with_the_worktree_instruction() {
        let error = default_hydration_plan(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .expect_err("mismatch must refuse default hydration");
        let message = error.to_string();
        assert!(
            message.contains("--psoxide ../PSoXide-rc1-pin"),
            "{message}"
        );
        assert!(message.contains("aaaaaaaa"), "{message}");
        assert!(message.contains("bbbbbbbb"), "{message}");
        assert!(message.contains("mislabeled"), "{message}");
    }

    #[test]
    fn matched_link_revision_hydrates_and_stamps_that_exact_revision() {
        let rev = default_hydration_plan(PSOXIDE_REV, PSOXIDE_REV).expect("match allows");
        assert_eq!(rev, PSOXIDE_REV);
    }

    /// Default hydration follows the psoxide-link revision in Cargo.lock.
    #[test]
    fn live_phase_gate_matches_the_lockfile() {
        let linked = linked_psoxide_link_rev().expect("lockfile parses");
        let plan = default_hydration_plan(&linked, PSOXIDE_REV);
        if linked == PSOXIDE_REV {
            assert_eq!(plan.expect("published phase allows"), PSOXIDE_REV);
        } else {
            let message = plan.expect_err("local-only phase refuses").to_string();
            assert!(
                message.contains("--psoxide ../PSoXide-rc1-pin"),
                "{message}"
            );
        }
    }

    /// No hydration marker can claim a revision other than the one that was
    /// actually hydrated: the pinned stamp text is derived from the carried
    /// revision, which the plan only ever sets to the compiled link revision.
    #[test]
    fn pinned_stamp_text_carries_the_hydrated_revision_only() {
        let source = PsoxideSource::Pinned {
            rev: "cccccccccccccccccccccccccccccccccccccccc".into(),
        };
        let stamp = source.describe();
        assert_eq!(stamp, "pinned cccccccccccccccccccccccccccccccccccccccc");
        assert!(!stamp.contains(PSOXIDE_REV));
    }

    #[test]
    fn lock_parser_rejects_non_git_sources() {
        assert!(parse_psoxide_link_rev(
            "[[package]]\nname = \"psoxide-link\"\nversion = \"0.1.0\"\n"
        )
        .is_err());
        assert!(parse_psoxide_link_rev(
            "[[package]]\nname = \"psoxide-link\"\nsource = \"registry+https://x\"\n"
        )
        .is_err());
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "quake-psx-provenance-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create isolated test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_toolchain() -> ToolchainIdentity {
        ToolchainIdentity {
            rust_toolchain_sha256: "a".repeat(64),
            rustc_version:
                "rustc 1.96.0-nightly (test 2026-03-25)\nhost: test-host\nLLVM version: test".into(),
            cargo_version: "cargo 1.96.0-nightly (test 2026-03-25)\nhost: test-host\nlibgit2: test"
                .into(),
        }
    }

    fn write_guest_recipe_fixture(root: &Path, hydration_source: &str) {
        for directory in [
            "game/src",
            "crates/quake-core/src",
            "crates/quake-formats/src",
            ".psoxide/sdk",
            "tools",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("rust-toolchain.toml"), b"channel = 'pinned'\n").unwrap();
        fs::write(root.join("game/Cargo.toml"), b"[package]\nname='game'\n").unwrap();
        fs::write(root.join("game/Cargo.lock"), b"version = 4\n").unwrap();
        fs::write(root.join("game/src/main.rs"), b"fn main() {}\n").unwrap();
        fs::write(
            root.join("tools/visual-parity-cameras.json"),
            b"{\"schema\":1,\"cameras\":[]}",
        )
        .unwrap();
        fs::write(
            root.join("crates/quake-core/Cargo.toml"),
            b"[package]\nname='quake-core'\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/quake-core/src/lib.rs"),
            b"pub const CORE: u8 = 1;\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/quake-formats/Cargo.toml"),
            b"[package]\nname='quake-formats'\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/quake-formats/src/lib.rs"),
            b"pub const FORMAT: u8 = 1;\n",
        )
        .unwrap();
        fs::write(root.join(".psoxide/sdk/psoxide.ld"), b"SECTIONS {}\n").unwrap();
        for relative in GUEST_RECIPE_PATHS
            .iter()
            .filter(|relative| relative.starts_with(".psoxide/") && !relative.ends_with(".ld"))
        {
            let package = root.join(relative);
            fs::create_dir_all(package.join("src")).unwrap();
            let name = relative.rsplit('/').next().unwrap();
            fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            fs::write(package.join("src/lib.rs"), b"pub const INPUT: u8 = 1;\n").unwrap();
        }
        fs::write(
            root.join(".psoxide/.hydration-stamp"),
            format!("local {hydration_source}\n"),
        )
        .unwrap();
        fs::write(
            root.join(".psoxide/.psoxide-source"),
            format!("local:{hydration_source}"),
        )
        .unwrap();
    }

    fn write_asset_recipe_fixture(root: &Path) {
        for directory in [
            "crates/quake-cook/src",
            "crates/quake-formats/src",
            ".psoxide/engine/crates/psx-bsp/src",
            ".psoxide/engine/crates/psx-render-contract/src",
            "tools/cfg/id1",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for (relative, contents) in [
            ("crates/quake-cook/Cargo.toml", b"cook".as_slice()),
            ("crates/quake-cook/src/lib.rs", b"cook source".as_slice()),
            ("crates/quake-formats/Cargo.toml", b"formats".as_slice()),
            (
                "crates/quake-formats/src/lib.rs",
                b"format source".as_slice(),
            ),
            (
                ".psoxide/engine/crates/psx-bsp/Cargo.toml",
                b"shared bsp".as_slice(),
            ),
            (
                ".psoxide/engine/crates/psx-bsp/src/lib.rs",
                b"wire schema".as_slice(),
            ),
            (
                ".psoxide/engine/crates/psx-render-contract/Cargo.toml",
                b"render contract manifest".as_slice(),
            ),
            (
                ".psoxide/engine/crates/psx-render-contract/src/lib.rs",
                b"draw surface schema".as_slice(),
            ),
            ("tools/cfg/id1/quake.rc", b"configuration".as_slice()),
        ] {
            fs::write(root.join(relative), contents).unwrap();
        }
    }

    #[test]
    fn asset_recipe_tracks_the_shared_bsp_pin_and_source() {
        let directory = TestDirectory::new();
        write_asset_recipe_fixture(&directory.0);
        let original = asset_recipe_hash_with_psoxide_revision(&directory.0, "revision-a").unwrap();
        assert_ne!(
            original,
            asset_recipe_hash_with_psoxide_revision(&directory.0, "revision-b").unwrap(),
            "a shared-format repin must invalidate cached cooked maps"
        );

        fs::write(
            directory
                .0
                .join(".psoxide/engine/crates/psx-bsp/src/lib.rs"),
            b"changed wire schema",
        )
        .unwrap();
        assert_ne!(
            original,
            asset_recipe_hash_with_psoxide_revision(&directory.0, "revision-a").unwrap(),
            "shared BSP source changes at one local pin must not reuse stale cooked maps"
        );
    }

    #[test]
    fn clean_revision_contract_rejects_dirty_invalid_and_drifted_trees() {
        let revision = "1111111111111111111111111111111111111111";
        require_clean_revision("test", revision, "", None).expect("clean revision");

        let dirty = require_clean_revision("test", revision, " M tracked.rs\n?? new.rs", None)
            .expect_err("dirty source must fail")
            .to_string();
        assert!(dirty.contains("dirty"), "{dirty}");
        assert!(dirty.contains("new.rs"), "{dirty}");

        let invalid = require_clean_revision("test", "not-a-revision", "", None)
            .expect_err("invalid revision must fail")
            .to_string();
        assert!(invalid.contains("invalid Git revision"), "{invalid}");

        let drifted = require_clean_revision("test", revision, "", Some(PSOXIDE_REV))
            .expect_err("revision drift must fail")
            .to_string();
        assert!(drifted.contains("revision drifted"), "{drifted}");
    }

    #[test]
    fn psoxide_shipping_contract_rejects_binary_dirty_and_drifted_sources() {
        let frontend = PsoxideSource::FrontendBinary {
            path: PathBuf::from("frontend"),
        };
        let binary_error = declared_psoxide_contract(&frontend)
            .expect_err("frontend binary must fail")
            .to_string();
        assert!(binary_error.contains("rejects"), "{binary_error}");

        let dirty = PsoxideSource::LocalCheckout {
            path: PathBuf::from("psoxide"),
            revision: PSOXIDE_REV.to_string(),
            dirty_files: 1,
        };
        let dirty_error = declared_psoxide_contract(&dirty)
            .expect_err("dirty checkout must fail")
            .to_string();
        assert!(dirty_error.contains("clean PSoXide"), "{dirty_error}");

        let drifted = PsoxideSource::LocalCheckout {
            path: PathBuf::from("psoxide"),
            revision: "2222222222222222222222222222222222222222".into(),
            dirty_files: 0,
        };
        let drift_error = declared_psoxide_contract(&drifted)
            .expect_err("drifted checkout must fail")
            .to_string();
        assert!(drift_error.contains("revision drifted"), "{drift_error}");

        let pinned = PsoxideSource::Pinned {
            rev: PSOXIDE_REV.to_string(),
        };
        let (_, source_kind, path) =
            declared_psoxide_contract(&pinned).expect("exact pin is provenance-grade");
        assert_eq!(source_kind, "pinned_hydration");
        assert_eq!(path, None);
    }

    #[test]
    fn hydration_and_shareware_contracts_fail_closed() {
        require_exact_hydration_stamp("pinned abc", "pinned abc\n")
            .expect("trailing newline is canonical");
        let stamp_error = require_exact_hydration_stamp("pinned abc", "pinned def")
            .expect_err("hydration stamp drift must fail")
            .to_string();
        assert!(stamp_error.contains("stamp drifted"), "{stamp_error}");

        require_sha256("pak", PAK0_SHA256, PAK0_SHA256).expect("expected PAK digest");
        let pak_error = require_sha256("pak", &"0".repeat(64), PAK0_SHA256)
            .expect_err("wrong PAK digest must fail")
            .to_string();
        assert!(pak_error.contains("checksum mismatch"), "{pak_error}");
    }

    #[test]
    fn artifact_contract_rejects_missing_or_misnamed_outputs() {
        let directory = TestDirectory::new();
        let missing = artifact_provenance(&directory.0.join("quake-psx.cue"), "quake-psx.cue")
            .expect_err("missing artifact must fail")
            .to_string();
        assert!(missing.contains("missing"), "{missing}");

        let wrong_name = directory.0.join("wrong.cue");
        fs::write(&wrong_name, b"cue").unwrap();
        let misnamed = artifact_provenance(&wrong_name, "quake-psx.cue")
            .expect_err("misnamed artifact must fail")
            .to_string();
        assert!(misnamed.contains("name does not match"), "{misnamed}");
    }

    #[test]
    fn shipping_guest_contract_rejects_featured_artifacts() {
        let directory = TestDirectory::new();
        let error = build_disc(
            &directory.0,
            &directory.0.join("build"),
            Some("combat-regression"),
            true,
        )
        .expect_err("shipping artifacts must use the recorded empty feature set")
        .to_string();
        assert!(error.contains("no guest features"), "{error}");
    }

    #[test]
    fn shipping_environment_gate_covers_compiler_flags_wrappers_targets_and_profiles() {
        for name in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC",
            "RUSTC_BOOTSTRAP",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_INCREMENTAL",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_TARGET",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
            "CARGO_TARGET_DIR",
            "CARGO_TARGET_MIPSEL_SONY_PSX_RUSTFLAGS",
            "CARGO_TARGET_MIPSEL_SONY_PSX_LINKER",
            "PSOXIDE",
            "QUAKE_PSX_RUST_DEBUG",
        ] {
            assert!(
                shipping_environment_variable_is_unsafe(name),
                "{name} can change the guest artifact"
            );
        }
        for name in ["RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME", "RUST_LOG"] {
            assert!(
                !shipping_environment_variable_is_unsafe(name),
                "{name} must preserve normal toolchain discovery or is output-neutral"
            );
        }
        let conflicts = shipping_environment_conflicts([
            (OsString::from("RUST_LOG"), OsString::from("debug")),
            (
                OsString::from("CARGO_PROFILE_RELEASE_LTO"),
                OsString::from("false"),
            ),
            (
                OsString::from("RUSTFLAGS"),
                OsString::from("-C opt-level=0"),
            ),
            (OsString::from("RUSTFLAGS"), OsString::from("duplicate")),
        ]);
        assert_eq!(
            conflicts,
            ["CARGO_PROFILE_RELEASE_LTO", "RUSTFLAGS"],
            "shipping reports only unsafe names, sorted and without exposing values"
        );
    }

    #[test]
    fn isolated_shipping_cargo_home_rejects_config_and_reextracts_registry_sources() {
        let directory = TestDirectory::new();
        let home = directory.0.join("cargo-home");
        let prepared = prepare_shipping_cargo_home_at(&home).unwrap();
        assert_eq!(
            fs::read_to_string(prepared.join(SHIPPING_CARGO_HOME_MARKER)).unwrap(),
            format!("schema={SHIPPING_CARGO_HOME_SCHEMA}\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&prepared).unwrap().permissions().mode() & 0o077,
                0,
                "the isolated Cargo home is private"
            );
            assert_eq!(
                fs::metadata(prepared.join(SHIPPING_CARGO_HOME_MARKER))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0,
                "the isolated Cargo home marker is private"
            );
        }

        let archive = prepared.join("registry/cache/index/bitflags.crate");
        let extracted = prepared.join("registry/src/index/bitflags/src/lib.rs");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        fs::create_dir_all(extracted.parent().unwrap()).unwrap();
        fs::write(&archive, b"checksummed archive retained").unwrap();
        fs::write(&extracted, b"tampered extracted source").unwrap();
        reset_shipping_registry_sources(&prepared).unwrap();
        assert!(
            archive.is_file(),
            "download cache remains available offline"
        );
        assert!(
            !prepared.join("registry/src").exists(),
            "shipping forces Cargo to reextract and checksum registry source"
        );

        fs::write(prepared.join("config.toml"), b"[build]\nrustflags=[]\n").unwrap();
        let config_error = prepare_shipping_cargo_home_at(&home)
            .expect_err("isolated Cargo home must reject config")
            .to_string();
        assert!(config_error.contains("ambient config"), "{config_error}");
        fs::remove_file(prepared.join("config.toml")).unwrap();

        fs::write(prepared.join(SHIPPING_CARGO_HOME_MARKER), b"tampered\n").unwrap();
        let marker_error = prepare_shipping_cargo_home_at(&home)
            .expect_err("isolated Cargo home marker must be exact")
            .to_string();
        assert!(marker_error.contains("failed integrity"), "{marker_error}");
    }

    #[test]
    fn shipping_guest_rejects_cargo_configs_above_the_canonical_stage() {
        let directory = TestDirectory::new();
        let game = directory.0.join("stage/game");
        let home = directory.0.join("cargo-home");
        fs::create_dir_all(game.join(".cargo")).unwrap();
        fs::write(
            game.join(".cargo/config.toml"),
            b"[build]\ntarget='mipsel-sony-psx'\n",
        )
        .unwrap();
        let prepared_home = prepare_shipping_cargo_home_at(&home).unwrap();
        reject_ambient_cargo_configs(&game, &prepared_home)
            .expect("the recipe-owned game config is allowed");

        fs::create_dir_all(directory.0.join("stage/.cargo")).unwrap();
        let ambient = directory.0.join("stage/.cargo/config.toml");
        fs::write(&ambient, b"[build]\nrustflags=['-Copt-level=0']\n").unwrap();
        let error = reject_ambient_cargo_configs(&game, &prepared_home)
            .expect_err("ancestor Cargo config must fail closed")
            .to_string();
        assert!(error.contains(&ambient.display().to_string()), "{error}");
    }

    #[test]
    fn rebuild_invalidation_removes_only_the_old_sidecar() {
        let directory = TestDirectory::new();
        let dist = directory.0.join("dist");
        fs::create_dir(&dist).unwrap();
        let cue = dist.join("quake-psx.cue");
        let sidecar = dist.join(PROVENANCE_FILE);
        fs::write(&cue, b"old cue").unwrap();
        fs::write(&sidecar, b"old provenance").unwrap();

        invalidate_shipping_provenance(&directory.0).unwrap();
        assert!(!sidecar.exists());
        assert_eq!(fs::read(&cue).unwrap(), b"old cue");
        invalidate_shipping_provenance(&directory.0).expect("missing sidecar is already invalid");
    }

    #[test]
    fn sidecar_is_deterministic_machine_readable_relative_and_atomic() {
        let directory = TestDirectory::new();
        let cue_path = directory.0.join("quake-psx.cue");
        let bin_path = directory.0.join("quake-psx.bin");
        let exe_path = directory.0.join("quake-psx.exe");
        fs::write(&cue_path, b"FILE quake-psx.bin BINARY\n").unwrap();
        fs::write(&bin_path, b"disc-image").unwrap();
        fs::write(&exe_path, b"PS-X EXE").unwrap();
        let cue = artifact_provenance(&cue_path, "quake-psx.cue").unwrap();
        let bin = artifact_provenance(&bin_path, "quake-psx.bin").unwrap();
        let exe = artifact_provenance(&exe_path, "quake-psx.exe").unwrap();
        let inputs = ShippingInputs {
            quake_revision: "1111111111111111111111111111111111111111".into(),
            psoxide_revision: PSOXIDE_REV.into(),
            psoxide_source_kind: "local_checkout",
            pak0_sha256: PAK0_SHA256.into(),
            pak0_bytes: 18_276_119,
            guest_recipe_sha256: "b".repeat(64),
            rust_toolchain_sha256: "c".repeat(64),
            rustc_version: "rustc test\nhost: test-host\nLLVM version: test".into(),
            cargo_version: "cargo test\nhost: test-host\nlibgit2: test".into(),
        };
        let sidecar = directory.0.join(PROVENANCE_FILE);

        write_shipping_provenance(&sidecar, &inputs, &cue, &bin, &exe).unwrap();
        let first = fs::read(&sidecar).unwrap();
        write_shipping_provenance(&sidecar, &inputs, &cue, &bin, &exe).unwrap();
        let second = fs::read(&sidecar).unwrap();
        assert_eq!(first, second, "identical inputs must be byte-identical");
        assert!(
            !directory.0.join(format!(".{PROVENANCE_FILE}.tmp")).exists(),
            "atomic temporary must not remain"
        );

        let text = String::from_utf8(second).unwrap();
        assert!(!text.contains(&directory.0.display().to_string()));
        let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed["schema"], 1);
        assert_eq!(parsed["quake_source"]["tree_clean"], true);
        assert_eq!(parsed["psoxide"]["revision"], PSOXIDE_REV);
        assert_eq!(parsed["psoxide"]["source_kind"], "local_checkout");
        assert_eq!(parsed["shareware"]["pak0_sha256"], PAK0_SHA256);
        assert_eq!(parsed["build"]["guest_stage_schema"], GUEST_STAGE_SCHEMA);
        assert_eq!(parsed["build"]["guest_recipe_sha256"], "b".repeat(64));
        assert_eq!(parsed["build"]["rust_toolchain_sha256"], "c".repeat(64));
        assert_eq!(
            parsed["build"]["rustc_version"],
            "rustc test\nhost: test-host\nLLVM version: test"
        );
        assert_eq!(parsed["build"]["profile"], SHIPPING_GUEST_PROFILE);
        assert_eq!(parsed["build"]["features"], serde_json::json!([]));
        assert_eq!(parsed["artifacts"]["cue"]["file"], "quake-psx.cue");
        assert_eq!(parsed["artifacts"]["bin"]["bytes"], 10);
        assert_eq!(parsed["artifacts"]["exe"]["bytes"], 8);
    }

    #[test]
    fn guest_recipe_is_content_addressed_and_ignores_hydration_checkout_paths() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        write_guest_recipe_fixture(&first.0, "/checkout/one");
        write_guest_recipe_fixture(&second.0, "/different/checkout/two");
        let toolchain = fake_toolchain();
        let first_hash = guest_recipe_hash(&first.0, &toolchain).unwrap();
        let second_hash = guest_recipe_hash(&second.0, &toolchain).unwrap();
        assert_eq!(first_hash, second_hash);

        fs::create_dir_all(second.0.join(".psoxide/assets")).unwrap();
        fs::write(
            second.0.join(".psoxide/assets/unrelated.bin"),
            b"not in the guest dependency closure",
        )
        .unwrap();
        assert_eq!(
            first_hash,
            guest_recipe_hash(&second.0, &toolchain).unwrap(),
            "unrelated hydrated assets must not invalidate the guest recipe"
        );

        fs::write(
            second.0.join("crates/quake-core/src/lib.rs"),
            b"pub const CORE: u8 = 2;\n",
        )
        .unwrap();
        assert_ne!(
            first_hash,
            guest_recipe_hash(&second.0, &toolchain).unwrap(),
            "guest source changes must select a different canonical stage"
        );

        let mut other_toolchain = toolchain.clone();
        other_toolchain.rustc_version.push_str(" changed");
        assert_ne!(
            first_hash,
            guest_recipe_hash(&first.0, &other_toolchain).unwrap(),
            "compiler identity is part of the complete guest recipe"
        );

        let changed_workspaces = [
            (
                ".psoxide/Cargo.toml",
                "[workspace]\nmembers=[]\n# changed projection\n",
            ),
            (".psoxide/sdk/Cargo.toml", PSOXIDE_SDK_WORKSPACE),
            (".psoxide/engine/Cargo.toml", PSOXIDE_ENGINE_WORKSPACE),
        ];
        assert_ne!(
            first_hash,
            guest_recipe_hash_with_workspaces(&first.0, &toolchain, &changed_workspaces).unwrap(),
            "projected Cargo semantics are part of the guest recipe"
        );
    }

    #[test]
    fn guest_recipe_is_a_literal_rust_only_source_closure() {
        let directory = TestDirectory::new();
        write_guest_recipe_fixture(&directory.0, "/checkout/rust-only");

        let font_vendor = directory.0.join(".psoxide/sdk/crates/psx-font/vendor");
        fs::create_dir_all(&font_vendor).unwrap();
        let provenance_header = font_vendor.join("font8x8_basic.h");
        fs::write(&provenance_header, b"char font8x8_basic[1];\n").unwrap();
        let files = guest_recipe_files(&directory.0).expect("Rust-only guest closure");
        assert!(
            !files.contains(&provenance_header),
            "unused font provenance header must not enter the canonical stage"
        );

        let native_source = directory.0.join("game/native_regression.c");
        fs::write(
            &native_source,
            b"int native_regression(void) { return 1; }\n",
        )
        .unwrap();
        let error = guest_recipe_files(&directory.0)
            .expect_err("native source in a staged package must fail closed")
            .to_string();
        assert!(error.contains("Rust-only guest recipe rejects native source or object"));
        assert!(error.contains("native_regression.c"));

        fs::remove_file(native_source).unwrap();
        fs::write(
            directory.0.join("game/Cargo.lock"),
            b"version = 4\n\n[[package]]\nname = \"cc\"\nversion = \"1.2.0\"\n",
        )
        .unwrap();
        let error = audit_rust_only_guest_lock(&directory.0)
            .expect_err("native build package in the guest lock must fail closed")
            .to_string();
        assert!(error.contains("native build package cc"));
    }

    #[test]
    fn pinned_guest_recipe_paths_match_the_resolved_psoxide_dependency_closure() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let output = Command::new("cargo")
            .current_dir(&root)
            .args([
                "metadata",
                "--manifest-path",
                "game/Cargo.toml",
                "--format-version",
                "1",
                "--locked",
            ])
            .output()
            .expect("run Cargo metadata for the real guest graph");
        assert!(
            output.status.success(),
            "Cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout).expect("metadata JSON");
        let psoxide = root.join(".psoxide").canonicalize().unwrap();
        let mut resolved = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|package| package["source"].is_null())
            .filter_map(|package| package["manifest_path"].as_str())
            .map(PathBuf::from)
            .filter(|manifest| manifest.starts_with(&psoxide))
            .map(|manifest| {
                manifest
                    .parent()
                    .unwrap()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<Vec<_>>();
        resolved.sort();

        let mut pinned = GUEST_RECIPE_PATHS
            .iter()
            .filter(|path| path.starts_with(".psoxide/") && !path.ends_with(".ld"))
            .map(|path| path.to_string())
            .collect::<Vec<_>>();
        pinned.sort();
        assert_eq!(
            pinned, resolved,
            "the canonical recipe must pin the complete and only the complete PSoXide guest closure"
        );
    }

    #[test]
    fn canonical_stage_is_atomic_locked_verified_and_rejects_tampering() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        let stages = TestDirectory::new();
        write_guest_recipe_fixture(&first.0, "/checkout/one");
        write_guest_recipe_fixture(&second.0, "/checkout/two");
        let toolchain = fake_toolchain();
        let recipe = GuestRecipe {
            sha256: guest_recipe_hash(&first.0, &toolchain).unwrap(),
            toolchain,
        };

        let prepared = prepare_guest_stage_at(&first.0, &recipe, &stages.0).unwrap();
        assert_eq!(prepared.path.file_name().unwrap(), recipe.sha256.as_str());
        assert_eq!(
            fs::read_to_string(prepared.path.join(GUEST_STAGE_MARKER)).unwrap(),
            guest_stage_marker(&recipe)
        );
        assert!(
            fs::read_dir(&stages.0)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .all(|entry| !entry.file_name().to_string_lossy().contains(".stage-")),
            "atomic staging must not leave a temporary directory"
        );
        let lock_error = prepare_guest_stage_at(&second.0, &recipe, &stages.0)
            .err()
            .expect("the same recipe cannot build concurrently")
            .to_string();
        assert!(lock_error.contains("already locked"), "{lock_error}");
        let stage_path = prepared.path.clone();
        drop(prepared);

        let cached_exe = stage_path.join("game/target/release/quake-psx.exe");
        fs::create_dir_all(cached_exe.parent().unwrap()).unwrap();
        fs::write(&cached_exe, b"untrusted cached executable").unwrap();
        let reused = prepare_guest_stage_at(&second.0, &recipe, &stages.0).unwrap();
        assert_eq!(reused.path, stage_path);
        assert!(
            cached_exe.is_file(),
            "ordinary builds may reuse a verified content-addressed target"
        );
        reset_guest_target(&reused.path).unwrap();
        assert!(
            !reused.path.join("game/target").exists(),
            "a fresh shipping build must discard cached compiler output"
        );
        drop(reused);

        let projected_manifest = stage_path.join(".psoxide/Cargo.toml");
        fs::write(&projected_manifest, b"[workspace]\n# tampered\n").unwrap();
        let manifest_error = prepare_guest_stage_at(&first.0, &recipe, &stages.0)
            .err()
            .expect("tampered projected workspace must fail closed")
            .to_string();
        assert!(
            manifest_error.contains("workspace failed integrity"),
            "{manifest_error}"
        );
        fs::write(&projected_manifest, PSOXIDE_ROOT_WORKSPACE).unwrap();

        fs::write(stage_path.join("game/src/main.rs"), b"tampered\n").unwrap();
        let tamper_error = prepare_guest_stage_at(&first.0, &recipe, &stages.0)
            .err()
            .expect("tampered canonical source must fail closed")
            .to_string();
        assert!(tamper_error.contains("failed integrity"), "{tamper_error}");
    }
}
