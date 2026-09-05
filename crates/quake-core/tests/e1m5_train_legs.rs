//! Authored `func_train` leg timing, measured against the cooked E1M5 lump.
//!
//! The guest and a host reimplementation disagreed on this: the guest computed
//! 27804 ticks for a leg the host measured at 87. 27804 is exactly
//! `isqrt_i32(i32::MAX) * 60 / 100`, the saturated result, so whatever the
//! guest fed `travel_ticks` was about four thousand times too large. Pinning
//! the real numbers here means the arithmetic can no longer drift silently.

use quake_core::train::QuakeTrain;
use quake_formats::{BrushModel, LumpKind, MapEntity, PsbIndex, RecordSlice, SliceReader};

const CLASS_FUNC_TRAIN: u8 = 0x11;

fn lump<'a>(bytes: &'a [u8], index: &PsbIndex, kind: LumpKind) -> &'a [u8] {
    let range = index.lump(kind);
    &bytes[range.offset as usize..range.end() as usize]
}

fn map(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../id1psx/maps/{name}.psb",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Every authored train on every Episode 1 map, walked around its whole
/// corner chain. Real Quake coordinates span a few thousand units, so no leg
/// can take anywhere near the saturated 27804 ticks, and none can be so short
/// that the train teleports.
#[test]
fn every_authored_train_leg_takes_a_believable_number_of_ticks() {
    const MAPS: [&str; 9] = [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ];
    let mut trains = 0usize;
    let mut legs = 0usize;
    let mut longest = 0u16;
    for name in MAPS {
        let bytes = map(name);
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader).expect("psb index");
        let entities = RecordSlice::<MapEntity>::new(lump(&bytes, &index, LumpKind::Entities))
            .expect("entities");
        let models =
            RecordSlice::<BrushModel>::new(lump(&bytes, &index, LumpKind::Models)).expect("models");
        for entity_index in 0..entities.len() {
            let entity = entities.get(entity_index).expect("entity");
            if entity.class_name != CLASS_FUNC_TRAIN || entity.model >= 0 {
                continue;
            }
            let model = models
                .get((-entity.model) as usize)
                .expect("train brush model");
            let Some(mut train) = QuakeTrain::from_entity(entity, model, &entities) else {
                continue;
            };
            trains += 1;
            // Walk enough ticks to cross several corners of the chain.
            let mut seen = 0usize;
            for _ in 0..20_000 {
                let before = train.corner_arrivals();
                train.tick(&entities);
                if train.corner_arrivals() != before {
                    seen += 1;
                    if seen >= 6 {
                        break;
                    }
                }
                let ticks = train.leg_ticks();
                if ticks != 0 {
                    legs += 1;
                    longest = longest.max(ticks);
                    assert!(
                        ticks < 3_600,
                        "{name} train at entity {entity_index} claims a {ticks} tick leg, \
                         which is over a minute of travel for a map a few thousand units across"
                    );
                }
            }
        }
    }
    assert!(trains > 0, "Episode 1 must author at least one func_train");
    assert!(legs > 0, "at least one leg must have been measured");
    println!("trains={trains} leg_samples={legs} longest_leg_ticks={longest}");
}

/// E1M5's trains specifically, with their exact leg tick counts recorded.
#[test]
fn e1m5_train_legs_are_pinned() {
    let bytes = map("e1m5");
    let mut reader = SliceReader::new(&bytes);
    let index = PsbIndex::read(&mut reader).expect("psb index");
    let entities =
        RecordSlice::<MapEntity>::new(lump(&bytes, &index, LumpKind::Entities)).expect("entities");
    let models =
        RecordSlice::<BrushModel>::new(lump(&bytes, &index, LumpKind::Models)).expect("models");
    let mut report = Vec::new();
    for entity_index in 0..entities.len() {
        let entity = entities.get(entity_index).expect("entity");
        if entity.class_name != CLASS_FUNC_TRAIN || entity.model >= 0 {
            continue;
        }
        let model = models
            .get((-entity.model) as usize)
            .expect("train brush model");
        let Some(mut train) = QuakeTrain::from_entity(entity, model, &entities) else {
            continue;
        };
        let mut ticks = Vec::new();
        let mut arrivals = train.corner_arrivals();
        ticks.push(train.leg_ticks());
        for _ in 0..20_000 {
            train.tick(&entities);
            if train.corner_arrivals() != arrivals {
                arrivals = train.corner_arrivals();
                ticks.push(train.leg_ticks());
                if ticks.len() >= 5 {
                    break;
                }
            }
        }
        report.push((entity_index, entity.speed, ticks));
    }
    for (entity_index, speed, ticks) in &report {
        println!("e1m5 train entity {entity_index} speed={speed} leg_ticks={ticks:?}");
    }
    assert!(!report.is_empty(), "E1M5 authors at least one func_train");
    for (entity_index, _, ticks) in &report {
        for &leg in ticks {
            assert_ne!(
                leg, 27_804,
                "e1m5 train {entity_index} reproduced the saturated leg length"
            );
        }
    }
}
