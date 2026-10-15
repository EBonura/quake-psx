// SPDX-License-Identifier: GPL-2.0-or-later
//! The ambient music pack: eight WAV loops into two XA-ADPCM files for the disc
//! and the song table the guest compiles in.
//!
//! The loops are not stored in Git (they are CC BY 4.0 music, "Eerie Ambiences
//! Vol 1" by Mulula VGM Loops, cut and loudness-matched into 37.8 kHz WAVs).
//! The builder takes them from `--music-dir`, refuses any file whose digest is
//! not the one below, and packs them with `psx-audio-cook xa-pack` from the
//! pinned PSoXide revision.
//!
//! `game/src/music_map.rs` is generated output that is committed so the guest
//! compiles on a checkout without the WAVs. The builder rewrites it from the
//! pack; a changed file shows up in `git status`, and the shipping build's
//! clean-tree check then refuses to proceed until it is committed.

use super::{git_capture, require_tool, run, sha256_path, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The loops in the order `xa-pack` receives them. The guest's banner names
/// and `XA_SONGS` rows follow this order. Each digest is of the WAV the loop
/// was cut to; a file that differs is a different mix.
pub const LOOPS: [(&str, &str); 8] = [
    (
        "Akulak",
        "00fa6d79981cf019ba6a30c0f270e0e5eab18d18bdca2c2ab655173f270401c2",
    ),
    (
        "Joik",
        "ced863f5c23f21fbc55a8f7fd464e84ebd060c0436655315501dbf743fed4b05",
    ),
    (
        "Laska",
        "ced99fa89e7ec18b2cf5c6f2ff9d34b54b37d5956e9c571d04d75699b4bff5bb",
    ),
    (
        "Mioto",
        "6a85872f5d26f239df5b7dcc86c6c711b4340f32805b2349f25a065010843ce9",
    ),
    (
        "Notus",
        "65eff63804453eaadb43bd137b8af6a6aeb26250fdf96c2a0c3bfdd03edb2bca",
    ),
    (
        "Opax",
        "a49959882a48358b2f3868212a93fc24fd46d9a4cc2c67963ebc2e57c6f15772",
    ),
    (
        "Sirtroin",
        "f768a9fad4e32db2f626ba2c503d3acf8f2552cb943f8d38666c623ee18aeaf2",
    ),
    (
        "Skinox",
        "1753ffafbab5368a3f764ddff635f8c3c3d7fafdb7428d6a577d156fb9ab03d1",
    ),
];

/// Stem of the file names `xa-pack` writes: `MUSIC1.XA`, `MUSIC2.XA`.
const PACK_NAME: &str = "MUSIC";
const XA_PACK_REPOSITORY: &str = "https://github.com/EBonura/PSoXide.git";
/// PSoXide revision carrying `psx-audio-cook xa-pack` (branch
/// `feat/shared-xa-encoder-2026-10-15` until it reaches `main`). The guest SDK
/// pin in `components.lock.json` is separate and unchanged: this revision is
/// used only as a host build tool, never linked into the game.
const XA_PACK_REVISION: &str = "4113960f79ea4a64b53bcc9c34318ea1b562e86e";

/// A finished pack.
pub struct MusicPack {
    /// The `.XA` files to hand to `mkisopsx --xa-file`.
    pub files: Vec<PathBuf>,
    /// Receipt for the shipping provenance.
    pub receipt: serde_json::Value,
}

/// The PSoXide checkout whose `psx-audio-cook` packs the music: `requested`
/// (it must be at the pinned revision), or a shallow fetch of the revision
/// into `.quakepsx/xa-pack`.
fn tool_checkout(root: &Path, requested: Option<&Path>) -> Result<PathBuf> {
    let checkout = match requested {
        Some(path) => path.to_path_buf(),
        None => {
            let path = root.join(".quakepsx/xa-pack");
            if !path.join(".git").exists() {
                fs::create_dir_all(&path)?;
                let git = require_tool(&["git"])?;
                run(Command::new(&git)
                    .arg("-C")
                    .arg(&path)
                    .arg("init")
                    .arg("--quiet"))?;
                run(Command::new(&git).arg("-C").arg(&path).args([
                    "remote",
                    "add",
                    "origin",
                    XA_PACK_REPOSITORY,
                ]))?;
            }
            if git_capture(&path, &["rev-parse", "HEAD"]).ok().as_deref() != Some(XA_PACK_REVISION)
            {
                let git = require_tool(&["git"])?;
                run(Command::new(&git).arg("-C").arg(&path).args([
                    "fetch",
                    "--quiet",
                    "--depth",
                    "1",
                    "origin",
                    XA_PACK_REVISION,
                ]))?;
                run(Command::new(&git).arg("-C").arg(&path).args([
                    "checkout",
                    "--quiet",
                    "--detach",
                    XA_PACK_REVISION,
                ]))?;
            }
            path
        }
    };
    let head = git_capture(&checkout, &["rev-parse", "HEAD"])?;
    if head != XA_PACK_REVISION {
        return Err(format!(
            "xa-pack checkout {} is {head}, but the music is packed with {XA_PACK_REVISION}",
            checkout.display()
        )
        .into());
    }
    if !git_capture(
        &checkout,
        &["status", "--porcelain", "--untracked-files=no"],
    )?
    .is_empty()
    {
        return Err(format!("xa-pack checkout {} has local changes", checkout.display()).into());
    }
    Ok(checkout)
}

/// Check the WAVs, pack them into `build/music`, refresh `game/src/music_map.rs`.
pub fn prepare(
    root: &Path,
    build: &Path,
    music_dir: &Path,
    tool: Option<&Path>,
) -> Result<MusicPack> {
    let mut wavs = Vec::new();
    let mut sources = Vec::new();
    for (name, digest) in LOOPS {
        let path = music_dir.join(format!("{name}.wav"));
        if !path.is_file() {
            return Err(format!("music loop {} is missing", path.display()).into());
        }
        let actual = sha256_path(&path)?;
        if actual != digest {
            return Err(format!(
                "music loop {} has digest {actual}, expected {digest}; \
                 it is not the mix this build was made for",
                path.display()
            )
            .into());
        }
        sources.push(serde_json::json!({"loop": name, "sha256": digest}));
        wavs.push(path);
    }
    let checkout = tool_checkout(root, tool)?;
    let out = build.join("music");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let manifest = out.join("music.json");
    let map = out.join("music_map.rs");
    let mut command = Command::new(require_tool(&["cargo"])?);
    command
        .current_dir(&checkout)
        .args(["run", "--quiet", "--release", "--manifest-path"])
        .arg(checkout.join("crates/psx-audio-cook/Cargo.toml"))
        .arg("--")
        .arg("xa-pack")
        .arg(&out)
        .arg(PACK_NAME)
        .args(&wavs)
        .arg("--manifest")
        .arg(&manifest)
        .arg("--rust")
        .arg(&map);
    run(&mut command)?;

    let manifest_json: serde_json::Value = serde_json::from_slice(&fs::read(&manifest)?)?;
    let empty = manifest_json["empty_slots"].as_u64().unwrap_or(u64::MAX);
    if empty != 0 {
        return Err(format!("the music pack leaves {empty} interleave slots empty").into());
    }
    let mut files = Vec::new();
    let mut artifacts = Vec::new();
    for entry in manifest_json["files"]
        .as_array()
        .ok_or("pack manifest has no files")?
    {
        let name = entry["name"].as_str().ok_or("pack file has no name")?;
        let path = out.join(name);
        artifacts.push(serde_json::json!({
            "file": name,
            "sha256": sha256_path(&path)?,
            "bytes": fs::metadata(&path)?.len(),
        }));
        files.push(path);
    }

    let names = LOOPS
        .iter()
        .map(|(name, _)| format!("\"{}\"", name.to_ascii_uppercase()))
        .collect::<Vec<_>>()
        .join(", ");
    let generated = format!(
        "{}pub const XA_SONG_NAMES: [&str; {}] = [{names}];\n",
        fs::read_to_string(&map)?,
        LOOPS.len()
    );
    let guest_map = root.join("game/src/music_map.rs");
    if fs::read_to_string(&guest_map).ok().as_deref() != Some(generated.as_str()) {
        fs::write(&guest_map, generated)?;
        println!("quake-psx-build: refreshed {}", guest_map.display());
    }

    let receipt = serde_json::json!({
        "credit": "Music: Mulula VGM Loops, Eerie Ambiences Vol 1, CC BY 4.0, https://mululavgmloops.itch.io/",
        "packer": {"repository": "EBonura/PSoXide", "revision": XA_PACK_REVISION},
        "sources": sources,
        "files": artifacts,
        "total_sectors": manifest_json["total_sectors"],
    });
    Ok(MusicPack { files, receipt })
}
