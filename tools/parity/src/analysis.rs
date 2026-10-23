//! Everything computed from the two sides of one map. No text formatting here.

use std::collections::{BTreeMap, BTreeSet};

use quake_core::level;
use quake_core::secrets;
use quake_formats::MapEntity;

use crate::cooked::{drop_reason, CookedMap, NameTable, Resources};
use crate::pak::Entity;

pub const SKILL_NAMES: [&str; 4] = ["Easy", "Normal", "Hard", "Nightmare"];

/// The original loader's skill exclusion: `NOT_EASY` 256, `NOT_NORMAL` 512,
/// `NOT_HARD` 1024. Bit 2048 is "not in deathmatch" and never excludes in
/// single player.
pub fn spawns_at_skill(flags: u32, skill: usize) -> bool {
    let bit = match skill {
        0 => 256,
        1 => 512,
        _ => 1024,
    };
    flags & bit == 0
}

/// A zombie with spawnflag 1 is the crucified wall decoration: its spawn
/// function never reaches `walkmonster_start`, so `total_monsters` skips it.
pub fn counts_as_monster(entity: &Entity) -> bool {
    entity.class().starts_with("monster_")
        && !(entity.class() == "monster_zombie" && entity.spawnflags() & 1 != 0)
}

#[derive(Default, Clone)]
pub struct ClassRow {
    pub total: u32,
    pub per_skill: [u32; 4],
    /// Keys present on at least one entity of the class.
    pub keys: BTreeSet<String>,
    /// Distinct values of `spawnflags & 0xff` seen.
    pub flag_values: BTreeSet<u32>,
    /// Distinct `sounds` values seen.
    pub sounds_values: BTreeSet<i64>,
}

pub struct Census {
    pub classes: BTreeMap<String, ClassRow>,
    pub monsters: [u32; 4],
    pub secrets: [u32; 4],
}

pub fn census(entities: &[Entity]) -> Census {
    let mut classes: BTreeMap<String, ClassRow> = BTreeMap::new();
    let mut monsters = [0u32; 4];
    let mut secrets = [0u32; 4];
    for entity in entities {
        let row = classes.entry(entity.class().to_owned()).or_default();
        row.total += 1;
        row.flag_values.insert(entity.spawnflags() & 0xff);
        if let Some(s) = entity.int("sounds") {
            row.sounds_values.insert(s);
        }
        for (key, _) in &entity.fields {
            row.keys.insert(key.clone());
        }
        for skill in 0..4 {
            if spawns_at_skill(entity.spawnflags(), skill) {
                row.per_skill[skill] += 1;
                if counts_as_monster(entity) {
                    monsters[skill] += 1;
                }
                if entity.class() == "trigger_secret" {
                    secrets[skill] += 1;
                }
            }
        }
    }
    Census {
        classes,
        monsters,
        secrets,
    }
}

/// One original entity and where it went.
pub struct Placement {
    pub original: usize,
    /// Index of its cooked record; `None` when the cooker drops it.
    pub record: Option<usize>,
    pub reason: Option<&'static str>,
}

pub struct CookCheck {
    pub placements: Vec<Placement>,
    pub dropped: BTreeMap<&'static str, BTreeMap<String, u32>>,
    pub expected_records: usize,
    pub actual_records: usize,
    /// Classes of cooked records that differ from the predicted sequence.
    pub class_mismatches: Vec<String>,
    /// Field by field differences between a record and the cooker's rules.
    pub field_mismatches: Vec<String>,
    /// Target names that the cooker's suffix-sharing table conflates.
    pub name_collisions: Vec<(String, String)>,
    pub name_table_bytes: usize,
}

fn f32_of(entity: &Entity, key: &str) -> Option<f32> {
    entity.get(key).and_then(|v| v.trim().parse::<f32>().ok())
}

fn q12(v: f32) -> i32 {
    (v * 4096.0) as i32
}

fn angle_units(v: f32) -> i16 {
    (v * 4096.0 / 360.0) as i16
}

fn vector(value: &str) -> Option<[f32; 3]> {
    let parts: Vec<f32> = value
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    parts.try_into().ok()
}

fn clamp16(v: i64) -> i16 {
    v.min(i16::MAX as i64) as i16
}

/// The record the cooker's rules give one original entity (target ids are
/// filled in by the caller from the shared name table).
fn expected_record(entity: &Entity, class_id: u8, is_light: bool) -> MapEntity {
    let mut r = MapEntity {
        class_name: class_id,
        ..MapEntity::default()
    };
    if let Some(v) = entity.get("origin").and_then(vector) {
        r.origin.x = q12(v[0]);
        r.origin.y = q12(v[1]);
        r.origin.z = q12(v[2]);
    }
    if let Some(v) = entity
        .get("angles")
        .or_else(|| entity.get("mangle"))
        .and_then(vector)
    {
        r.angles.x = angle_units(v[0]);
        r.angles.y = angle_units(v[1]);
        r.angles.z = angle_units(v[2]);
    } else if let Some(a) = f32_of(entity, "angle") {
        r.angles.y = if a == -1.0 || a == -2.0 {
            a as i16
        } else {
            angle_units(a)
        };
    }
    if let Some(m) = entity.get("model") {
        if let Some(n) = m.strip_prefix('*') {
            r.model = -(n.parse::<i32>().unwrap_or(0) as i16);
        }
    }
    r.spawn_flags = entity.int("spawnflags").unwrap_or(0) as u16;
    r.noise = entity.int("sounds").unwrap_or(0) as i8;
    if let Some(c) = entity.int("count") {
        r.count = clamp16(c);
    }
    if let Some(l) = entity.int("lip") {
        r.count = clamp16(l);
    }
    if is_light {
        if let Some(s) = entity.int("style") {
            r.count = clamp16(s);
        }
    }
    r.damage = clamp16(entity.int("dmg").unwrap_or(0));
    r.speed = clamp16(entity.int("speed").unwrap_or(0));
    r.height = clamp16(entity.int("height").unwrap_or(0));
    r.health = clamp16(entity.int("health").unwrap_or(0));
    r.wait = f32_of(entity, "wait").map(q12).unwrap_or(0);
    r.delay = f32_of(entity, "delay").map(q12).unwrap_or(0);
    r
}

fn cooked_string(strings: &[u8], offset: u16) -> String {
    let start = offset as usize;
    let end = strings[start.min(strings.len())..]
        .iter()
        .position(|b| *b == 0)
        .map_or(strings.len(), |n| start + n);
    String::from_utf8_lossy(&strings[start.min(strings.len())..end]).into_owned()
}

fn unescape(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(n) => {
                    out.push(c);
                    out.push(n);
                }
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn cook_check(
    entities: &[Entity],
    cooked: &CookedMap,
    res: &Resources,
    entmap: &BTreeMap<u16, String>,
) -> CookCheck {
    let class_ids: BTreeMap<&str, u16> = entmap.iter().map(|(i, n)| (n.as_str(), *i)).collect();
    let mut placements = Vec::new();
    let mut dropped: BTreeMap<&'static str, BTreeMap<String, u32>> = BTreeMap::new();
    let mut next = 2usize;
    let mut names = NameTable::new();
    let mut seen_names: BTreeMap<u16, BTreeSet<String>> = BTreeMap::new();
    let mut class_mismatches = Vec::new();
    let mut field_mismatches = Vec::new();
    let mut note_name = |names: &mut NameTable, value: &str| -> u16 {
        let id = names.add(value);
        if id != 0 {
            seen_names.entry(id).or_default().insert(value.to_owned());
        }
        id
    };
    let mut start_record: Option<usize> = None;
    for (index, entity) in entities.iter().enumerate() {
        if index == 0 {
            placements.push(Placement {
                original: 0,
                record: Some(0),
                reason: None,
            });
            continue;
        }
        if let Some(reason) = drop_reason(entity, res) {
            *dropped
                .entry(reason)
                .or_default()
                .entry(entity.class().to_owned())
                .or_default() += 1;
            placements.push(Placement {
                original: index,
                record: None,
                reason: Some(reason),
            });
            continue;
        }
        let slot = if entity.class() == "info_player_start" {
            start_record = Some(1);
            1
        } else {
            next += 1;
            next - 1
        };
        placements.push(Placement {
            original: index,
            record: Some(slot),
            reason: None,
        });
    }
    let expected_records = next;
    let actual_records = cooked.records.len();
    if expected_records != actual_records {
        class_mismatches.push(format!(
            "record count: the cooker's rules give {expected_records}, the cooked file holds {actual_records}"
        ));
    }
    // Target ids come out of one table, filled in entity order, targets
    // before kill targets before names, as the cooker does.
    for placement in &placements {
        let Some(slot) = placement.record else {
            continue;
        };
        let entity = &entities[placement.original];
        let Some(record) = cooked.records.get(slot) else {
            continue;
        };
        let class = entity.class();
        // The cooker builds the start's record from the reserved player slot,
        // which keeps class 1 (`player`).
        let id = if class == "info_player_start" {
            1
        } else {
            *class_ids.get(class).unwrap_or(&0xffff) as u8
        };
        let is_light = class.starts_with("light");
        let mut expect = expected_record(entity, id, is_light);
        if placement.original == 0 {
            expect.class_name = 0;
            expect.noise = entity.int("sounds").unwrap_or(0) as i8;
        }
        if class == "event_lightning" {
            expect.target = note_name(&mut names, "lightning");
        } else if let Some(t) = entity.get("target") {
            expect.target = note_name(&mut names, t);
        }
        if let Some(t) = entity.get("killtarget") {
            expect.kill_target = note_name(&mut names, t);
        }
        if let Some(t) = entity.get("targetname") {
            expect.target_name = note_name(&mut names, t);
        }
        let label = format!("#{} {class}", placement.original);
        if record.class_name != expect.class_name {
            class_mismatches.push(format!(
                "{label}: cooked class {:#04x}, expected {:#04x}",
                record.class_name, expect.class_name
            ));
            continue;
        }
        let mut diffs = Vec::new();
        macro_rules! cmp {
            ($f:ident) => {
                if record.$f != expect.$f {
                    diffs.push(format!(
                        "{} cooked {:?} expected {:?}",
                        stringify!($f),
                        record.$f,
                        expect.$f
                    ));
                }
            };
        }
        cmp!(noise);
        cmp!(spawn_flags);
        cmp!(model);
        cmp!(health);
        cmp!(damage);
        cmp!(speed);
        cmp!(count);
        cmp!(height);
        cmp!(target);
        cmp!(kill_target);
        cmp!(target_name);
        cmp!(wait);
        cmp!(delay);
        cmp!(angles);
        cmp!(origin);
        let expected_string = if let Some(map) = entity.get("map") {
            map.to_ascii_uppercase()
        } else if placement.original != 0 {
            entity.get("message").map(unescape).unwrap_or_default()
        } else {
            String::new()
        };
        let got = cooked_string(&cooked.strings, record.string);
        if got != expected_string {
            diffs.push(format!(
                "string cooked {got:?} expected {expected_string:?}"
            ));
        }
        if !diffs.is_empty() {
            field_mismatches.push(format!("{label}: {}", diffs.join("; ")));
        }
    }
    let _ = start_record;
    let mut name_collisions = Vec::new();
    for set in seen_names.values() {
        if set.len() > 1 {
            let v: Vec<&String> = set.iter().collect();
            name_collisions.push((v[0].clone(), v[1].clone()));
        }
    }
    CookCheck {
        placements,
        dropped,
        expected_records,
        actual_records,
        class_mismatches,
        field_mismatches,
        name_collisions,
        name_table_bytes: names.len(),
    }
}

/// Totals the shipping runtime computes from the cooked records, per skill:
/// `(monsters, secrets)`.
pub fn runtime_totals(cooked: &CookedMap) -> [(u16, u16); 4] {
    let mut out = [(0, 0); 4];
    for skill in 0..4u8 {
        out[skill as usize] = (
            level::count_authored(&cooked.records[..], skill),
            secrets::count_authored(&cooked.records[..], skill),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cooked::NameTable;
    use crate::pak::parse_entities;

    fn ents(text: &str) -> Vec<Entity> {
        parse_entities(text).unwrap()
    }

    #[test]
    fn skill_bits_follow_the_original_loader() {
        // 256 hides on Easy, 512 on Normal, 1024 on Hard and Nightmare, 2048 never.
        assert!(!spawns_at_skill(256, 0) && spawns_at_skill(256, 1) && spawns_at_skill(256, 2));
        assert!(spawns_at_skill(512, 0) && !spawns_at_skill(512, 1) && spawns_at_skill(512, 3));
        assert!(spawns_at_skill(1024, 1) && !spawns_at_skill(1024, 2) && !spawns_at_skill(1024, 3));
        assert!((0..4).all(|s| spawns_at_skill(2048, s)));
    }

    #[test]
    fn totals_exclude_crucified_zombies_and_follow_the_skill_bits() {
        let text = r#"
            {"classname" "worldspawn"}
            {"classname" "monster_army"}
            {"classname" "monster_army" "spawnflags" "256"}
            {"classname" "monster_ogre" "spawnflags" "1024"}
            {"classname" "monster_zombie" "spawnflags" "1"}
            {"classname" "monster_zombie"}
            {"classname" "trigger_secret"}
            {"classname" "trigger_secret" "spawnflags" "512"}
        "#;
        let c = census(&ents(text));
        assert_eq!(c.monsters, [3, 4, 3, 3]);
        assert_eq!(c.secrets, [2, 1, 2, 2]);
    }

    #[test]
    fn the_runtime_counts_the_same_totals_from_cooked_records() {
        use quake_formats::MapEntity;
        // class ids: army 0x36, zombie 0x44, secret 0x50
        let record = |class_name, spawn_flags| MapEntity {
            class_name,
            spawn_flags,
            ..MapEntity::default()
        };
        let cooked = CookedMap {
            records: vec![
                record(0, 0),
                record(0x36, 0),
                record(0x36, 256),
                record(0x44, 1),
                record(0x44, 0),
                record(0x50, 0),
                record(0x50, 512),
            ],
            strings: vec![0],
            local_sounds: Vec::new(),
            local_payload_bytes: 0,
            spu_high_water: 0,
        };
        // Same shape as the previous test, minus the ogre: army, army(256), zombie, secrets.
        let t = runtime_totals(&cooked);
        assert_eq!(t.map(|(m, _)| m), [2, 3, 3, 3]);
        assert_eq!(t.map(|(_, s)| s), [2, 1, 2, 2]);
    }

    #[test]
    fn the_cooker_name_table_shares_suffixes() {
        let mut t = NameTable::new();
        assert_eq!(t.add("door"), 1);
        assert_eq!(t.add("door"), 1);
        // "or" is the tail of "door": the cooker gives both one id.
        assert_eq!(t.add("or"), 3);
        assert_eq!(t.add("t1"), 6);
    }

    #[test]
    fn every_runtime_citation_resolves() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(crate::evidence::verify(&repo), Vec::<String>::new());
    }

    /// Pins the shareware totals when the original archive is at hand
    /// (`QUAKE_PAK0=/path/PAK0.PAK cargo test`).
    #[test]
    fn shareware_totals_match_the_known_rule() {
        let Ok(path) = std::env::var("QUAKE_PAK0") else {
            return;
        };
        let maps = crate::pak::original_maps(&std::fs::read(path).unwrap()).unwrap();
        let expect: [(&str, [u32; 4], u32); 9] = [
            ("start", [0, 0, 0, 0], 0),
            ("e1m1", [10, 23, 42, 42], 6),
            ("e1m2", [16, 24, 42, 42], 3),
            ("e1m3", [31, 47, 65, 65], 3),
            ("e1m4", [22, 42, 59, 59], 3),
            ("e1m5", [18, 39, 60, 60], 5),
            ("e1m6", [14, 24, 31, 31], 4),
            ("e1m7", [21, 21, 21, 21], 0),
            ("e1m8", [17, 28, 34, 34], 2),
        ];
        for (name, monsters, secrets) in expect {
            let c = census(&maps[name]);
            assert_eq!(c.monsters, monsters, "{name} kills");
            assert_eq!(c.secrets, [secrets; 4], "{name} secrets");
        }
    }
}
