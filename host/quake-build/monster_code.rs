//! Supported SDK composition; this consumer never extracts or relocates code.
use super::{run_hazard_tool, Result};
use std::{ffi::OsString, fs, path::Path};

fn tool(root: &Path, name: &str, args: &[OsString]) -> Result<()> {
    let refs: Vec<_> = args.iter().map(|arg| arg.as_os_str()).collect();
    run_hazard_tool(root, name, &refs)
}

fn recipe(elf: &Path, destination: &Path, map_text: &str) -> Vec<OsString> {
    let mut args = vec![
        elf.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ];
    for arg in [
        "--header",
        ".ps_exe_head",
        "--resident",
        ".text",
        "--resident",
        ".data",
        "--resident",
        ".bss",
    ] {
        args.push(arg.into());
    }
    for (name, id, entry, wrapper, descriptor, sites) in [
        (
            "monsters",
            "4",
            "quake_monster_advance",
            "quake_monster_call",
            "QUAKE_MONSTER_DESCRIPTOR",
            "QUAKE_MONSTER_CALLS",
        ),
        (
            "doors",
            "5",
            "quake_door_link",
            "quake_door_call",
            "QUAKE_DOOR_DESCRIPTOR",
            "QUAKE_DOOR_CALLS",
        ),
        (
            "save",
            "6",
            "quake_save_encode",
            "quake_save_call",
            "QUAKE_SAVE_DESCRIPTOR",
            "QUAKE_SAVE_CALLS",
        ),
        (
            "load",
            "7",
            "quake_load_header",
            "quake_load_header_call",
            "QUAKE_LOAD_DESCRIPTOR",
            "QUAKE_LOAD_CALLS",
        ),
        (
            "card",
            "8",
            "quake_card_write",
            "quake_card_write_call",
            "QUAKE_CARD_DESCRIPTOR",
            "QUAKE_CARD_CALLS",
        ),
    ] {
        for arg in [
            "--image",
            name,
            "--id",
            id,
            "--code",
            &format!(".overlay.quake_{name}.text"),
            "--descriptor",
            descriptor,
            "--sites",
            sites,
            "--trap",
            "quake_monster_unloaded",
            "--call",
            &format!("{wrapper}:{entry}"),
        ] {
            args.push(arg.into());
        }
        let data = format!(".overlay.quake_{name}.rodata");
        // LLD omits empty output sections. Select every actual cold data
        // section, rather than inventing an empty SDK image input.
        if map_text.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() == 5 && fields[4] == data
        }) {
            args.extend(["--data".into(), data.into()]);
        }
        if name == "doors" {
            args.extend([
                "--call".into(),
                "quake_mover_call:quake_mover_from_entity".into(),
            ]);
        }
        if name == "load" {
            args.extend([
                "--call".into(),
                "quake_load_restore_call:quake_load_restore".into(),
            ]);
        }
        if name == "card" {
            args.extend([
                "--call".into(),
                "quake_card_read_call:quake_card_read".into(),
            ]);
        }
        if name == "monsters" {
            args.extend([
                "--call".into(),
                "quake_missile_call:quake_monster_missiles".into(),
                "--call".into(),
                "quake_bubble_call:quake_bubble_tick".into(),
            ]);
        }
    }
    args
}

fn guards(root: &Path, generation: &Path, map: &Path, patch: bool) -> Result<()> {
    let image = generation.join("guard.exe");
    let words: Vec<OsString> = fs::read_to_string(generation.join("guard-args"))?
        .split_whitespace()
        .map(OsString::from)
        .collect();
    if words.len() % 2 != 0
        || words
            .chunks_exact(2)
            .any(|pair| pair[0] != "--module" && pair[0] != "--code")
    {
        return Err("SDK emitted malformed guard arguments".into());
    }
    let mut hazards = vec![
        image.as_os_str().to_owned(),
        "--map".into(),
        map.as_os_str().to_owned(),
    ];
    hazards.extend(words.iter().cloned());
    if patch {
        tool(root, "hazard-patch", &hazards)?;
    }
    tool(root, "hazard-scan", &hazards)?;
    let mut stack = vec![image.as_os_str().to_owned(), map.as_os_str().to_owned()];
    for pair in words.chunks_exact(2).filter(|pair| pair[0] == "--module") {
        stack.extend_from_slice(pair);
    }
    // Exact frozen-image parent frames total 40,568 bytes even when
    // mutually exclusive load and think paths are conservatively added:
    // run 31,840 + load_level 2,584 + reset 264 + invoke 232 +
    // Admission 2,304 + update_monsters 3,344. Round upward to 40 KiB.
    // BIOS/cache-flush and GPU callback full-tree limits are recorded
    // separately; these callable closures must never reach cache flush.
    const CALLABLE_BUDGET: usize = 53_248 - 40_960;
    // Original scratchpad roots are checked with the complete composed closure.
    tool(root, "stack-guard", &stack)?;
    for wrapper in [
        "quake_monster_call",
        "quake_door_call",
        "quake_missile_call",
        "quake_bubble_call",
        "quake_mover_call",
        "quake_save_call",
        "quake_load_header_call",
        "quake_load_restore_call",
        "quake_card_write_call",
        "quake_card_read_call",
    ] {
        let mut callable = stack.clone();
        callable.extend([
            "--root".into(),
            wrapper.into(),
            "--budget".into(),
            CALLABLE_BUDGET.to_string().into(),
            "--forbid".into(),
            "__psx_rt_flush_i_cache".into(),
        ]);
        tool(root, "stack-guard", &callable)?;
    }
    Ok(())
}

pub(super) fn compose(root: &Path, elf: &Path, map: &Path) -> Result<()> {
    // Each attempt publishes a new generation; failed partial outputs remain
    // separate from anything admitted or shipped. No second source link occurs.
    let parent = elf.parent().ok_or("ELF has no parent")?;
    let raw = parent.join(format!("overlay-raw-{}", std::process::id()));
    let final_dir = parent.join(format!("overlay-final-{}", std::process::id()));
    // The generation names carry the process id, so a second build in one
    // process (an action that builds two images) finds its own earlier output.
    for stale in [&raw, &final_dir] {
        if stale.exists() {
            fs::remove_dir_all(stale)?;
        }
    }
    let map_text = fs::read_to_string(map)?;
    tool(root, "overlay-compose", &recipe(elf, &raw, &map_text))?;
    guards(root, &raw, map, true)?;
    let mut final_args = recipe(elf, &final_dir, &map_text);
    final_args.extend([
        "--final-guard".into(),
        raw.join("guard.exe").into_os_string(),
    ]);
    tool(root, "overlay-compose", &final_args)?;
    guards(root, &final_dir, map, false)?;
    for name in ["monsters", "doors", "save", "load", "card"] {
        let package = final_dir.join(format!("{name}.pak"));
        if fs::metadata(&package)?.len() > 15360 {
            return Err(format!("{name} package exceeds its existing transfer half").into());
        }
        // A save package must leave the transport half's tail free for the
        // payload (game/src/monster_code.rs STAGING_OFFSET).
        if matches!(name, "save" | "load" | "card") && fs::metadata(&package)?.len() > 10240 {
            return Err(format!("{name} package reaches into the save staging area").into());
        }
        let destination = root.join(format!("game/target/overlay-{name}.pak"));
        fs::create_dir_all(destination.parent().ok_or("package has no parent")?)?;
        fs::copy(package, destination)?;
    }
    // Preserve the original ELF beside the final checked runtime executable.
    fs::copy(elf, elf.with_extension("elf"))?;
    fs::copy(final_dir.join("runtime.exe"), elf)?;
    Ok(())
}
