//! Regression for the E1M1 clipping-hull seam captured in PSoXide slot 0 on
//! 2026-08-14. The shipping player could enter this wedge but, without
//! Quake's `SV_TryUnstick`, could move only between x=400 and x=424.

use quake_core::collision::CollisionHull;
use quake_core::movement::{MovementInput, MovementScratch, MovementState};
use quake_formats::resident::ResidentMap;
use quake_formats::{SliceReader, Vec3I32};

fn q12(value: f64) -> i32 {
    (value * 4096.0).round() as i32
}

#[test]
fn e1m1_precision_wedge_uses_quakes_unstick_recovery() {
    let map_path = format!("{}/../../id1psx/maps/e1m1.psb", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&map_path).expect("read cooked E1M1");
    let mut reader = SliceReader::new(&bytes);
    let mut map = ResidentMap::new();
    map.load(1, &mut reader).expect("load cooked E1M1");
    let world = map.brush_models().get(0).expect("world model");
    let collision = CollisionHull::new(map.planes(), map.clip_nodes(), world.head_nodes[1])
        .expect("E1M1 player hull");

    // Q20.12 position and yaw from the recorded failure. This is free space,
    // but its angled hull planes stop an ordinary step-slide almost at once.
    let saved_origin = Vec3I32 {
        x: 1_736_627,
        y: 3_080_208,
        z: 358_509,
    };
    assert_eq!(
        (q12(423.981_201), q12(752.003_906), q12(87.526_611)),
        (saved_origin.x, saved_origin.y, saved_origin.z)
    );
    let mut state = MovementState::new(saved_origin);
    let mut scratch = MovementScratch::new();
    let input = MovementInput {
        forward: -127,
        strafe: 0,
        yaw: 769,
        pitch: 0,
        jump: false,
    };
    let leaves = map.leaves();

    for _ in 0..120 {
        let _ = state.update_ticks(&collision, &mut scratch, input, 1, |point| {
            map.point_leaf_index(*point)
                .and_then(|leaf| leaves.get(leaf))
                .map(|leaf| leaf.contents)
        });
    }

    let escaped = state.origin();
    assert!(
        escaped.y < 748 << 12,
        "player remained in the E1M1 wedge: start={saved_origin:?}, end={escaped:?}"
    );
    assert!(
        (escaped.x - saved_origin.x).abs() > 4 << 12
            || (escaped.y - saved_origin.y).abs() > 4 << 12,
        "Quake's recovery must make more than four units of progress"
    );
}
