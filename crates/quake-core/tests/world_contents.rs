//! `SV_PointContents` for the world: the leaf a point is in.
//!
//! The guest used to read it by walking the clip nodes from `head_nodes[0]`.
//! Clip nodes exist for hulls 1 and 2; hull 0 is the render tree, so that walk
//! read a point of open air as solid about one time in fifteen and never read
//! a sky leaf as sky. Rockets, nails and Chthon's lava balls decide whether a
//! sky impact is silent from exactly this value, so E1M5, the only shareware
//! map with sky leaves, kept exploding on its sky.
//!
//! The rule the guest now follows, the leaf the point is in, is pinned here
//! against the cooked maps, with the clip-node walk's disagreement beside it.

use quake_core::collision::{CollisionHull, CONTENTS_EMPTY, CONTENTS_SKY, CONTENTS_SOLID};
use quake_formats::{resident::ResidentMap, SliceReader, Vec3I32};

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

fn load(index: usize) -> ResidentMap {
    let path = format!(
        "{}/../../id1psx/maps/{}.psb",
        env!("CARGO_MANIFEST_DIR"),
        MAPS[index]
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut reader = SliceReader::new(&bytes);
    let mut map = ResidentMap::new();
    map.load(index as u32, &mut reader)
        .expect("load cooked map");
    map
}

fn leaf_contents(map: &ResidentMap, point: Vec3I32) -> Option<i16> {
    let leaf = map.point_leaf_index(point)?;
    Some(map.leaves().get(leaf)?.contents)
}

fn clip_walk_contents(map: &ResidentMap, point: Vec3I32) -> Option<i16> {
    let world = map.brush_models().get(0)?;
    CollisionHull::new(map.planes(), map.clip_nodes(), world.head_nodes[0])?.point_contents(point)
}

fn sample_points(map: &ResidentMap) -> impl Iterator<Item = Vec3I32> {
    let world = map.brush_models().get(0).expect("world model");
    let step = 48usize;
    let range = move |low: i16, high: i16| (i32::from(low)..i32::from(high)).step_by(step);
    let (mins, maxs) = (world.mins, world.maxs);
    range(mins.x, maxs.x).flat_map(move |x| {
        range(mins.y, maxs.y).flat_map(move |y| {
            range(mins.z, maxs.z).map(move |z| Vec3I32 {
                x: x << 12,
                y: y << 12,
                z: z << 12,
            })
        })
    })
}

/// The clip-node walk is not the world's contents: pin the disagreement so
/// nobody reintroduces it for a contents question.
#[test]
fn the_clip_node_walk_reads_open_air_as_solid() {
    let map = load(1);
    let mut empty = 0u32;
    let mut read_solid = 0u32;
    for point in sample_points(&map) {
        if leaf_contents(&map, point) == Some(CONTENTS_EMPTY) {
            empty += 1;
            if clip_walk_contents(&map, point) == Some(CONTENTS_SOLID) {
                read_solid += 1;
            }
        }
    }
    assert!(empty > 1_000);
    assert!(
        read_solid * 100 / empty >= 5,
        "{read_solid} of {empty} open points read solid"
    );
}

/// Only E1M5 has sky leaves. The leaf contents reads every sampled point of
/// them as sky; the clip-node walk reads none of them as sky, so the old guest
/// check could never fire.
#[test]
fn only_the_leaf_walk_finds_e1m5s_sky() {
    for (index, name) in MAPS.iter().enumerate() {
        let map = load(index);
        let sky_leaves = (0..map.leaves().len())
            .filter(|&leaf| map.leaves().get(leaf).unwrap().contents == CONTENTS_SKY)
            .count();
        assert_eq!(
            sky_leaves > 0,
            *name == "e1m5",
            "{name}: {sky_leaves} sky leaves"
        );
    }
    let map = load(5);
    let mut sky_points = 0u32;
    let mut clip_walk_sky = 0u32;
    for point in sample_points(&map) {
        if leaf_contents(&map, point) == Some(CONTENTS_SKY) {
            sky_points += 1;
            if clip_walk_contents(&map, point) == Some(CONTENTS_SKY) {
                clip_walk_sky += 1;
            }
        }
    }
    assert!(sky_points >= 6, "{sky_points} sampled sky points");
    assert_eq!(clip_walk_sky, 0);
}
