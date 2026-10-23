//! Installing the packaged disc into the PSoXide games library.
//!
//! A library disc is named `Game Name (repo)`: `<name>.bin`, `<name>.cue` (quoting the bin) and the
//! sidecars beside them. An install replaces each file in place (written under a temporary name,
//! checked against the source digest, then renamed over the live name) and never leaves a copy
//! under the old flat name (`quake-psx.bin` and friends).

use super::{sha256_path, Result, PROVENANCE_FILE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Library name of the shipping disc.
const NAME: &str = "Quake (quake-psx)";
/// The library name before 2026-10-23 (flat `quake-psx.bin`); an install removes a stale set.
const LEGACY_STEM: &str = "quake-psx";
const LICENCE_SUFFIX: &str = ".SLICNSE.TXT";
const PROVENANCE_SUFFIX: &str = ".provenance.json";

/// A file installed beside the disc as `<name><suffix>`.
struct Sidecar<'a> {
    suffix: &'a str,
    contents: &'a [u8],
}

/// A built disc, ready to install under a library name.
struct Disc {
    image: PathBuf,
    image_digest: String,
    name: String,
    cue_text: String,
}

impl Disc {
    /// Read the cue and hash the `.bin` beside it.
    fn load(cue: &Path, name: &str) -> Result<Self> {
        let image = cue.with_extension("bin");
        let cue_text = retarget_cue(&fs::read_to_string(cue)?, name)
            .map_err(|error| format!("{}: {error}", cue.display()))?;
        Ok(Self {
            image_digest: sha256_path(&image)
                .map_err(|error| format!("read {}: {error}", image.display()))?,
            image,
            name: name.to_string(),
            cue_text,
        })
    }

    /// Install the disc and `sidecars` into `games_dir`.
    ///
    /// Files under the old flat name `legacy_stem` are removed after the install, but only when
    /// that cannot lose anything: the legacy bin must be byte-identical to, or older than, this
    /// disc, and a legacy sidecar must have a replacement under the new name. Otherwise the
    /// install is refused before anything is written.
    fn install(
        &self,
        games_dir: &Path,
        legacy_stem: &str,
        sidecars: &[Sidecar<'_>],
    ) -> Result<PathBuf> {
        fs::create_dir_all(games_dir)?;
        let stale = self.stale_legacy_files(games_dir, legacy_stem, sidecars)?;

        let live_bin = games_dir.join(format!("{}.bin", self.name));
        let live_cue = games_dir.join(format!("{}.cue", self.name));
        publish(&live_bin, &self.image_digest, |temporary| {
            fs::copy(&self.image, temporary).map(drop)
        })?;
        for sidecar in sidecars {
            let live = games_dir.join(format!("{}{}", self.name, sidecar.suffix));
            publish(&live, &sha256_bytes(sidecar.contents), |temporary| {
                fs::write(temporary, sidecar.contents)
            })?;
        }
        // The cue goes last: it is what the emulator opens, so it appears only once its bin and
        // sidecars are in place.
        publish(
            &live_cue,
            &sha256_bytes(self.cue_text.as_bytes()),
            |temporary| fs::write(temporary, &self.cue_text),
        )?;
        for path in stale {
            fs::remove_file(&path)
                .map_err(|error| format!("remove stale {}: {error}", path.display()))?;
            println!("REMOVED stale {}", path.display());
        }
        Ok(live_cue)
    }

    fn stale_legacy_files(
        &self,
        games_dir: &Path,
        legacy_stem: &str,
        sidecars: &[Sidecar<'_>],
    ) -> Result<Vec<PathBuf>> {
        let legacy_bin = games_dir.join(format!("{legacy_stem}.bin"));
        let legacy_cue = games_dir.join(format!("{legacy_stem}.cue"));
        let mut stale = Vec::new();
        if legacy_bin.is_file() {
            let same = sha256_path(&legacy_bin)? == self.image_digest;
            if !same && !is_older(&legacy_bin, &self.image)? {
                return Err(format!(
                    "{} is under the old name, differs from the disc being installed and is not \
                     older than it; compare the two and remove or rename one by hand",
                    legacy_bin.display()
                )
                .into());
            }
            stale.push(legacy_bin);
        }
        if legacy_cue.is_file() {
            stale.push(legacy_cue);
        }
        let mut suffixes = vec![PROVENANCE_SUFFIX];
        suffixes.extend(sidecars.iter().map(|sidecar| sidecar.suffix));
        suffixes.dedup();
        for suffix in suffixes {
            let legacy = games_dir.join(format!("{legacy_stem}{suffix}"));
            if !legacy.is_file() {
                continue;
            }
            let replaced = sidecars.iter().any(|sidecar| sidecar.suffix == suffix)
                || games_dir.join(format!("{}{suffix}", self.name)).is_file();
            if !replaced {
                return Err(format!(
                    "{} is under the old name and nothing replaces it; rename it to \
                     \"{}{suffix}\" first so its record survives",
                    legacy.display(),
                    self.name
                )
                .into());
            }
            stale.push(legacy);
        }
        Ok(stale)
    }
}

/// The cue text with its `FILE` line pointing at `<name>.bin`, quoted (the name has spaces).
fn retarget_cue(text: &str, name: &str) -> Result<String> {
    let mut replaced = false;
    let mut output = String::new();
    for line in text.lines() {
        if !replaced && line.trim_start().starts_with("FILE ") {
            output.push_str(&format!("FILE \"{name}.bin\" BINARY\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if replaced {
        Ok(output)
    } else {
        Err("no FILE directive".into())
    }
}

fn is_older(path: &Path, than: &Path) -> io::Result<bool> {
    let modified = |path: &Path| -> io::Result<SystemTime> { fs::metadata(path)?.modified() };
    Ok(modified(path)? < modified(than)?)
}

/// Write `live` by filling a temporary file beside it, checking that it hashes to `expected`, and
/// renaming it over `live`. A failed install leaves the previous `live` untouched.
fn publish(live: &Path, expected: &str, fill: impl FnOnce(&Path) -> io::Result<()>) -> Result<()> {
    let file_name = live.file_name().ok_or("library path has no file name")?;
    let temporary = live.with_file_name(format!(".{}.installing", file_name.to_string_lossy()));
    let outcome = fill(&temporary)
        .map_err(|error| format!("write {}: {error}", temporary.display()).into())
        .and_then(|()| {
            let actual = sha256_path(&temporary)?;
            if actual == expected {
                Ok(())
            } else {
                Err(format!(
                    "{} hashes to {actual}, expected {expected}; install aborted",
                    temporary.display()
                )
                .into())
            }
        })
        .and_then(|()| {
            fs::rename(&temporary, live)
                .map_err(|error| format!("rename onto {}: {error}", live.display()).into())
        });
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The shipping provenance for the library copy: the packaged record with the artifact names and
/// the cue's hash of the library files, when it was installed and which bin it replaced.
fn library_provenance(
    packaged: &Value,
    disc: &Disc,
    previous_bin_sha256: Option<String>,
    installed_at: &str,
) -> Result<Vec<u8>> {
    let mut record = packaged.clone();
    let artifacts = record["artifacts"]
        .as_object_mut()
        .ok_or("shipping provenance has no artifacts")?;
    artifacts
        .get_mut("bin")
        .and_then(Value::as_object_mut)
        .ok_or("shipping provenance has no bin artifact")?
        .insert("file".into(), format!("{NAME}.bin").into());
    artifacts.insert(
        "cue".into(),
        json!({
            "file": format!("{NAME}.cue"),
            "sha256": sha256_bytes(disc.cue_text.as_bytes()),
            "bytes": disc.cue_text.len(),
        }),
    );
    record["installed_at"] = installed_at.into();
    if let Some(previous) = previous_bin_sha256.filter(|previous| *previous != disc.image_digest) {
        record["replaced"] = json!({ "bin_sha256": previous });
    }
    Ok((serde_json::to_string_pretty(&record)? + "\n").into_bytes())
}

fn utc_now() -> Result<String> {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .map_err(|error| format!("cannot run date: {error}"))?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Install the packaged shipping disc (`dist/`) into `games_dir`; return the installed cue.
///
/// `dist/` must be the output of one `build`: the provenance has to describe the bin and cue
/// found there.
pub fn install_dist(root: &Path, games_dir: &Path) -> Result<PathBuf> {
    let dist = root.join("dist");
    let cue = dist.join("quake-psx.cue");
    let disc = Disc::load(&cue, NAME)?;
    let provenance_path = dist.join(PROVENANCE_FILE);
    let packaged: Value =
        serde_json::from_slice(&fs::read(&provenance_path).map_err(|error| {
            format!(
                "read {}: {error}; run `cargo quake-build build` first",
                provenance_path.display()
            )
        })?)?;
    for (artifact, actual) in [
        ("bin", disc.image_digest.clone()),
        ("cue", sha256_path(&cue)?),
    ] {
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
    let live_bin = games_dir.join(format!("{NAME}.bin"));
    let previous = live_bin
        .is_file()
        .then(|| sha256_path(&live_bin))
        .transpose()?;
    let provenance = library_provenance(&packaged, &disc, previous, &utc_now()?)?;
    let licence = fs::read(root.join("release/SLICNSE.TXT"))?;
    disc.install(
        games_dir,
        LEGACY_STEM,
        &[
            Sidecar {
                suffix: PROVENANCE_SUFFIX,
                contents: &provenance,
            },
            Sidecar {
                suffix: LICENCE_SUFFIX,
                contents: &licence,
            },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    const TEST_NAME: &str = "Game Name (repo)";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("quake-library-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A built disc in `dist/` and a library directory path (not yet created).
    fn built(scratch: &Scratch, bin: &[u8]) -> (Disc, PathBuf) {
        let dist = scratch.0.join("dist");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("repo.bin"), bin).unwrap();
        fs::write(
            dist.join("repo.cue"),
            "FILE repo.bin BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        (
            Disc::load(&dist.join("repo.cue"), TEST_NAME).unwrap(),
            scratch.0.join("library"),
        )
    }

    fn age(path: &Path, seconds: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    const SIDECARS: [Sidecar<'static>; 2] = [
        Sidecar {
            suffix: PROVENANCE_SUFFIX,
            contents: b"{}",
        },
        Sidecar {
            suffix: LICENCE_SUFFIX,
            contents: b"licence",
        },
    ];

    #[test]
    fn installs_under_the_library_name_with_a_quoted_cue_and_sidecars() {
        let scratch = Scratch::new("fresh");
        let (disc, library) = built(&scratch, b"disc");
        disc.install(&library, "repo", &SIDECARS).unwrap();
        assert_eq!(
            names(&library),
            [
                "Game Name (repo).SLICNSE.TXT",
                "Game Name (repo).bin",
                "Game Name (repo).cue",
                "Game Name (repo).provenance.json",
            ]
        );
        assert_eq!(
            fs::read_to_string(library.join("Game Name (repo).cue")).unwrap(),
            "FILE \"Game Name (repo).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
        );
    }

    #[test]
    fn replaces_the_live_disc_in_place_and_leaves_no_temporary() {
        let scratch = Scratch::new("replace");
        let (disc, library) = built(&scratch, b"first");
        disc.install(&library, "repo", &SIDECARS).unwrap();
        let (second, _) = built(&scratch, b"second");
        second.install(&library, "repo", &SIDECARS).unwrap();
        assert_eq!(
            fs::read(library.join("Game Name (repo).bin")).unwrap(),
            b"second"
        );
        assert_eq!(names(&library).len(), 4);
    }

    #[test]
    fn removes_an_identical_or_older_legacy_set() {
        for (label, legacy_bin, older) in
            [("identical", &b"disc"[..], false), ("older", b"old", true)]
        {
            let scratch = Scratch::new(label);
            let (disc, library) = built(&scratch, b"disc");
            fs::create_dir_all(&library).unwrap();
            for (file, contents) in [
                ("repo.bin", legacy_bin),
                ("repo.cue", b"FILE repo.bin BINARY\n"),
                ("repo.provenance.json", b"{}"),
                ("repo.SLICNSE.TXT", b"licence"),
            ] {
                fs::write(library.join(file), contents).unwrap();
            }
            if older {
                age(&library.join("repo.bin"), 3600);
            }
            disc.install(&library, "repo", &SIDECARS).unwrap();
            assert_eq!(names(&library).len(), 4, "{label}: {:?}", names(&library));
            assert!(!library.join("repo.bin").exists(), "{label}");
        }
    }

    #[test]
    fn refuses_a_newer_different_legacy_disc_and_writes_nothing() {
        let scratch = Scratch::new("newer");
        let (disc, library) = built(&scratch, b"built disc");
        age(&scratch.0.join("dist/repo.bin"), 3600);
        fs::create_dir_all(&library).unwrap();
        fs::write(library.join("repo.bin"), b"someone else's newer disc").unwrap();
        let error = disc.install(&library, "repo", &SIDECARS).err().unwrap();
        assert!(error.to_string().contains("not older"), "{error}");
        assert_eq!(names(&library), ["repo.bin"]);
    }

    #[test]
    fn refuses_a_legacy_sidecar_nothing_replaces() {
        let scratch = Scratch::new("sidecar");
        let (disc, library) = built(&scratch, b"disc");
        fs::create_dir_all(&library).unwrap();
        fs::write(library.join("repo.provenance.json"), b"{}").unwrap();
        let error = disc.install(&library, "repo", &[]).err().unwrap();
        assert!(error.to_string().contains("nothing replaces it"), "{error}");
        assert_eq!(names(&library), ["repo.provenance.json"]);
    }

    #[test]
    fn library_provenance_renames_artifacts_and_records_the_replaced_bin() {
        let scratch = Scratch::new("record");
        let (disc, _) = built(&scratch, b"disc");
        let packaged = json!({
            "artifacts": {
                "bin": {"file": "quake-psx.bin", "sha256": disc.image_digest, "bytes": 4},
                "cue": {"file": "quake-psx.cue", "sha256": "x", "bytes": 1},
                "exe": {"file": "quake-psx.exe", "sha256": "y", "bytes": 2},
            }
        });
        let bytes =
            library_provenance(&packaged, &disc, Some("old".into()), "2026-10-23T00:00:00Z")
                .unwrap();
        let record: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record["artifacts"]["bin"]["file"], format!("{NAME}.bin"));
        assert_eq!(record["artifacts"]["bin"]["sha256"], disc.image_digest);
        assert_eq!(record["artifacts"]["cue"]["file"], format!("{NAME}.cue"));
        assert_eq!(
            record["artifacts"]["cue"]["sha256"],
            sha256_bytes(disc.cue_text.as_bytes())
        );
        assert_eq!(record["artifacts"]["exe"]["file"], "quake-psx.exe");
        assert_eq!(record["installed_at"], "2026-10-23T00:00:00Z");
        assert_eq!(record["replaced"]["bin_sha256"], "old");
        let same =
            library_provenance(&packaged, &disc, Some(disc.image_digest.clone()), "t").unwrap();
        assert!(serde_json::from_slice::<Value>(&same)
            .unwrap()
            .get("replaced")
            .is_none());
    }

    #[test]
    fn a_cue_without_a_file_line_is_rejected() {
        assert!(retarget_cue("TRACK 01 MODE2/2352\n", TEST_NAME).is_err());
    }
}
