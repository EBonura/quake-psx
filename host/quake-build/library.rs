//! Installing the packaged disc into the PSoXide games library.
//!
//! The install itself (library naming, atomic replace, stale-copy cleanup, provenance) is the
//! shared `psx-library` crate; this module only checks that `dist/` is one consistent build and
//! hands it over with the licence and the packaged provenance.

use super::{sha256_path, Result, PROVENANCE_FILE};
use psx_library::{install, Request, Sidecar, Source};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Library name of the shipping disc.
const NAME: &str = "Quake (quake-psx)";
/// The library name before 2026-10-23 (flat `quake-psx.bin`); an install removes a stale set.
const LEGACY_STEM: &str = "quake-psx";
const LICENCE_SUFFIX: &str = ".SLICNSE.TXT";
const REPOSITORY: &str = "EBonura/quake-psx";

/// Install the packaged shipping disc (`dist/`) into `games_dir`; return the installed cue.
///
/// `dist/` must be the output of one `build`: the provenance has to describe the bin and cue
/// found there.
pub fn install_dist(root: &Path, games_dir: &Path) -> Result<PathBuf> {
    let dist = root.join("dist");
    let cue = dist.join("quake-psx.cue");
    let provenance_path = dist.join(PROVENANCE_FILE);
    let packaged: Value =
        serde_json::from_slice(&fs::read(&provenance_path).map_err(|error| {
            format!(
                "read {}: {error}; run `cargo quake-build build` first",
                provenance_path.display()
            )
        })?)?;
    check_packaged(&packaged, &provenance_path, &cue)?;
    let licence = fs::read(root.join("release/SLICNSE.TXT"))?;
    let sidecars = [Sidecar::new(LICENCE_SUFFIX, &licence)];
    let installed = install(
        &Request::new(&cue, games_dir, NAME, Source::from_git(root, REPOSITORY)?)
            .with_legacy_stem(LEGACY_STEM)
            .with_sidecars(&sidecars)
            .with_provenance_base(&packaged),
    )?;
    for path in &installed.removed {
        println!("REMOVED stale {}", path.display());
    }
    Ok(installed.cue)
}

/// Fail unless `packaged` records the digests of the bin and cue beside `cue`.
fn check_packaged(packaged: &Value, provenance_path: &Path, cue: &Path) -> Result<()> {
    for (artifact, file) in [
        ("bin", cue.with_extension("bin")),
        ("cue", cue.to_path_buf()),
    ] {
        let actual = sha256_path(&file)?;
        let recorded = packaged["artifacts"][artifact]["sha256"].as_str();
        if recorded != Some(actual.as_str()) {
            return Err(format!(
                "{} does not describe dist/quake-psx.{artifact} (recorded {recorded:?}, found \
                 {actual}); rebuild dist before installing",
                provenance_path.display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dist(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("quake-dist-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("quake-psx.bin"), b"disc").unwrap();
        fs::write(path.join("quake-psx.cue"), b"FILE quake-psx.bin BINARY\n").unwrap();
        path
    }

    fn describing(bin: &[u8], cue: &[u8]) -> Value {
        json!({ "artifacts": {
            "bin": { "sha256": psx_library::sha256_bytes(bin) },
            "cue": { "sha256": psx_library::sha256_bytes(cue) },
        } })
    }

    #[test]
    fn a_provenance_that_describes_the_dist_files_is_accepted() {
        let dist = dist("ok");
        let packaged = describing(b"disc", b"FILE quake-psx.bin BINARY\n");
        check_packaged(
            &packaged,
            &dist.join(PROVENANCE_FILE),
            &dist.join("quake-psx.cue"),
        )
        .unwrap();
        fs::remove_dir_all(&dist).unwrap();
    }

    #[test]
    fn a_provenance_of_another_build_is_refused_for_the_bin_and_for_the_cue() {
        let dist = dist("stale");
        for (packaged, which) in [
            (
                describing(b"other disc", b"FILE quake-psx.bin BINARY\n"),
                "quake-psx.bin",
            ),
            (
                describing(b"disc", b"FILE other.bin BINARY\n"),
                "quake-psx.cue",
            ),
        ] {
            let error = check_packaged(
                &packaged,
                &dist.join(PROVENANCE_FILE),
                &dist.join("quake-psx.cue"),
            )
            .unwrap_err();
            assert!(error.to_string().contains(which), "{error}");
        }
        fs::remove_dir_all(&dist).unwrap();
    }
}
