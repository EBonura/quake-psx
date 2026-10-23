//! Markdown for one map and for the episode summary. Plain tables, no dates,
//! deterministic order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::analysis::{Census, CookCheck, SKILL_NAMES};
use crate::evidence::{self, Handling, CLASS_RULES, FLAG_RULES, KEY_RULES};
use crate::graph::GraphInfo;
use crate::pak::Entity;
use crate::sounds::SoundInfo;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Differs from the original, nothing in LIMITS.md covers it.
    Bug,
    /// Differs, and LIMITS.md lists it (the evidence column says how strong).
    Listed,
    /// A deliberate decision recorded outside LIMITS.md.
    Decision,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bug => "BUG (unlisted)",
            Kind::Listed => "LIMITS-listed",
            Kind::Decision => "decision (not in LIMITS.md)",
        }
    }
}

pub struct Diff {
    pub id: &'static str,
    pub kind: Kind,
    pub what: String,
    pub count: usize,
    pub cite: &'static str,
}

pub struct MapFacts<'a> {
    pub name: &'static str,
    pub entities: &'a [Entity],
    pub census: Census,
    pub check: CookCheck,
    pub runtime: [(u16, u16); 4],
    pub graph: GraphInfo,
    pub sounds: SoundInfo,
    pub records: usize,
    pub worldtype: i64,
    pub cd_track: i64,
    pub title: String,
}

impl MapFacts<'_> {
    fn count_class(&self, class: &str) -> u32 {
        self.census.classes.get(class).map_or(0, |r| r.total)
    }

    fn with_sounds(&self, class: &str, f: impl Fn(i64) -> bool) -> usize {
        self.entities
            .iter()
            .filter(|e| e.class() == class && f(e.int("sounds").unwrap_or(0)))
            .count()
    }
}

/// Selector the port's world-type variant plays for doors: world type 0 uses
/// the `sounds 1` pair, 1 the `sounds 3` pair, 2 the `sounds 2` pair.
fn port_door_selector(worldtype: i64) -> i64 {
    [1, 3, 2][worldtype.clamp(0, 2) as usize]
}

/// Buttons: world types 0 and 1 play `sounds 0` (airbut1), type 2 `sounds 1`.
fn port_button_selector(worldtype: i64) -> i64 {
    [0, 0, 1][worldtype.clamp(0, 2) as usize]
}

pub fn differences(m: &MapFacts<'_>) -> Vec<Diff> {
    let mut out = Vec::new();
    let n = m.count_class("air_bubbles") as usize;
    if n > 0 {
        out.push(Diff { id: "D-AIRBUBBLES", kind: Kind::Bug, count: n,
            what: "air_bubbles emitters spawn nothing; the original releases bubble sprites underwater".into(),
            cite: "game/src/entity.rs fn render_spawn has no arm for class 0x02" });
    }
    let doors = m.with_sounds("func_door", |s| {
        s != 0 && s != port_door_selector(m.worldtype)
    });
    if doors > 0 {
        out.push(Diff { id: "D-DOOR-SOUNDS", kind: Kind::Bug, count: doors,
            what: format!("door move/stop sounds follow the world type (type {} plays the sounds {} pair); the original picks the pair from each door's `sounds` (1 doormv1/drclos4, 2 hydro, 3 stndr, 4 ddoor); the selector table is the original door spawn function as known, not decoded from progs.dat, so confirm with a retail trace before fixing", m.worldtype, port_door_selector(m.worldtype)),
            cite: "tools/cfg/id1/reslist.txt ent func_door; crates/quake-core/src/mover.rs fn mover_sound_events" });
    }
    let buttons = m.with_sounds("func_button", |s| s != port_button_selector(m.worldtype));
    if buttons > 0 {
        out.push(Diff { id: "D-BUTTON-SOUNDS", kind: Kind::Bug, count: buttons,
            what: "button sound follows the world type; the original picks airbut1, switch21, switch02 or switch04 from `sounds` 0..3 (same caveat on the selector table)".into(),
            cite: "tools/cfg/id1/reslist.txt ent func_button; crates/quake-core/src/mover.rs BUTTON_ACTIVATE_SOUND" });
    }
    let plats = m
        .entities
        .iter()
        .filter(|e| {
            e.class() == "func_plat" && {
                let sel = e.int("sounds").unwrap_or(0);
                let effective = if sel == 0 { 2 } else { sel };
                effective != [2, 2, 1][m.worldtype.clamp(0, 2) as usize]
            }
        })
        .count();
    if plats > 0 {
        out.push(Diff { id: "D-PLAT-SOUNDS", kind: Kind::Bug, count: plats,
            what: "plat sound pair follows the world type; the original picks plat1 or medplat from `sounds`".into(),
            cite: "tools/cfg/id1/reslist.txt ent func_plat" });
    }
    let trig3 =
        m.with_sounds("trigger_once", |s| s == 3) + m.with_sounds("trigger_multiple", |s| s == 3);
    if trig3 > 0 {
        out.push(Diff { id: "D-TRIGGER-SOUND-3", kind: Kind::Bug, count: trig3,
            what: "triggers with sounds 3 should play misc/trigger1.wav; the sample is in sfxmap (0x7c) but no resource rule cooks it".into(),
            cite: "game/src/entity.rs fn trigger_noise" });
    }
    let swamp2 = m.count_class("ambient_swamp2") as usize;
    if swamp2 > 0 {
        out.push(Diff {
            id: "D-SWAMP2",
            kind: Kind::Decision,
            count: swamp2,
            what:
                "ambient_swamp2 plays swamp1.wav (resource list comment: they are almost the same)"
                    .into(),
            cite: "tools/cfg/id1/reslist.txt ent ambient_swamp2",
        });
    }
    let facing = m
        .entities
        .iter()
        .filter(|e| {
            matches!(e.class(), "trigger_once" | "trigger_multiple") && e.get("angle").is_some()
        })
        .count();
    if facing > 0 {
        out.push(Diff { id: "D-TRIGGER-FACING", kind: Kind::Listed, count: facing,
            what: "trigger_once/multiple with an `angle` should fire only from the facing side; the port fires from any side (LIMITS: needs the player yaw in the trigger pass; no hardware evidence given)".into(),
            cite: "LIMITS.md Inventory pass 3" });
    }
    if matches!(m.name, "e1m4" | "e1m6") {
        out.push(Diff { id: "D-DOOR-CHAIN-MESSAGE", kind: Kind::Listed, count: 1,
            what: "a door chain prints its message only from a member that carries it (about 150 bytes of image would fix it; weak evidence)".into(),
            cite: "LIMITS.md Inventory pass 3" });
    }
    let crucified = m
        .entities
        .iter()
        .filter(|e| e.class() == "monster_zombie" && e.spawnflags() & 1 != 0)
        .count();
    if crucified > 0 {
        out.push(Diff { id: "D-CRUCIFIED-NONSOLID", kind: Kind::Bug, count: crucified,
            what: "crucified zombies are non-blocking; the original keeps them SOLID_SLIDEBOX (stated as a known deviation in the source, absent from LIMITS.md)".into(),
            cite: "game/src/entity.rs (comment above `damageable:`)" });
    }
    let wizards = m.count_class("monster_wizard") as usize;
    if wizards > 0 {
        out.push(Diff { id: "D-WIZARD-SPIKE-MODEL", kind: Kind::Bug, count: wizards,
            what: "wizard spikes fly as the plain spike.mdl (NAIL_MODEL_ID); the original uses w_spike.mdl, which no resource rule cooks (LIMITS covers only the impact puff and the trail)".into(),
            cite: "game/src/entity.rs fn launch_monster_missile (MonsterAttack::Spit)" });
    }
    let teleports = m.count_class("trigger_teleport");
    if teleports > 0 {
        out.push(Diff {
            id: "D-TELE-FOG",
            kind: Kind::Listed,
            count: teleports as usize,
            what: "departure fog and sound are not drawn".into(),
            cite: "LIMITS.md Inventory pass 2",
        });
    }
    let flames: u32 = [
        "light_flame_large_yellow",
        "light_flame_small_white",
        "light_flame_small_yellow",
        "light_torch_small_walltorch",
        "light_fluoro",
        "light_fluorospark",
    ]
    .iter()
    .map(|c| m.count_class(c))
    .sum();
    if flames > 0 {
        out.push(Diff { id: "D-LIGHT-HUMS", kind: Kind::Listed, count: flames as usize,
            what: "torch crackle (fire1) and tube hum (fl_hum1, buzz1) are not cooked; water and wind leaf ambience is absent".into(),
            cite: "LIMITS.md Inventory pass 2 (SPU bytes quoted)" });
    }
    let amb: u32 = m.sounds.ambient.values().sum();
    if amb > 11 {
        out.push(Diff { id: "D-AMBIENT-VOICES", kind: Kind::Decision, count: amb as usize,
            what: "11 ambient voices serve the map's ambient sources (cap 32 sources); the original plays every static source (SPU has 24 voices in total)".into(),
            cite: "game/src/audio.rs MAX_AMBIENT_VOICES, MAX_AMBIENT_SOURCES" });
    }
    out.push(Diff { id: "D-CD-TRACK", kind: Kind::Decision, count: 1,
        what: format!("worldspawn CD track {} is not played; sporadic XA ambient loops play instead (the tracks are not in PAK0)", m.cd_track),
        cite: "README.md Music" });
    out.push(Diff {
        id: "D-INTERMISSION",
        kind: Kind::Listed,
        count: 1,
        what: "end-of-level panel is text, waits for a button, and the camera pick is not random"
            .into(),
        cite: "LIMITS.md Inventory pass 2 and 3",
    });
    out.push(Diff { id: "D-SHARED-POOLS", kind: Kind::Listed, count: 1,
        what: "fixed projectile pools, one dynamic light, reduced particles, 4 of 6 player pain voices, SPU-bank sample omissions".into(),
        cite: "LIMITS.md Weapons, items and monsters; Survival, view and messages" });
    for (label, n) in [
        ("count mismatch", m.check.class_mismatches.len()),
        ("field mismatch", m.check.field_mismatches.len()),
        ("target name collision", m.check.name_collisions.len()),
    ] {
        if n > 0 {
            out.push(Diff {
                id: "D-COOK",
                kind: Kind::Bug,
                count: n,
                what: format!("the cooked file disagrees with the cooker's rules: {label}"),
                cite: "section 2",
            });
        }
    }
    if m.graph.cooked_edges != m.graph.original_edges {
        out.push(Diff {
            id: "D-TARGET-EDGES",
            kind: Kind::Bug,
            count: m.graph.cooked_edges.abs_diff(m.graph.original_edges),
            what: format!(
                "cooked target edges {} against {} in the original names",
                m.graph.cooked_edges, m.graph.original_edges
            ),
            cite: "section 4",
        });
    }
    for skill in 0..4 {
        let want = (m.census.monsters[skill], m.census.secrets[skill]);
        let got = (m.runtime[skill].0 as u32, m.runtime[skill].1 as u32);
        if want != got {
            out.push(Diff {
                id: "D-TOTALS",
                kind: Kind::Bug,
                count: 1,
                what: format!(
                    "{}: runtime totals {:?} against original rule {:?}",
                    SKILL_NAMES[skill], got, want
                ),
                cite: "crates/quake-core/src/level.rs count_authored",
            });
        }
    }
    out
}

fn table(out: &mut String, head: &[&str], rows: &[Vec<String>]) {
    let _ = writeln!(out, "| {} |", head.join(" | "));
    let _ = writeln!(
        out,
        "|{}|",
        head.iter().map(|_| " --- ").collect::<Vec<_>>().join("|")
    );
    for r in rows {
        let _ = writeln!(out, "| {} |", r.join(" | "));
    }
    out.push('\n');
}

fn category(class: &str) -> Option<&'static str> {
    Some(match class {
        "item_health" => "health",
        "item_armor1" | "item_armor2" | "item_armorInv" => "armor",
        "item_shells" | "item_spikes" | "item_rockets" | "item_cells" => "ammo",
        c if c.starts_with("weapon_") => "weapons",
        "item_key1" | "item_key2" => "keys",
        c if c.starts_with("item_artifact") => "artifacts",
        "item_sigil" => "runes",
        _ => return None,
    })
}

pub fn items_per_skill(census: &Census) -> BTreeMap<&'static str, [u32; 4]> {
    let mut out: BTreeMap<&'static str, [u32; 4]> = BTreeMap::new();
    for (class, row) in &census.classes {
        if let Some(cat) = category(class) {
            let slot = out.entry(cat).or_default();
            for s in 0..4 {
                slot[s] += row.per_skill[s];
            }
        }
    }
    out
}

pub fn map_report(m: &MapFacts<'_>, limits_ok: bool) -> String {
    let mut o = String::new();
    let diffs = differences(m);
    let _ = writeln!(o, "# {} parity: {}\n", m.name.to_uppercase(), m.title);
    let _ = writeln!(o, "Generated by `tools/parity`; do not edit by hand. Original: shareware 1.06 `PAK0.PAK`. Port: `id1psx/maps/{}.psb` plus the sources cited below. World type {}, CD track {}.\n", m.name, m.worldtype, m.cd_track);

    let _ = writeln!(o, "## 1. Original census\n");
    let _ = writeln!(o, "Skill rule of the original loader: bit 256 hides an entity on Easy, 512 on Normal, 1024 on Hard and Nightmare; bit 2048 (not in deathmatch) never hides one in single player. A crucified zombie (spawnflag 1) is wall decoration and is not counted by `total_monsters`.\n");
    let mut rows = Vec::new();
    for (class, r) in &m.census.classes {
        rows.push(vec![
            format!("`{class}`"),
            r.total.to_string(),
            r.per_skill[0].to_string(),
            r.per_skill[1].to_string(),
            r.per_skill[2].to_string(),
            r.per_skill[3].to_string(),
        ]);
    }
    table(
        &mut o,
        &["class", "authored", "Easy", "Normal", "Hard", "Nightmare"],
        &rows,
    );
    let mut rows = Vec::new();
    for skill in 0..4 {
        rows.push(vec![
            SKILL_NAMES[skill].into(),
            m.census.monsters[skill].to_string(),
            m.runtime[skill].0.to_string(),
            m.census.secrets[skill].to_string(),
            m.runtime[skill].1.to_string(),
            if (m.census.monsters[skill], m.census.secrets[skill])
                == (m.runtime[skill].0 as u32, m.runtime[skill].1 as u32)
            {
                "match".into()
            } else {
                "DIFFERENT".into()
            },
        ]);
    }
    let _ = writeln!(o, "Expected HUD totals (original rule) against what the shipping runtime computes from the cooked records with its own `count_authored` code:\n");
    table(
        &mut o,
        &[
            "skill",
            "kills (original)",
            "kills (runtime)",
            "secrets (original)",
            "secrets (runtime)",
            "result",
        ],
        &rows,
    );
    let items = items_per_skill(&m.census);
    let mut rows = Vec::new();
    for (cat, v) in &items {
        rows.push(vec![
            (*cat).into(),
            v[0].to_string(),
            v[1].to_string(),
            v[2].to_string(),
            v[3].to_string(),
        ]);
    }
    let _ = writeln!(o, "Pickups by category per skill (original rule):\n");
    table(
        &mut o,
        &["category", "Easy", "Normal", "Hard", "Nightmare"],
        &rows,
    );

    let _ = writeln!(o, "## 2. Cook\n");
    let _ = writeln!(o, "Original entities {}; cooked records {} (the cooker's rules predict {}). Class sequence and every field (origin, angles, model, spawnflags, sounds, count/lip/style, dmg, speed, height, health, wait, delay, message/map string, target ids) were recomputed from the original text with the cooker's conversions and compared with the cooked file: {} class mismatches, {} field mismatches. The cooker's target-name table shares suffixes, so two different names can collapse into one id: {} collisions ({} bytes of names).\n", m.entities.len(), m.records, m.check.expected_records, m.check.class_mismatches.len(), m.check.field_mismatches.len(), m.check.name_collisions.len(), m.check.name_table_bytes);
    for line in m
        .check
        .class_mismatches
        .iter()
        .chain(m.check.field_mismatches.iter())
        .take(20)
    {
        let _ = writeln!(o, "- {line}");
    }
    for (a, b) in &m.check.name_collisions {
        let _ = writeln!(o, "- target names `{a}` and `{b}` share one cooked id");
    }
    let mut rows = Vec::new();
    for (reason, classes) in &m.check.dropped {
        for (class, n) in classes {
            rows.push(vec![format!("`{class}`"), n.to_string(), (*reason).into()]);
        }
    }
    let _ = writeln!(o, "Dropped by the cooker:\n");
    if rows.is_empty() {
        let _ = writeln!(o, "Nothing.\n");
    } else {
        table(&mut o, &["class", "count", "reason"], &rows);
    }
    let mut keys: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (class, r) in &m.census.classes {
        for k in &r.keys {
            keys.entry(k.as_str()).or_default().insert(class.as_str());
        }
    }
    let mut rows = Vec::new();
    for (key, classes) in &keys {
        let rule = KEY_RULES.iter().find(|r| r.key == *key);
        let list = if matches!(*key, "classname" | "origin" | "spawnflags") {
            format!("{} classes", classes.len())
        } else {
            classes
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        rows.push(match rule {
            Some(r) => vec![
                format!("`{key}`"),
                r.cooked_as.into(),
                r.runtime.into(),
                list,
            ],
            None => vec![
                format!("`{key}`"),
                "UNCLASSIFIED".into(),
                "review".into(),
                list,
            ],
        });
    }
    let _ = writeln!(
        o,
        "Authored keys in this map, where the cook puts them and what reads them:\n"
    );
    table(
        &mut o,
        &["key", "cooked as", "runtime use", "carried by"],
        &rows,
    );

    let _ = writeln!(o, "## 3. Runtime handling\n");
    let mut rows = Vec::new();
    for class in m.census.classes.keys() {
        match CLASS_RULES
            .iter()
            .find(|r| r.classes.contains(&class.as_str()))
        {
            Some(r) => rows.push(vec![
                format!("`{class}`"),
                r.handling.label().into(),
                r.note.into(),
                format!("`{}` `{}`", r.cite.file, r.cite.symbol),
            ]),
            None => rows.push(vec![
                format!("`{class}`"),
                "NO RULE".into(),
                "not reviewed".into(),
                "-".into(),
            ]),
        }
    }
    table(
        &mut o,
        &["class", "handling", "what it does", "evidence"],
        &rows,
    );
    let mut rows = Vec::new();
    for (class, r) in &m.census.classes {
        for value in &r.flag_values {
            if *value == 0 {
                continue;
            }
            let mut notes = Vec::new();
            for bit in 0..8 {
                let b = 1u32 << bit;
                if value & b == 0 {
                    continue;
                }
                match FLAG_RULES
                    .iter()
                    .find(|f| f.classes.contains(&class.as_str()) && f.bit & b != 0)
                {
                    Some(f) => notes.push(format!(
                        "bit {b} {} ({}; `{}`)",
                        f.meaning,
                        f.handling.label(),
                        f.cite.symbol
                    )),
                    None => notes.push(format!("bit {b} UNREVIEWED")),
                }
            }
            rows.push(vec![
                format!("`{class}`"),
                value.to_string(),
                notes.join("; "),
            ]);
        }
    }
    let _ = writeln!(o, "Spawnflag values (low 8 bits) authored in this map:\n");
    if rows.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        table(&mut o, &["class", "value", "meaning and runtime"], &rows);
    }

    let _ = writeln!(o, "## 4. Triggers and target graph\n");
    let g = &m.graph;
    let _ = writeln!(o, "Edges from `target`/`killtarget` to a matching `targetname`, kept entities only, lights excluded (their targets are compile-time aim points): {} by the original names, {} by the cooked ids ({}).\n", g.original_edges, g.cooked_edges, if g.original_edges == g.cooked_edges { "equal" } else { "DIFFERENT" });
    let mut rows = Vec::new();
    for ((a, b), n) in &g.edges {
        rows.push(vec![format!("`{a}`"), format!("`{b}`"), n.to_string()]);
    }
    table(&mut o, &["source class", "target class", "edges"], &rows);
    let _ = writeln!(o, "Targets that resolve to nothing in the original either: {}. Targets that resolve only to entities the cooker drops: {}.\n", g.unresolved.len(), g.only_dropped.len());
    for line in g.unresolved.iter().chain(g.only_dropped.iter()).take(30) {
        let _ = writeln!(o, "- {line}");
    }
    if !g.unresolved.is_empty() || !g.only_dropped.is_empty() {
        o.push('\n');
    }
    if !g.teleports.is_empty() {
        let missing = g.teleports.iter().filter(|(_, n)| *n == 0).count();
        let multi = g.teleports.iter().filter(|(_, n)| *n > 1).count();
        let _ = writeln!(o, "Teleporters: {} triggers, {} without a destination, {} with several (the original and the runtime both take the first).\n", g.teleports.len(), missing, multi);
    }
    if !g.trains.is_empty() {
        let rows: Vec<Vec<String>> = g
            .trains
            .iter()
            .map(|t| {
                vec![
                    t.label.clone(),
                    t.corners.to_string(),
                    if t.looped {
                        "loops".into()
                    } else {
                        "ends".into()
                    },
                ]
            })
            .collect();
        table(&mut o, &["func_train", "path corners", "chain"], &rows);
    }
    if g.patrols + g.death_targets > 0 {
        let _ = writeln!(o, "Monsters with a target: {} patrol a path_corner chain, {} fire the target when they die (`monster_death_use`; `fire_monster_death_targets`).\n", g.patrols, g.death_targets);
    }
    if !g.changelevels.is_empty() {
        let rows: Vec<Vec<String>> = g
            .changelevels
            .iter()
            .map(|(map, flags, target)| {
                vec![
                    format!("`{map}`"),
                    flags.to_string(),
                    if *target { "yes".into() } else { "no".into() },
                ]
            })
            .collect();
        table(
            &mut o,
            &["changelevel map", "spawnflags", "fires a target"],
            &rows,
        );
    }
    if !g.counters.is_empty() {
        let rows: Vec<Vec<String>> = g
            .counters
            .iter()
            .map(|(n, c, f)| vec![format!("`{n}`"), c.to_string(), f.to_string()])
            .collect();
        table(
            &mut o,
            &["trigger_counter", "count", "entities feeding it"],
            &rows,
        );
    }

    let _ = writeln!(o, "## 5. Sounds\n");
    let s = &m.sounds;
    let _ = writeln!(o, "The cooker picks samples per class from `tools/cfg/id1/reslist.txt` (all original entities, any skill). Expected samples {}; cooked {} ({} map-local plus the global bank); SPU high-water {:#x} of 0x80000. Expected but not cooked: {}. Cooked but not expected: {}. CD track {} is not played.\n", s.expected.len(), s.cooked.len(), s.local_count, s.spu_high_water, s.missing.len(), s.extra.len(), m.cd_track);
    for name in &s.missing {
        let _ = writeln!(o, "- missing: `{name}`");
    }
    for name in &s.extra {
        let _ = writeln!(o, "- extra: `{name}`");
    }
    let amb: u32 = s.ambient.values().sum();
    let _ = writeln!(
        o,
        "Ambient sources: {} ({}) against a cap of 32 sources and 11 voices.\n",
        amb,
        s.ambient
            .iter()
            .map(|(c, n)| format!("{c} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut rows = Vec::new();
    for class in [
        "func_door",
        "func_door_secret",
        "func_plat",
        "func_button",
        "func_train",
        "trigger_once",
        "trigger_multiple",
        "trigger_secret",
        "item_key1",
        "item_key2",
    ] {
        if let Some(r) = m.census.classes.get(class) {
            let mut hist: BTreeMap<i64, u32> = BTreeMap::new();
            for e in m.entities.iter().filter(|e| e.class() == class) {
                *hist.entry(e.int("sounds").unwrap_or(-1)).or_default() += 1;
            }
            let h = hist
                .iter()
                .map(|(k, v)| {
                    if *k < 0 {
                        format!("unset x{v}")
                    } else {
                        format!("{k} x{v}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            rows.push(vec![format!("`{class}`"), r.total.to_string(), h]);
        }
    }
    let _ = writeln!(
        o,
        "`sounds` selectors authored (the original picks a sample set from each):\n"
    );
    table(&mut o, &["class", "count", "selectors"], &rows);

    let _ = writeln!(o, "## 6. Differences\n");
    let rows: Vec<Vec<String>> = diffs
        .iter()
        .map(|d| {
            vec![
                d.id.into(),
                d.kind.label().into(),
                d.count.to_string(),
                d.what.clone(),
                d.cite.into(),
            ]
        })
        .collect();
    table(
        &mut o,
        &["id", "status", "count", "difference", "evidence"],
        &rows,
    );
    let _ = writeln!(
        o,
        "Citations in sections 3 and 6 checked against the source tree: {}.",
        if limits_ok {
            format!("all {} resolve", evidence::citations_checked())
        } else {
            "SOME DO NOT RESOLVE, see the tool output".into()
        }
    );
    let _ = Handling::Full;
    o
}
