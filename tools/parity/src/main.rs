//! Parity reports between the original shareware maps and the PS1 port.
//!
//! ```text
//! cargo run --release -- --pak /path/to/PAK0.PAK --repo ../.. [--out ../../docs/parity] [--check]
//! ```
//!
//! Reads the entity lumps of the original BSPs, the cooked `id1psx/maps/*.psb`,
//! the cooker's cfg tables and the runtime sources, and writes one Markdown
//! report per map plus `SUMMARY.md`. `--check` compares instead of writing and
//! exits 1 when a generated file differs from the one on disk.

mod analysis;
mod cooked;
mod evidence;
mod graph;
mod pak;
mod report;
mod sounds;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use report::{Kind, MapFacts};

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

struct Args {
    pak: PathBuf,
    repo: PathBuf,
    out: PathBuf,
    check: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut pak = None;
    let mut repo = PathBuf::from(".");
    let mut out = None;
    let mut check = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pak" => pak = it.next().map(PathBuf::from),
            "--repo" => repo = it.next().map(PathBuf::from).ok_or("--repo needs a path")?,
            "--out" => out = it.next().map(PathBuf::from),
            "--check" => check = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let out = out.unwrap_or_else(|| repo.join("docs/parity"));
    Ok(Args {
        pak: pak.ok_or("--pak PATH is required")?,
        repo,
        out,
        check,
    })
}

fn run(args: &Args) -> Result<bool, String> {
    let pak_bytes = cooked::read_file(&args.pak)?;
    let originals = pak::original_maps(&pak_bytes)?;
    let cfg = |name: &str| -> Result<String, String> {
        std::fs::read_to_string(args.repo.join("tools/cfg/id1").join(name))
            .map_err(|e| format!("{name}: {e}"))
    };
    let res = cooked::read_resources(&cfg("reslist.txt")?);
    let entmap = cooked::read_id_table(&cfg("entmap.txt")?);
    let sfxmap = cooked::read_id_table(&cfg("sfxmap.txt")?);
    let mdlmap = cooked::read_id_table(&cfg("mdlmap.txt")?);
    let limits = std::fs::read_to_string(args.repo.join("LIMITS.md"))
        .map_err(|e| format!("LIMITS.md: {e}"))?;
    let broken = evidence::verify(&args.repo);
    for b in &broken {
        eprintln!("broken citation: {b}");
    }

    let mut outputs: Vec<(String, String)> = Vec::new();
    let mut summary_rows = Vec::new();
    let mut diff_index: BTreeMap<&'static str, (Kind, String, Vec<(&'static str, usize)>)> =
        BTreeMap::new();
    for name in MAPS {
        let entities = originals
            .get(name)
            .ok_or_else(|| format!("PAK0 has no maps/{name}.bsp"))?;
        let bytes = cooked::read_file(&args.repo.join(format!("id1psx/maps/{name}.psb")))?;
        let cm = cooked::read_psb(&bytes)?;
        let census = analysis::census(entities);
        let check = analysis::cook_check(entities, &cm, &res, &entmap);
        let runtime = analysis::runtime_totals(&cm);
        let graph = graph::analyse(entities, &check.placements, &cm);
        let snd = sounds::analyse(entities, &cm, &res, &sfxmap);
        let facts = MapFacts {
            name,
            entities,
            census,
            check,
            runtime,
            graph,
            sounds: snd,
            records: cm.records.len(),
            worldtype: entities[0].int("worldtype").unwrap_or(0),
            cd_track: entities[0].int("sounds").unwrap_or(0),
            title: entities[0].get("message").unwrap_or("").to_owned(),
        };
        let text = report::map_report(&facts, broken.is_empty());
        outputs.push((format!("{name}.md"), text));
        let diffs = report::differences(&facts);
        let bugs = diffs.iter().filter(|d| d.kind == Kind::Bug).count();
        let items = report::items_per_skill(&facts.census);
        let item_total = |cats: &[&str]| -> String {
            let sum: Vec<String> = (0..3)
                .map(|s| {
                    cats.iter()
                        .map(|c| items.get(c).map_or(0, |v| v[s]))
                        .sum::<u32>()
                        .to_string()
                })
                .collect();
            sum.join("/")
        };
        let m = facts.census.monsters;
        let r = facts.runtime;
        let totals_ok =
            (0..4).all(|s| (m[s], facts.census.secrets[s]) == (r[s].0 as u32, r[s].1 as u32));
        summary_rows.push(vec![
            format!("[{}]({name}.md)", name.to_uppercase()),
            facts.title.clone(),
            facts.worldtype.to_string(),
            facts.entities.len().to_string(),
            format!(
                "{} (+{} dropped)",
                facts.records,
                facts
                    .check
                    .dropped
                    .values()
                    .flat_map(|c| c.values())
                    .sum::<u32>()
            ),
            format!("{}/{}/{}", m[0], m[1], m[2]),
            facts.census.secrets[0].to_string(),
            item_total(&["health"]),
            item_total(&["armor"]),
            item_total(&["ammo"]),
            item_total(&["weapons"]),
            if totals_ok {
                "match".into()
            } else {
                "DIFFERENT".into()
            },
            bugs.to_string(),
        ]);
        for d in diffs {
            let e = diff_index
                .entry(d.id)
                .or_insert((d.kind, d.what.clone(), Vec::new()));
            e.2.push((name, d.count));
        }
    }

    // Episode level: samples the cooker can never select, models the PS1 lacks.
    let mut used: BTreeSet<&str> = BTreeSet::new();
    for rules in res.sfx.values() {
        for rule in rules {
            for n in &rule.names {
                used.insert(n.as_str());
            }
        }
    }
    let progs = pak::progs_resource_strings(&pak_bytes)?;
    let in_progs: BTreeSet<&str> = progs.iter().map(String::as_str).collect();
    let mut sound_rows = Vec::new();
    for name in sfxmap.values() {
        if used.contains(name.as_str()) {
            continue;
        }
        let base = name.trim_start_matches("sound/").trim_end_matches(".wav");
        let (dir, stem) = base.split_once('/').unwrap_or(("", base));
        // Named as `dir/stem`, or by a numbered stem (`death2`, `gasp2`) that
        // LIMITS.md lists bare.
        let listed = limits.contains(base)
            || (stem.len() >= 5
                && stem.ends_with(|c: char| c.is_ascii_digit())
                && limits.contains(stem));
        let registered_only = matches!(
            dir,
            "blob" | "boss2" | "enforcer" | "fish" | "hknight" | "shalrath"
        );
        sound_rows.push(vec![
            format!("`{name}`"),
            if registered_only {
                "registered-episode monster, not in E1".into()
            } else {
                "E1 or shared".into()
            },
            if listed {
                "named in LIMITS.md".into()
            } else {
                "NOT in LIMITS.md".into()
            },
        ]);
    }
    let missing_from_sfxmap: Vec<&str> = in_progs
        .iter()
        .copied()
        .filter(|n| n.ends_with(".wav") && !sfxmap.values().any(|s| s == &format!("sound/{n}")))
        .collect();
    let model_rules: BTreeSet<&str> = res.models.values().flatten().map(String::as_str).collect();
    let mut model_rows = Vec::new();
    for n in mdlmap
        .values()
        .filter(|n| !model_rules.contains(n.as_str()))
    {
        let stem = n
            .rsplit('/')
            .next()
            .unwrap_or(n)
            .trim_end_matches(".mdl")
            .trim_end_matches(".spr")
            .trim_end_matches(".bsp");
        model_rows.push(vec![
            format!("`{n}`"),
            if in_progs.contains(n.as_str()) {
                "named by progs.dat".into()
            } else {
                "-".into()
            },
            if limits.contains(stem) {
                "named in LIMITS.md".into()
            } else {
                "not named in LIMITS.md".into()
            },
        ]);
    }

    let mut s = String::new();
    let _ = writeln!(s, "# Episode 1 parity summary\n");
    let _ = writeln!(s, "Generated by `tools/parity` from the shareware `PAK0.PAK` and the cooked maps; do not edit by hand. Per-map reports are linked in the first column. Kills are Easy/Normal/Hard (Nightmare equals Hard); secrets are the same on every skill; pickups are Easy/Normal/Hard.\n");
    let head = [
        "map",
        "title",
        "world",
        "authored",
        "cooked records",
        "kills E/N/H",
        "secrets",
        "health E/N/H",
        "armor E/N/H",
        "ammo E/N/H",
        "weapons E/N/H",
        "runtime totals",
        "bugs",
    ];
    let _ = writeln!(s, "| {} |", head.join(" | "));
    let _ = writeln!(
        s,
        "|{}|",
        head.iter().map(|_| " --- ").collect::<Vec<_>>().join("|")
    );
    for r in &summary_rows {
        let _ = writeln!(s, "| {} |", r.join(" | "));
    }
    let _ = writeln!(s, "\n`runtime totals` compares the original's `total_monsters` and `total_secrets` rule with what the shipping `quake-core` computes from the cooked records on all four skills.\n");
    let _ = writeln!(s, "## Differences by id\n");
    let _ = writeln!(s, "| id | status | maps (count) | difference |");
    let _ = writeln!(s, "| --- | --- | --- | --- |");
    let mut ordered: Vec<_> = diff_index.iter().collect();
    ordered.sort_by_key(|(id, (kind, _, _))| (*kind, **id));
    for (id, (kind, what, maps)) in ordered {
        let list = maps
            .iter()
            .map(|(m, n)| format!("{m} ({n})"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(s, "| {id} | {} | {list} | {what} |", kind.label());
    }
    let _ = writeln!(s, "\nEvidence strength of the LIMITS-listed rows varies: SPU and RAM numbers are hardware evidence; \"needs X\" or \"not done\" rows are cost or effort, which `full-game-completion-standard` does not accept as a PS1 limit without a measurement.\n");
    let _ = writeln!(s, "## Samples the cooker can never select\n");
    let _ = writeln!(
        s,
        "Every `sfxmap.txt` sample no `reslist.txt` rule names, so no map can carry it:\n"
    );
    let _ = writeln!(s, "| sample | scope | LIMITS.md |\n| --- | --- | --- |");
    for r in &sound_rows {
        let _ = writeln!(s, "| {} |", r.join(" | "));
    }
    let _ = writeln!(
        s,
        "\nSounds the original `progs.dat` names that `sfxmap.txt` lacks altogether: {}.\n",
        if missing_from_sfxmap.is_empty() {
            "none".into()
        } else {
            missing_from_sfxmap
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    let _ = writeln!(s, "## Models mapped but never cooked\n");
    let _ = writeln!(s, "Entries of `mdlmap.txt` that no `reslist.txt` `mdl` rule names (the registered-episode monsters, their gib heads, deathmatch backpacks):\n");
    let _ = writeln!(s, "| model | original | LIMITS.md |\n| --- | --- | --- |");
    for r in &model_rows {
        let _ = writeln!(s, "| {} |", r.join(" | "));
    }
    let _ = writeln!(s);
    outputs.push(("SUMMARY.md".into(), s));

    let mut clean = broken.is_empty();
    if args.check {
        for (name, text) in &outputs {
            let on_disk = std::fs::read_to_string(args.out.join(name)).unwrap_or_default();
            if &on_disk != text {
                eprintln!("docs/parity/{name} is out of date");
                clean = false;
            }
        }
    } else {
        std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
        for (name, text) in &outputs {
            std::fs::write(args.out.join(name), text).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    Ok(clean)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("quake-parity: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("quake-parity: {e}");
            ExitCode::from(2)
        }
    }
}
