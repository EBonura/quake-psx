//! Three of E1M1's sixteen-unit rises have risers that lean about seven
//! degrees (plane normal z 0.124, not Quake's exact zero). Standing at the
//! foot and starting to walk moves the player under a unit per tick, which
//! used to be too little to carry the raised box past the lean, so the player
//! stood and flickered between grounded and airborne forever.

use quake_core::collision::CollisionHull;
use quake_core::movement::{MovementInput, MovementScratch, MovementState, DEFAULT_GRAVITY};
use quake_formats::resident::ResidentMap;
use quake_formats::{SliceReader, Vec3I32};

/// `(start x, y, z, heading x, y, goal x, y, z)` in whole units, taken from the
/// routesim `ramps` sweep that found them.
const RISES: &[(i32, i32, i32, i32, i32, i32, i32, i32)] = &[
    (1281, 1969, -191, 0, 1, 1281, 1985, -175),
    (721, 2001, -127, 1, 0, 737, 2001, -111),
    (1313, 2497, -255, -1, 0, 1297, 2497, -239),
];

#[test]
fn the_leaning_sixteen_unit_rises_are_walked_up_from_standstill_at_every_tick_rate() {
    let map_path = format!("{}/../../id1psx/maps/e1m1.psb", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&map_path).expect("read cooked E1M1");
    let mut reader = SliceReader::new(&bytes);
    let mut map = ResidentMap::new();
    map.load(1, &mut reader).expect("load cooked E1M1");
    let world = map.brush_models().get(0).expect("world model");
    let collision = CollisionHull::new(map.planes(), map.clip_nodes(), world.head_nodes[1])
        .expect("E1M1 player hull");
    let leaves = map.leaves();

    for &(sx, sy, sz, dx, dy, gx, gy, gz) in RISES {
        for ticks in 1..=4u16 {
            let mut state = MovementState::new(Vec3I32 {
                x: sx << 12,
                y: sy << 12,
                z: sz << 12,
            });
            let mut scratch = MovementScratch::new();
            let input = MovementInput {
                forward: 127,
                strafe: 0,
                yaw: psx_math::atan2_q12(dy, dx) & 0x0fff,
                pitch: 0,
                jump: false,
            };
            let mut crossed = None;
            for _ in 0..(48 / ticks).max(8) {
                let _ = state.update_ticks_with_gravity(
                    &collision,
                    &mut scratch,
                    input,
                    ticks,
                    DEFAULT_GRAVITY,
                    |point| {
                        map.point_leaf_index(*point)
                            .and_then(|leaf| leaves.get(leaf))
                            .map(|leaf| leaf.contents)
                    },
                );
                let end = state.origin();
                let along = ((end.x >> 12) - sx) * dx + ((end.y >> 12) - sy) * dy;
                if along >= 12 {
                    crossed = Some(end);
                    break;
                }
            }
            let end = crossed.unwrap_or_else(|| {
                panic!(
                    "ticks {ticks}: from ({sx},{sy},{sz}) toward ({gx},{gy},{gz}) the player stopped at {:?}",
                    state.origin()
                )
            });
            let rise = (end.z >> 12) - sz;
            assert!(
                rise >= 14,
                "ticks {ticks}: from ({sx},{sy},{sz}) the player crossed the rise at {end:?} but only rose {rise}"
            );
        }
    }
}
