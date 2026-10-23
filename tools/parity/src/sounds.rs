//! Sound parity of one map: what the original would load for the classes the
//! map places, against what the cooked bank holds.

use std::collections::{BTreeMap, BTreeSet};

use crate::cooked::{CookedMap, Resources};
use crate::pak::Entity;

pub struct SoundInfo {
    pub expected: BTreeSet<String>,
    pub cooked: BTreeSet<String>,
    pub missing: Vec<String>,
    pub extra: Vec<String>,
    pub ambient: BTreeMap<String, u32>,
    pub local_count: usize,
    pub spu_high_water: u32,
}

pub fn analyse(
    entities: &[Entity],
    cooked: &CookedMap,
    res: &Resources,
    sfx_names: &BTreeMap<u16, String>,
) -> SoundInfo {
    let world_slot = entities[0].int("worldtype").unwrap_or(0).clamp(0, 2) as usize;
    let mut expected = BTreeSet::new();
    for e in entities {
        let Some(rules) = res.sfx.get(e.class()) else {
            continue;
        };
        for rule in rules {
            if rule
                .when_sounds
                .is_some_and(|n| e.int("sounds").unwrap_or(0) != n as i64)
            {
                continue;
            }
            let pick = rule.names.get(world_slot).or_else(|| rule.names.first());
            if let Some(name) = pick {
                expected.insert(name.clone());
            }
        }
    }
    // The global bank (the non-variant worldspawn sounds) is always resident.
    let mut global = BTreeSet::new();
    if let Some(rules) = res.sfx.get("worldspawn") {
        for rule in rules {
            if let Some(name) = rule.names.first() {
                global.insert(name.clone());
            }
        }
    }
    let mut cooked_names: BTreeSet<String> = global;
    for id in &cooked.local_sounds {
        if let Some(name) = sfx_names.get(id) {
            cooked_names.insert(name.clone());
        }
    }
    let missing = expected.difference(&cooked_names).cloned().collect();
    let extra = cooked_names.difference(&expected).cloned().collect();
    let mut ambient: BTreeMap<String, u32> = BTreeMap::new();
    for e in entities {
        if e.class().starts_with("ambient_") {
            *ambient.entry(e.class().to_owned()).or_default() += 1;
        }
    }
    SoundInfo {
        expected,
        cooked: cooked_names,
        missing,
        extra,
        ambient,
        local_count: cooked.local_sounds.len(),
        spu_high_water: cooked.spu_high_water,
    }
}
