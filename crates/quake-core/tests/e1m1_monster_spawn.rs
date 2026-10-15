//! Where every E1M1 monster stands once the level has loaded.
//!
//! `walkmonster_start_go` raises a monster one unit and calls `droptofloor`:
//! the monster's own hull is swept 256 units down and the origin lands on the
//! first floor it meets. Nothing else moves a monster before the player's
//! first frame, so these positions are what the player meets, and they are
//! fully determined by the authored origin and the clip hull.
//!
//! The table pins the dropped origin of every authored walking monster in
//! the cooked map. It also pins the property the table cannot show by itself:
//! a monster drops straight down, so x and y never move from the authored
//! origin and z only ever decreases. The guest's own drop (`EntityScene::load`)
//! is checked against the same table by `cargo quake-build e1m1-spawn-regress`.

use quake_core::collision::{CollisionHull, Trace, TraceScratch, Q12_ONE};
use quake_core::monster::MonsterKind;
use quake_formats::resident::ResidentMap;
use quake_formats::{LumpKind, MapEntity, PsbIndex, RecordSlice, SliceReader, Vec3I32};

/// `(cooked source index, authored x, y, z, dropped z, stands on a mover)` in
/// whole units. Skill flags are not applied: the drop depends only on the
/// geometry. The world hull is all this test traces, so a monster authored on
/// top of a brush mover shows the pit floor below it here and rests on the
/// mover in the running game (the last column): monster 80 is placed on the
/// closed `t1` lift door, whose top is the authored origin height.
const EXPECTED: &[(u16, i32, i32, i32, i32, bool)] = &[
    (5, 616, 72, 40, 40, false),
    (21, 248, 2392, 40, 40, false),
    (80, 0, 576, 24, -216, true),
    (81, 8, 1520, -200, -200, false),
    (82, 88, 1520, -200, -200, false),
    (83, 224, 1552, -200, -200, false),
    (84, -8, 936, -200, -200, false),
    (85, 648, 736, 104, 104, false),
    (89, 1312, 936, -248, -256, false),
    (90, 1336, 1784, -408, -408, false),
    (91, 1392, 928, -248, -256, false),
    (92, 1384, 1008, -248, -256, false),
    (93, 1240, 1008, -248, -256, false),
    (94, 1256, 1760, -408, -408, false),
    (95, 824, 1784, -408, -408, false),
    (96, 1128, 1760, -408, -408, false),
    (99, 1232, 2088, -216, -216, false),
    (100, 1232, 2448, -280, -280, false),
    (101, 832, 2464, -344, -344, false),
    (102, 832, 2072, -408, -408, false),
    (103, 840, 1960, -408, -408, false),
    (109, 416, 1912, -168, -168, false),
    (110, 432, 2120, -168, -168, false),
    (115, 80, 2024, -184, -184, false),
    (116, -16, 1888, -184, -184, false),
    (117, -248, 2144, -136, -136, false),
    (120, -432, 2352, 56, 56, false),
    (121, -544, 2584, 56, 56, false),
    (122, -344, 2656, -104, -104, false),
    (123, -72, 2896, -56, -56, false),
    (124, 432, 2920, -56, -56, false),
    (125, 424, 2832, -56, -56, false),
    (130, 424, 2672, -56, -56, false),
    (131, 424, 2880, -56, -56, false),
    (132, 424, 2760, -56, -56, false),
    (135, 848, 2584, -72, -72, false),
    (136, 824, 2008, -152, -152, false),
    (138, 248, 2352, 40, 40, false),
    (139, -72, 2464, 40, 40, false),
    (140, 904, 1024, -248, -248, false),
    (155, 288, 1536, -200, -200, false),
    (156, 968, 2432, -112, -112, false),
];

fn units(value: i32) -> i32 {
    value >> 12
}

fn e1m1() -> Vec<u8> {
    let path = format!("{}/../../id1psx/maps/e1m1.psb", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// `(source index, authored origin, hull index)` of every monster that
/// `walkmonster_start` drops: not Chthon (his own rise places him), not a
/// crucified zombie (decoration), and no flyer.
fn droppers(bytes: &[u8]) -> Vec<(u16, MapEntity, usize)> {
    let mut reader = SliceReader::new(bytes);
    let index = PsbIndex::read(&mut reader).expect("psb index");
    let range = index.lump(LumpKind::Entities);
    let entities =
        RecordSlice::<MapEntity>::new(&bytes[range.offset as usize..range.end() as usize])
            .expect("entities");
    let mut found = Vec::new();
    for source in 0..entities.len() {
        let entity = entities.get(source).expect("entity");
        let Some(kind) = MonsterKind::from_class_name(entity.class_name) else {
            continue;
        };
        if kind.is_boss() || kind.flies() {
            continue;
        }
        // Spawnflag 1 on a zombie is "crucified", not "ambush".
        if kind == MonsterKind::Zombie && entity.spawn_flags & 1 != 0 {
            continue;
        }
        found.push((source as u16, entity, kind.collision_hull()));
    }
    found
}

fn dropped(map: &ResidentMap, origin: Vec3I32, hull_index: usize) -> Vec3I32 {
    let world = map.brush_models().get(0).expect("world model");
    let hull = CollisionHull::new(map.planes(), map.clip_nodes(), world.head_nodes[hull_index])
        .expect("monster hull");
    let start = Vec3I32 {
        z: origin.z + (1 << 12),
        ..origin
    };
    let end = Vec3I32 {
        z: start.z - (256 << 12),
        ..start
    };
    let mut scratch = TraceScratch::default();
    let mut trace = Trace::default();
    assert!(hull.trace_into(&start, &end, &mut scratch, &mut trace));
    assert!(
        !trace.all_solid && trace.fraction < Q12_ONE,
        "no floor under {origin:?}"
    );
    trace.end
}

#[test]
fn every_e1m1_monster_drops_straight_to_the_floor() {
    let bytes = e1m1();
    let mut reader = SliceReader::new(&bytes);
    let mut map = ResidentMap::new();
    map.load(1, &mut reader).expect("load cooked E1M1");
    let mut rows = Vec::new();
    for (source, entity, hull) in droppers(&bytes) {
        let floor = dropped(&map, entity.origin, hull);
        assert_eq!(floor.x, entity.origin.x, "monster {source} slid in x");
        assert_eq!(floor.y, entity.origin.y, "monster {source} slid in y");
        assert!(
            floor.z <= entity.origin.z + (1 << 12),
            "monster {source} rose from {} to {}",
            units(entity.origin.z),
            units(floor.z)
        );
        rows.push((
            source,
            units(entity.origin.x),
            units(entity.origin.y),
            units(entity.origin.z),
            units(floor.z),
        ));
    }
    let world_rows: Vec<_> = EXPECTED
        .iter()
        .map(|&(source, x, y, z, floor, _)| (source, x, y, z, floor))
        .collect();
    assert_eq!(rows, world_rows);
}
