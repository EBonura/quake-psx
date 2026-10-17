// SPDX-License-Identifier: GPL-2.0-or-later
//! The ambient music pack: eight WAV loops as two XA-ADPCM files for the disc
//! and the song table the guest compiles in.
//!
//! The cooked files are committed in `assets/music` (`MUSIC1.XA`, `MUSIC2.XA`,
//! the pack manifest `music.json` and their digests in `SHA256SUMS`), so a fresh
//! clone and CI build the disc without any WAV: [`committed`] checks the files
//! against the digests and the guest's song table and hands them to
//! `mkisopsx --xa-file`.
//!
//! The loops themselves are not stored in Git (they are CC BY 4.0 music,
//! "Eerie Ambiences Vol 1" by Mulula VGM Loops, cut and loudness-matched into
//! 37.8 kHz WAVs). A maintainer who changes them runs `music-check` (cook from
//! `--music-dir`, fail unless the result is byte for byte the committed files)
//! or `music-refresh` (cook, then replace the committed files and
//! `game/src/music_map.rs`). Both refuse any WAV whose digest is not the one
//! below and cook with `psx-audio-cook xa-pack` from the pinned PSoXide revision.

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

/// Times each loop is laid end to end in the XA file. The drive cannot jump
/// from the end of a file back to its start without a seek, and the seek is an
/// audible gap (about 0.4 s in the emulator, more on a console); a loop heard
/// up to three times in a row therefore sits in the file three times, and the
/// drive just keeps reading. The audio is the loop itself, repeated, with no
/// crossfade or edit.
pub const REPEATS: usize = 3;

/// Stem of the file names `xa-pack` writes: `MUSIC1.XA`, `MUSIC2.XA`.
const PACK_NAME: &str = "MUSIC";
const XA_PACK_REPOSITORY: &str = "https://github.com/EBonura/PSoXide.git";
/// PSoXide revision carrying `psx-audio-cook xa-pack` (branch
/// `feat/shared-xa-encoder-2026-10-15` until it reaches `main`). The guest SDK
/// pin in `components.lock.json` is separate and unchanged: this revision is
/// used only as a host build tool, never linked into the game.
const XA_PACK_REVISION: &str = "4113960f79ea4a64b53bcc9c34318ea1b562e86e";

/// Write `source` to `target` with its PCM data repeated `times` times: the
/// same header with the data length updated, the samples appended end to end.
fn repeat_wav(source: &Path, target: &Path, times: usize) -> Result<()> {
    let bytes = fs::read(source)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF WAVE file", source.display()).into());
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        if &bytes[at..at + 4] == b"data" {
            let start = at + 8;
            let end = (start + size).min(bytes.len());
            let mut out = bytes[..start].to_vec();
            for _ in 0..times {
                out.extend_from_slice(&bytes[start..end]);
            }
            let data_len = ((end - start) * times) as u32;
            out[at + 4..at + 8].copy_from_slice(&data_len.to_le_bytes());
            let riff = (out.len() - 8) as u32;
            out[4..8].copy_from_slice(&riff.to_le_bytes());
            fs::write(target, out)?;
            return Ok(());
        }
        at += 8 + size + (size & 1);
    }
    Err(format!("{} has no data chunk", source.display()).into())
}

/// A finished pack.
pub struct MusicPack {
    /// The `.XA` files to hand to `mkisopsx --xa-file`.
    pub files: Vec<PathBuf>,
    /// Receipt for the shipping provenance.
    pub receipt: serde_json::Value,
}

/// Where the cooked music is committed, relative to the repository root.
pub const COMMITTED_DIR: &str = "assets/music";
/// The committed files, in the order of `SHA256SUMS`.
const COMMITTED_FILES: [&str; 3] = ["MUSIC1.XA", "MUSIC2.XA", "music.json"];
/// Bytes of one XA sector as `xa-pack` writes it (mode 2 form 2 without sync).
const XA_SECTOR_BYTES: u64 = 2336;
const CREDIT: &str =
    "Music: Mulula VGM Loops, Eerie Ambiences Vol 1, CC BY 4.0, https://mululavgmloops.itch.io/";

/// The sector counts in the guest's `XA_FILE_SECTORS` line.
fn guest_sectors(root: &Path) -> Result<Vec<u64>> {
    let map = fs::read_to_string(root.join("game/src/music_map.rs"))?;
    let line = map
        .lines()
        .find(|line| line.starts_with("pub const XA_FILE_SECTORS"))
        .ok_or("game/src/music_map.rs has no XA_FILE_SECTORS")?;
    let list = line
        .split('[')
        .nth(2)
        .and_then(|rest| rest.split(']').next())
        .ok_or("XA_FILE_SECTORS is not a list")?;
    list.split(',')
        .map(|value| Ok(value.trim().parse::<u64>()?))
        .collect()
}

/// The committed music, verified: every file against `SHA256SUMS`, the sector
/// counts against the manifest and the guest's song table. This is what every
/// disc build presses; no WAV and no packer is involved.
pub fn committed(root: &Path) -> Result<MusicPack> {
    let dir = root.join(COMMITTED_DIR);
    let sums = fs::read_to_string(dir.join("SHA256SUMS"))
        .map_err(|error| format!("{}/SHA256SUMS: {error}", COMMITTED_DIR))?;
    let mut digests = Vec::new();
    for name in COMMITTED_FILES {
        let expected = sums
            .lines()
            .filter_map(|line| line.split_once("  "))
            .find(|(_, file)| *file == name)
            .map(|(digest, _)| digest.to_string())
            .ok_or_else(|| format!("{COMMITTED_DIR}/SHA256SUMS has no entry for {name}"))?;
        let path = dir.join(name);
        if !path.is_file() {
            return Err(format!("committed music {} is missing", path.display()).into());
        }
        let actual = sha256_path(&path)?;
        if actual != expected {
            return Err(format!(
                "committed music {} has digest {actual}, SHA256SUMS says {expected}",
                path.display()
            )
            .into());
        }
        digests.push(expected);
    }
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("music.json"))?)?;
    let empty = manifest["empty_slots"].as_u64().unwrap_or(u64::MAX);
    if empty != 0 {
        return Err(format!("the music pack leaves {empty} interleave slots empty").into());
    }
    let manifest_files = manifest["files"]
        .as_array()
        .ok_or("music.json has no files")?;
    let sectors: Vec<u64> = manifest_files
        .iter()
        .map(|entry| entry["sectors"].as_u64().unwrap_or(0))
        .collect();
    if sectors != guest_sectors(root)? {
        return Err(format!(
            "game/src/music_map.rs says {:?} sectors per file, {COMMITTED_DIR}/music.json says              {sectors:?}; run `music-refresh` or restore the matching table",
            guest_sectors(root)?
        )
        .into());
    }
    let mut files = Vec::new();
    let mut artifacts = Vec::new();
    for (index, entry) in manifest_files.iter().enumerate() {
        let name = entry["name"].as_str().ok_or("pack file has no name")?;
        let path = dir.join(name);
        let bytes = fs::metadata(&path)?.len();
        if bytes != sectors[index] * XA_SECTOR_BYTES {
            return Err(format!(
                "{} is {bytes} bytes, not {} sectors",
                path.display(),
                sectors[index]
            )
            .into());
        }
        artifacts.push(serde_json::json!({
            "file": name,
            "sha256": digests[index],
            "bytes": bytes,
        }));
        files.push(path);
    }
    let sources: Vec<_> = LOOPS
        .iter()
        .map(|(name, digest)| serde_json::json!({"loop": name, "sha256": digest}))
        .collect();
    let receipt = serde_json::json!({
        "credit": CREDIT,
        "packer": {"repository": "EBonura/PSoXide", "revision": XA_PACK_REVISION},
        "sources": sources,
        "files": artifacts,
        "repeats": REPEATS,
        "total_sectors": manifest["total_sectors"],
    });
    Ok(MusicPack { files, receipt })
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

/// What a cook from the WAVs produced, in `<build>/music`.
struct Cooked {
    out: PathBuf,
    /// The text `game/src/music_map.rs` must hold for this pack.
    guest_map: String,
}

/// Check the WAVs and pack them into `<build>/music` with the pinned `xa-pack`.
fn cook(root: &Path, build: &Path, music_dir: &Path, tool: Option<&Path>) -> Result<Cooked> {
    let mut wavs = Vec::new();
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
        wavs.push((name, path));
    }
    let checkout = tool_checkout(root, tool)?;
    let out = build.join("music");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let repeated_dir = out.join("loops");
    fs::create_dir_all(&repeated_dir)?;
    let mut repeated = Vec::new();
    for (name, path) in &wavs {
        let target = repeated_dir.join(format!("{name}.wav"));
        repeat_wav(path, &target, REPEATS)?;
        repeated.push(target);
    }
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
        .args(&repeated)
        .arg("--manifest")
        .arg(&manifest)
        .arg("--rust")
        .arg(&map);
    run(&mut command)?;
    fs::remove_dir_all(&repeated_dir)?;
    let guest_map = fs::read_to_string(&map)?
        + &format!(
            "/// Copies of each loop laid end to end in its XA channel: XA_SONGS holds the\n\
         /// milliseconds of all of them, so one pass is that divided by this.\n\
         pub const XA_REPEATS: u32 = {REPEATS};\n"
        );
    Ok(Cooked { out, guest_map })
}

/// `music-check` (`write` false): cook from the WAVs and fail unless the files
/// and the guest's song table are byte for byte the committed ones.
/// `music-refresh` (`write` true): cook, then replace them.
pub fn cook_committed(
    root: &Path,
    build: &Path,
    music_dir: &Path,
    tool: Option<&Path>,
    write: bool,
) -> Result<()> {
    let cooked = cook(root, build, music_dir, tool)?;
    let dir = root.join(COMMITTED_DIR);
    let guest_map = root.join("game/src/music_map.rs");
    if write {
        fs::create_dir_all(&dir)?;
        let mut sums = String::new();
        for name in COMMITTED_FILES {
            fs::copy(cooked.out.join(name), dir.join(name))?;
            sums.push_str(&format!("{}  {name}\n", sha256_path(&dir.join(name))?));
        }
        fs::write(dir.join("SHA256SUMS"), sums)?;
        fs::write(&guest_map, &cooked.guest_map)?;
        println!("music-refresh: wrote {COMMITTED_DIR} and game/src/music_map.rs; commit them");
        return Ok(());
    }
    for name in COMMITTED_FILES {
        if fs::read(cooked.out.join(name))? != fs::read(dir.join(name))? {
            return Err(format!("music-check: {name} differs from {COMMITTED_DIR}/{name}").into());
        }
    }
    if fs::read_to_string(&guest_map)? != cooked.guest_map {
        return Err("music-check: game/src/music_map.rs differs from the cooked table".into());
    }
    println!("music-check: the cook is byte-identical to the committed music");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed pack is the one the guest's song table describes, and the
    /// files match their digests.
    #[test]
    fn committed_pack_is_consistent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap();
        let pack = committed(root).expect("assets/music matches SHA256SUMS and music_map.rs");
        assert_eq!(pack.files.len(), 2);
        assert_eq!(pack.receipt["repeats"], REPEATS);
        assert_eq!(
            pack.receipt["sources"].as_array().unwrap().len(),
            LOOPS.len()
        );
    }
}
