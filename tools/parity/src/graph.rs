//! Target graph, teleports, trains, changelevels and counters of one map.

use std::collections::{BTreeMap, BTreeSet};

use crate::analysis::Placement;
use crate::cooked::CookedMap;
use crate::pak::Entity;

pub struct Train {
    pub label: String,
    pub corners: usize,
    pub looped: bool,
}

#[derive(Default)]
pub struct GraphInfo {
    /// `(source class, target class)` -> edge count, original names, kept entities.
    pub edges: BTreeMap<(String, String), u32>,
    pub unresolved: Vec<String>,
    pub only_dropped: Vec<String>,
    pub original_edges: usize,
    pub cooked_edges: usize,
    pub teleports: Vec<(String, usize)>,
    pub trains: Vec<Train>,
    pub patrols: usize,
    pub death_targets: usize,
    pub changelevels: Vec<(String, i64, bool)>,
    pub counters: Vec<(String, i64, usize)>,
}

fn is_light(class: &str) -> bool {
    class.starts_with("light")
}

pub fn analyse(entities: &[Entity], placements: &[Placement], cooked: &CookedMap) -> GraphInfo {
    let kept = |i: usize| placements[i].record.is_some();
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, e) in entities.iter().enumerate() {
        if let Some(n) = e.get("targetname") {
            by_name.entry(n).or_default().push(i);
        }
    }
    let mut g = GraphInfo::default();
    for (i, e) in entities.iter().enumerate().skip(1) {
        if !kept(i) || is_light(e.class()) {
            continue;
        }
        for key in ["target", "killtarget"] {
            let Some(value) = e.get(key) else { continue };
            if key == "target" && e.class() == "event_lightning" {
                continue;
            }
            if key == "target" && value == "lightning" && e.class() == "func_door" {
                // The implicit name `event_lightning` searches for; nothing carries it as a targetname.
                continue;
            }
            let label = format!("#{i} {} {key}={value}", e.class());
            match by_name.get(value) {
                None => g.unresolved.push(label),
                Some(list) => {
                    let live: Vec<usize> = list.iter().copied().filter(|j| kept(*j)).collect();
                    if live.is_empty() {
                        g.only_dropped.push(label);
                    }
                    for j in live {
                        g.original_edges += 1;
                        *g.edges
                            .entry((e.class().to_owned(), entities[j].class().to_owned()))
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    // The same edges, read back from the cooked ids.
    for (i, r) in cooked.records.iter().enumerate() {
        if r.class_name == 0x09 {
            continue;
        }
        let _ = i;
        for id in [r.target, r.kill_target] {
            if id == 0 {
                continue;
            }
            g.cooked_edges += cooked
                .records
                .iter()
                .filter(|o| o.target_name == id)
                .count();
        }
    }
    // Cooked light records carry their own target ids; the original edges skip
    // lights, so remove theirs from the cooked count for a like-for-like check.
    let mut light_edges = 0;
    for p in placements {
        let Some(slot) = p.record else { continue };
        let e = &entities[p.original];
        if !is_light(e.class()) {
            continue;
        }
        let r = &cooked.records[slot];
        for id in [r.target, r.kill_target] {
            if id != 0 {
                light_edges += cooked
                    .records
                    .iter()
                    .filter(|o| o.target_name == id)
                    .count();
            }
        }
    }
    g.cooked_edges -= light_edges;

    let first_named = |name: &str, class: &str| -> Option<usize> {
        by_name
            .get(name)?
            .iter()
            .copied()
            .find(|j| entities[*j].class() == class)
    };
    for (i, e) in entities.iter().enumerate() {
        if !kept(i) {
            continue;
        }
        match e.class() {
            "trigger_teleport" => {
                let t = e.get("target").unwrap_or("");
                let n = by_name.get(t).map_or(0, |l| {
                    l.iter()
                        .filter(|j| entities[**j].class() == "info_teleport_destination")
                        .count()
                });
                g.teleports.push((t.to_owned(), n));
            }
            "func_train" => {
                let mut seen = BTreeSet::new();
                let mut cur = e.get("target").and_then(|t| first_named(t, "path_corner"));
                let mut looped = false;
                while let Some(c) = cur {
                    if !seen.insert(c) {
                        looped = true;
                        break;
                    }
                    cur = entities[c]
                        .get("target")
                        .and_then(|t| first_named(t, "path_corner"));
                }
                g.trains.push(Train {
                    label: format!("#{i} target={}", e.get("target").unwrap_or("-")),
                    corners: seen.len(),
                    looped,
                });
            }
            "trigger_changelevel" => {
                g.changelevels.push((
                    e.get("map").unwrap_or("").to_owned(),
                    e.spawnflags() as i64 & 0xff,
                    e.get("target").is_some(),
                ));
            }
            "trigger_counter" => {
                let name = e.get("targetname").unwrap_or("");
                let feeders = entities
                    .iter()
                    .enumerate()
                    .filter(|(j, o)| kept(*j) && o.get("target") == Some(name) && !name.is_empty())
                    .count();
                g.counters
                    .push((name.to_owned(), e.int("count").unwrap_or(0), feeders));
            }
            c if c.starts_with("monster_") && c != "monster_boss" => {
                if let Some(t) = e.get("target") {
                    if first_named(t, "path_corner").is_some() {
                        g.patrols += 1;
                    } else {
                        g.death_targets += 1;
                    }
                }
            }
            _ => {}
        }
    }
    g
}
