//! Inventory original map data without importing original game source.
use std::{collections::BTreeMap, env, fs, path::Path};
type Result<T> = std::result::Result<T, String>;
type Entity = BTreeMap<String, String>;
type OriginalMap = (String, Vec<Entity>);
fn range(data: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    data.get(offset..offset.checked_add(length).ok_or("range overflow")?)
        .ok_or_else(|| "truncated data range".into())
}
fn word(data: &[u8], offset: usize) -> Result<usize> {
    Ok(u32::from_le_bytes(range(data, offset, 4)?.try_into().unwrap()) as usize)
}
fn tokens(data: &[u8]) -> Result<Vec<String>> {
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    let mut chars = text.trim_end_matches('\0').chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '{' || c == '}' {
            out.push(c.to_string());
            continue;
        }
        if c != '"' {
            return Err(format!("unexpected entity token {c:?}"));
        }
        let mut value = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '"' {
                closed = true;
                break;
            }
            value.push(next);
        }
        if !closed {
            return Err("unterminated entity string".into());
        }
        out.push(value);
    }
    Ok(out)
}
fn entities(data: &[u8]) -> Result<Vec<BTreeMap<String, String>>> {
    let tokens = tokens(data)?;
    let mut cursor = 0;
    let mut out = Vec::new();
    while cursor < tokens.len() {
        if tokens[cursor] != "{" {
            return Err("entity must start with brace".into());
        }
        cursor += 1;
        let mut entity = BTreeMap::new();
        while tokens.get(cursor).is_some_and(|v| v != "}") {
            let key = tokens.get(cursor).ok_or("missing key")?.clone();
            let value = tokens.get(cursor + 1).ok_or("missing value")?.clone();
            if key == "{" || value == "{" || value == "}" {
                return Err("invalid entity field".into());
            }
            entity.insert(key, value);
            cursor += 2;
        }
        if tokens.get(cursor).is_none() {
            return Err("missing closing brace".into());
        }
        cursor += 1;
        out.push(entity);
    }
    Ok(out)
}
fn pak_maps(path: &str) -> Result<Vec<OriginalMap>> {
    let data = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if range(&data, 0, 4)? != b"PACK" {
        return Err("invalid PACK signature".into());
    }
    let directory = range(&data, word(&data, 4)?, word(&data, 8)?)?;
    if directory.len() % 64 != 0 {
        return Err("invalid PACK directory size".into());
    }
    let mut out = Vec::new();
    for entry in directory.chunks_exact(64) {
        let end = entry[..56].iter().position(|b| *b == 0).unwrap_or(56);
        let name = std::str::from_utf8(&entry[..end])
            .map_err(|e| e.to_string())?
            .to_owned();
        if !name.starts_with("maps/") || !name.ends_with(".bsp") {
            continue;
        }
        let bsp = range(&data, word(entry, 56)?, word(entry, 60)?)?;
        if word(bsp, 0)? != 29 {
            return Err(format!("{name}: unsupported BSP version"));
        }
        let lump = range(bsp, word(bsp, 4)?, word(bsp, 8)?)?;
        out.push((name, entities(lump)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
fn clean(value: &str) -> String {
    value.replace(['\t', '\n', '\r', '|'], " ")
}
fn progs_records(path: &str) -> Result<Vec<(String, usize, String)>> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if range(&data, 0, 4)? != b"PACK" {
        return Err("invalid PACK signature".into());
    }
    let directory = range(&data, word(&data, 4)?, word(&data, 8)?)?;
    if directory.len() % 64 != 0 {
        return Err("invalid PACK directory size".into());
    }
    let mut out = Vec::new();
    for entry in directory.chunks_exact(64) {
        if entry[..56].split(|b| *b == 0).next() != Some(b"progs.dat".as_slice()) {
            continue;
        }
        let progs = range(&data, word(entry, 56)?, word(entry, 60)?)?;
        if word(progs, 0)? != 6 {
            return Err("unsupported progs.dat version".into());
        }
        let strings = range(progs, word(progs, 40)?, word(progs, 44)?)?;
        let name = |offset: usize| -> Result<String> {
            let bytes = strings.get(offset..).ok_or("bad program string offset")?;
            let end = bytes
                .iter()
                .position(|b| *b == 0)
                .ok_or("unterminated program string")?;
            Ok(std::str::from_utf8(&bytes[..end])
                .map_err(|e| e.to_string())?
                .into())
        };
        for (kind, offset, count, size, name_offset) in [
            ("function", word(progs, 32)?, word(progs, 36)?, 36, 16),
            ("field", word(progs, 24)?, word(progs, 28)?, 8, 4),
            ("global", word(progs, 16)?, word(progs, 20)?, 8, 4),
        ] {
            let records = range(
                progs,
                offset,
                count.checked_mul(size).ok_or("record size overflow")?,
            )?;
            for (index, record) in records.chunks_exact(size).enumerate() {
                out.push((kind.into(), index, name(word(record, name_offset)?)?));
            }
        }
    }
    Ok(out)
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn binding(root: &Path, value: &str) -> Result<()> {
    let (file, symbol) = value
        .split_once("::")
        .ok_or("binding requires file::symbol")?;
    let source =
        fs::read_to_string(root.join(file)).map_err(|e| format!("source binding {file}: {e}"))?;
    if symbol.is_empty() || !source.contains(symbol) {
        return Err(format!("source symbol missing: {value}"));
    }
    Ok(())
}
fn exercise(root: &Path, feature: &str, assertion: &str, receipt: &str) -> Result<bool> {
    if assertion == "-" && receipt == "-" {
        return Ok(false);
    }
    binding(root, assertion)?;
    let (file, symbol) = assertion.split_once("::").unwrap();
    let receipt_text =
        fs::read_to_string(root.join(receipt)).map_err(|e| format!("receipt: {e}"))?;
    let receipt_fields: BTreeMap<_, _> = receipt_text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .collect();
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    if !git.status.success() {
        return Err("exercise requires a versioned source checkout".into());
    }
    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--error-unmatch", file])
        .output()
        .map_err(|e| e.to_string())?;
    let clean = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .map_err(|e| e.to_string())?;
    if !tracked.status.success() || !clean.success() {
        return Err("exercise requires tracked journey and unchanged tracked source".into());
    }
    let sha = String::from_utf8(git.stdout).map_err(|e| e.to_string())?;
    let hash = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(root.join(file))
        .output()
        .map_err(|e| e.to_string())?;
    if !hash.status.success() {
        return Err("journey hashing failed".into());
    }
    let hash = String::from_utf8(hash.stdout).map_err(|e| e.to_string())?;
    for (key, expected) in [
        ("feature", feature),
        ("assertion", symbol),
        ("pass", "true"),
        ("source_sha", sha.trim()),
        (
            "journey_sha256",
            hash.split_whitespace()
                .next()
                .ok_or("missing journey hash")?,
        ),
    ] {
        if receipt_fields.get(key) != Some(&expected) {
            return Err(format!("unbound exercise receipt {feature}: {key}"));
        }
    }
    Ok(true)
}
fn run() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 4 {
        return Err(
            "usage: quake-feature-census OUTPUT_DIR EVIDENCE.tsv PORT_ROOT PAK [PAK...]".into(),
        );
    }
    let root = Path::new(&args[2]);
    let output = Path::new(&args[0]);
    fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let evidence_text = fs::read_to_string(&args[1]).map_err(|e| e.to_string())?;
    let mut evidence = BTreeMap::new();
    let mut exercised = BTreeMap::new();
    for line in evidence_text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 6
            || !["implemented-not-exercised", "missing", "unverified"].contains(&fields[1])
        {
            return Err(format!("invalid evidence row {line}"));
        }
        if fields[5] != "-" {
            binding(root, fields[5])?;
        } else if fields[1] == "implemented-not-exercised" {
            return Err(format!(
                "implemented row has no source binding: {}",
                fields[0]
            ));
        }
        let yes = exercise(root, fields[0], fields[3], fields[4])?;
        if yes && fields[1] != "implemented-not-exercised" {
            return Err("exercised row must have implementation evidence".into());
        }
        let state = if yes {
            "implemented-exercised"
        } else {
            fields[1]
        };
        exercised.insert(fields[0].to_owned(), yes);
        if evidence
            .insert(
                fields[0].to_owned(),
                (state.to_owned(), fields[2].to_owned()),
            )
            .is_some()
        {
            return Err(format!("duplicate feature {}", fields[0]));
        }
    }
    let mut maps = BTreeMap::new();
    let mut program = String::from("pak\trecord_kind\trecord_index\tname\n");
    let mut original_functions = BTreeMap::new();
    for path in &args[3..] {
        for (kind, index, name) in progs_records(path)? {
            program.push_str(&format!(
                "{}\t{kind}\t{index}\t{}\n",
                clean(path),
                clean(&name)
            ));
            if kind == "function" {
                original_functions.insert(name, format!("progs.dat/function#{index}"));
            }
        }
        for (name, entities) in pak_maps(path)? {
            if maps.insert(name.clone(), entities).is_some() {
                return Err(format!("duplicate original map {name}"));
            }
        }
    }
    for key in evidence.keys() {
        if let Some(name) = key.strip_prefix("@contract:") {
            if !original_functions.contains_key(name) {
                return Err(format!("stale original contract evidence: {key}"));
            }
        }
        if let Some(name) = key.strip_prefix("@map:") {
            if !maps.contains_key(name) {
                return Err(format!("stale original map evidence: {key}"));
            }
        }
    }
    let journey = if root.join("tests/journey.toml").is_file() {
        "present; only bound assertion receipts count as exercised"
    } else {
        "absent; no shipping journey exercise is established"
    };
    if root.join("tests/journey.toml").is_file()
        && exercised.get("@journey:shipping-survival") != Some(&true)
    {
        evidence.insert("@journey:shipping-survival".into(),("unverified".into(),"Shipping journey file exists; source-bound passing assertion receipts are required before exercise can be established.".into()));
    }
    let journey_yes = |key: &str| {
        if exercised.get(key) == Some(&true) {
            "yes"
        } else {
            "no"
        }
    };
    for map in maps.keys() {
        evidence.entry(format!("@map:{map}")).or_insert(("unverified".into(),"Original BSP asset exists; not an EpisodeMap campaign entry. b_* BSPs are pickup/prop brush-model assets, not playable levels; audit cooker/runtime asset usage separately.".into()));
    }
    // Enumerate original semantic entry points from executable metadata only.
    // Statement instructions and original source files are never read or copied.
    for (name, location) in &original_functions {
        let lower = name.to_lowercase();
        if name.starts_with("monster_")
            || name.starts_with("W_Fire")
            || name == "W_Attack"
            || lower.contains("melee")
            || lower.contains("missile")
            || lower.ends_with("checkattack")
            || [
                "hknight_shot",
                "enforcer_fire",
                "CastLightning",
                "OgreFireGrenade",
                "ZombieFireGrenade",
                "CheckPowerups",
                "powerup_touch",
                "trigger_secret",
                "trigger_changelevel",
                "item_sigil",
            ]
            .contains(&name.as_str())
        {
            let key = format!("@contract:{name}");
            let row = evidence.entry(key).or_insert((
                "unverified".into(),
                "Named executable contract exists; semantics and port parity need audited evidence"
                    .into(),
            ));
            row.1 = format!("Original {location}; {}", row.1);
        }
    }
    let mut classes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut locations = String::from("original_location\tclassname\torigin\tmodel\tspawnflags\tmap\ttarget\ttargetname\tstatus\tport_evidence\tjourney_exercised\toriginal_fields\n");
    let mut report = String::from("# Quake missing feature census\n\nScope: only supplied original PAK data, not original source. Entity data proves authored placements and fields, not monster attack semantics or weapon timing parity. Implementation evidence describes audited port execution paths, not original parity. Historical diagnostic receipts do not count as shipping survival journeys.\n\n## Original map progression\n\n| Original map | Entities | Secrets | Exit destinations | Port status | Journey exercised |\n|---|---:|---:|---|---|---|\n");
    report.insert_str(
        report.find("\n\n## Original").unwrap(),
        &format!("\n\nPort journey tests/journey.toml: {journey}."),
    );
    let registered = maps.keys().any(|m| {
        m.starts_with("maps/e2")
            || m.starts_with("maps/e3")
            || m.starts_with("maps/e4")
            || m == "maps/end.bsp"
    });
    report.insert_str(report.find("\n\n## Original").unwrap(),&format!("\n\nSupplied original archives: {}. Registered campaign BSP data present: {}. When absent, episodes 2 to 4, final level and deathmatch placement/secret/exit enumeration remains unavailable and is a queue prerequisite, not a completed full-original census. b_* BSPs are pickup/prop model assets rather than playable levels.",args[3..].join(", "),if registered {"yes (only supplied maps are enumerated)"} else {"no"}));
    for (map, entities) in &maps {
        let secrets = entities
            .iter()
            .filter(|e| e.get("classname").is_some_and(|v| v == "trigger_secret"))
            .count();
        let exits: Vec<_> = entities
            .iter()
            .filter(|e| {
                e.get("classname")
                    .is_some_and(|v| v == "trigger_changelevel")
            })
            .map(|e| e.get("map").map(String::as_str).unwrap_or("unspecified"))
            .collect();
        let key = format!("@map:{map}");
        let (state, proof) = evidence.get(&key).cloned().unwrap_or((
            "unverified".into(),
            "No audited map load execution evidence".into(),
        ));
        report.push_str(&format!(
            "| {map} | {} | {secrets} | {} | {state}: {} | {} |\n",
            entities.len(),
            exits.join(", "),
            clean(&proof),
            journey_yes(&key)
        ));
        for (index, entity) in entities.iter().enumerate() {
            let class = entity
                .get("classname")
                .cloned()
                .unwrap_or_else(|| "<absent>".into());
            let location = format!("{map}#{index}");
            classes
                .entry(class.clone())
                .or_default()
                .push(location.clone());
            let (state, proof) = evidence.get(&class).cloned().unwrap_or((
                "unverified".into(),
                "Not audited; entmap entry alone is not implementation evidence".into(),
            ));
            let value = |key: &str| clean(entity.get(key).map(String::as_str).unwrap_or(""));
            let fields = entity
                .iter()
                .map(|(k, v)| format!("{}={}", clean(k), clean(v)))
                .collect::<Vec<_>>()
                .join("; ");
            locations.push_str(&format!(
                "{location}\t{class}\t{}\t{}\t{}\t{}\t{}\t{}\t{state}\t{}\t{}\t{fields}\n",
                value("origin"),
                value("model"),
                value("spawnflags"),
                value("map"),
                value("target"),
                value("targetname"),
                clean(&proof),
                journey_yes(&class)
            ));
        }
    }
    report.push_str("\n## Class features and exact original placements\n\nEvery original entity and all its authored fields are retained in original-entities.tsv. Locations use original BSP entity order, including lights and worldspawn.\n\n| Feature | Original locations | Status | Port evidence | Journey exercised |\n|---|---|---|---|---|\n");
    let mut counts = BTreeMap::<String, usize>::new();
    let mut queue = String::from("priority\tfeature\tstatus\toriginal_locations\tnext_step\n");
    for (class, locations) in &classes {
        let (state, proof) = evidence.get(class).cloned().unwrap_or((
            "unverified".into(),
            "Not audited; entmap entry alone is not implementation evidence".into(),
        ));
        *counts.entry(state.clone()).or_default() += 1;
        report.push_str(&format!(
            "| {class} | {} | {state} | {} | {} |\n",
            locations.join(", "),
            clean(&proof),
            journey_yes(class)
        ));
        let priority = if class == "trigger_changelevel" || class == "info_player_start" {
            1
        } else if state == "missing" {
            2
        } else if class.starts_with("monster_")
            || class.starts_with("weapon_")
            || class.starts_with("item_")
        {
            3
        } else {
            4
        };
        if state != "implemented-exercised" {
            queue.push_str(&format!(
                "{priority}\t{class}\t{state}\t{}\t{}\n",
                locations.join(", "),
                if state == "implemented-not-exercised" {
                    "Add shipping journey assertions for every authored occurrence and skill flag"
                } else {
                    "Audit execution; implement missing behavior; add shipping journey assertions"
                }
            ));
        }
    }
    report.push_str("\n## Additional mechanics and unavailable original data\n\n| Feature | Status | Evidence | Journey exercised |\n|---|---|---|---|\n");
    for (feature, (state, proof)) in evidence.iter().filter(|(k, _)| k.starts_with('@')) {
        *counts.entry(state.clone()).or_default() += 1;
        report.push_str(&format!(
            "| {feature} | {state} | {} | {} |\n",
            clean(proof),
            journey_yes(feature)
        ));
        if state != "implemented-exercised" {
            let priority = if feature.starts_with("@journey:") {
                0
            } else if feature == "@map:maps/e1m4.bsp" {
                1
            } else if feature.starts_with("@map:maps/e1") || feature == "@map:maps/start.bsp" {
                2
            } else if feature.starts_with("@progression:") {
                3
            } else if state == "missing" {
                4
            } else if feature.starts_with("@map:maps/b_") {
                6
            } else {
                5
            };
            queue.push_str(&format!(
                "{priority}\t{feature}\t{state}\tsee report\t{}\n",
                if state == "missing" {
                    "Implement progression after obtaining and auditing lawful original map data"
                } else {
                    "Add original parity evidence and shipping survival journey"
                }
            ));
        }
    }
    let total: usize = counts.values().sum();
    report.push_str(&format!("\nCounts are feature rows (classes plus explicit map/mechanic rows), not entity instances: missing={}, implemented-not-exercised={}, unverified={}, total={total}. Journey-exercised={}. Unverified is separate from missing because lack of audited evidence does not prove absent implementation.\n",counts.get("missing").unwrap_or(&0),counts.get("implemented-not-exercised").unwrap_or(&0),counts.get("unverified").unwrap_or(&0),counts.get("implemented-exercised").unwrap_or(&0)));
    report.push_str("\nRanked progression queue: (1) source-bound shipping journey and E1M4 normal survival exit; (2) E1M8 return and remaining Episode 1 survival maps plus earned secrets; (3) absent registered episodes, final level and deathmatch, with original data availability as prerequisite; (4) missing class behavior; (5) all implemented-but-unexercised occurrences. queue.tsv contains every missing, unverified and unexercised feature row.\n");
    let mut queue_lines = queue.lines();
    let header = queue_lines.next().unwrap_or("").to_owned();
    let mut ranked: Vec<_> = queue_lines.map(str::to_owned).collect();
    ranked.sort();
    queue = format!("{header}\n{}\n", ranked.join("\n"));
    for (name, text) in [
        ("MISSING-FEATURES-quake-psx.md", report),
        ("original-entities.tsv", locations),
        ("original-progs-records.tsv", program),
        ("queue.tsv", queue),
    ] {
        fs::write(output.join(name), text).map_err(|e| e.to_string())?;
    }
    println!(
        "original_bsp_assets={} campaign_maps={} class_features={} feature_rows={total}",
        maps.len(),
        maps.keys().filter(|m| !m.starts_with("maps/b_")).count(),
        classes.len()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scratch(name: &str) -> std::path::PathBuf {
        let p = env::temp_dir().join(format!("quake-census-{}-{name}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn pack(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut data = Vec::from(b"PACK".as_slice());
        data.extend_from_slice(&(12u32 + payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&64u32.to_le_bytes());
        data.extend_from_slice(payload);
        let mut entry = [0u8; 64];
        entry[..name.len()].copy_from_slice(name.as_bytes());
        entry[56..60].copy_from_slice(&12u32.to_le_bytes());
        entry[60..64].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&entry);
        data
    }
    #[test]
    fn pack_bsp_and_progs_bounds_fail_closed() {
        let dir = scratch("bounds");
        let file = dir.join("fixture.pak");
        fs::write(&file, b"PACK\xff\xff\xff\xff\x40\0\0\0").unwrap();
        assert!(pak_maps(file.to_str().unwrap()).is_err());
        let mut bsp = vec![0u8; 124];
        bsp[..4].copy_from_slice(&29u32.to_le_bytes());
        bsp[4..8].copy_from_slice(&124u32.to_le_bytes());
        bsp[8..12].copy_from_slice(&20u32.to_le_bytes());
        fs::write(&file, pack("maps/test.bsp", &bsp)).unwrap();
        assert!(pak_maps(file.to_str().unwrap()).is_err());
        let mut progs = vec![0u8; 60];
        progs[..4].copy_from_slice(&6u32.to_le_bytes());
        progs[40..44].copy_from_slice(&60u32.to_le_bytes());
        progs[44..48].copy_from_slice(&2u32.to_le_bytes());
        fs::write(&file, pack("progs.dat", &progs)).unwrap();
        assert!(progs_records(file.to_str().unwrap()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn source_and_exercise_evidence_are_conservative() {
        let dir = scratch("evidence");
        fs::write(dir.join("guest.rs"), "fn real_symbol() {}").unwrap();
        assert!(binding(&dir, "guest.rs::real_symbol").is_ok());
        assert!(binding(&dir, "guest.rs::invented_symbol").is_err());
        assert!(!exercise(&dir, "feature", "-", "-").unwrap());
        assert!(exercise(&dir, "feature", "missing.toml::assertion", "receipt.tsv").is_err());
        fs::write(dir.join("journey.toml"), "assertion = 'real_exit'").unwrap();
        fs::write(
            dir.join("receipt.tsv"),
            "feature\twrong_feature\npass\ttrue\n",
        )
        .unwrap();
        assert!(exercise(&dir, "feature", "journey.toml::real_exit", "receipt.tsv").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn bound_passing_receipt_changes_exercise_and_dirty_source_rejects_it() {
        let dir = scratch("passing");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-b", "main"]);
        fs::write(dir.join("journey.toml"), "assertion = 'earned_exit'").unwrap();
        git(&["add", "journey.toml"]);
        git(&[
            "-c",
            "user.name=Census Fixture",
            "-c",
            "user.email=census-fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "Record a journey fixture",
        ]);
        let sha = git(&["rev-parse", "HEAD"]);
        let hash = std::process::Command::new("shasum")
            .args(["-a", "256"])
            .arg(dir.join("journey.toml"))
            .output()
            .unwrap();
        let hash = String::from_utf8(hash.stdout).unwrap();
        fs::write(dir.join("receipt.tsv"),format!("feature\texit\nassertion\tearned_exit\npass\ttrue\nsource_sha\t{}\njourney_sha256\t{}\n",sha.trim(),hash.split_whitespace().next().unwrap())).unwrap();
        assert!(exercise(&dir, "exit", "journey.toml::earned_exit", "receipt.tsv").unwrap());
        fs::write(
            dir.join("journey.toml"),
            "assertion = 'earned_exit'\nchanged = true",
        )
        .unwrap();
        assert!(exercise(&dir, "exit", "journey.toml::earned_exit", "receipt.tsv").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn entity_order_and_fields_survive() {
        let e=entities(b"{\"classname\" \"worldspawn\"}{\"classname\" \"trigger_changelevel\" \"map\" \"e1m8\" \"origin\" \"1 2 3\"}\0").unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[1]["map"], "e1m8");
        assert_eq!(e[1]["origin"], "1 2 3");
    }
    #[test]
    fn rejects_truncated_or_invalid_data() {
        assert!(range(&[0; 8], usize::MAX, 2).is_err());
        assert!(range(&[0; 8], 4, 8).is_err());
        for data in [b"{\"a\"}".as_slice(), b"{\"a\" \"b\"", b"{\"a\" \"b"] {
            assert!(entities(data).is_err());
        }
    }
}
