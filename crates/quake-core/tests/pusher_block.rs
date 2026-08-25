//! `SV_PushMove`'s failure path over a real authored `func_train`.
//!
//! The original does not let a blocked pusher keep its move. It puts the
//! pusher back where it was, puts every body it pushed back where they were,
//! rewinds the pusher's own local time, and only then runs the `blocked`
//! function. `train_blocked` not reversing direction is a separate thing: the
//! train tries the same leg again on the next tick, it does not keep an
//! intersecting position it was refused.
//!
//! This runs the exact sequence the game layer runs (snapshot, advance, push,
//! roll back on a block) against E1M5's own trains, which start moving at load
//! with no `targetname`, so nothing here is a synthetic fixture.

use quake_core::collision::TraceScratch;
use quake_core::movement::{MovementTrace, MovementTraceResult};
use quake_core::push::{penetrates, push_move, BlockCrush, RiderBody};
use quake_core::train::{QuakeTrain, TrainState, TRAIN_BLOCK_COOLDOWN_TICKS};
use quake_formats::{
    BrushModel, LumpKind, MapEntity, PsbIndex, RecordSlice, SliceReader, Vec3I32,
};

const CLASS_FUNC_TRAIN: u8 = 0x11;

/// A world with nowhere to go: every push is refused where it started. This is
/// the shape a rider pinned between a train and a wall presents.
struct Pinned;

impl MovementTrace for Pinned {
    fn trace(
        &self,
        start: &Vec3I32,
        _end: &Vec3I32,
        _scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        *output = MovementTraceResult::unobstructed(*start);
        output.fraction = 0;
        true
    }
}

/// Open air: every push completes.
struct Open;

impl MovementTrace for Open {
    fn trace(
        &self,
        _start: &Vec3I32,
        end: &Vec3I32,
        _scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        *output = MovementTraceResult::unobstructed(*end);
        true
    }
}

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

fn volume(origin: Vec3I32, model: BrushModel) -> (Vec3I32, Vec3I32) {
    (
        Vec3I32 {
            x: origin.x + (i32::from(model.mins.x) << 12),
            y: origin.y + (i32::from(model.mins.y) << 12),
            z: origin.z + (i32::from(model.mins.z) << 12),
        },
        Vec3I32 {
            x: origin.x + (i32::from(model.maxs.x) << 12),
            y: origin.y + (i32::from(model.maxs.y) << 12),
            z: origin.z + (i32::from(model.maxs.z) << 12),
        },
    )
}

const fn subtract(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x - right.x,
        y: left.y - right.y,
        z: left.z - right.z,
    }
}

/// One gameplay frame's worth of pusher ticks, the same 1..4 the game layer
/// clamps to.
const FRAME_TICKS: usize = 4;

/// The first authored train on `name` that is moving under its own power,
/// wound forward to a tick where it is actually travelling.
fn moving_train(bytes: &[u8]) -> (QuakeTrain, BrushModel, RecordSlice<'_, MapEntity>) {
    let mut reader = SliceReader::new(bytes);
    let index = PsbIndex::read(&mut reader).expect("psb index");
    let entities = RecordSlice::<MapEntity>::new(lump(bytes, &index, LumpKind::Entities))
        .expect("entities");
    let models =
        RecordSlice::<BrushModel>::new(lump(bytes, &index, LumpKind::Models)).expect("models");
    for entity_index in 0..entities.len() {
        let entity = entities.get(entity_index).expect("entity");
        if entity.class_name != CLASS_FUNC_TRAIN || entity.model >= 0 || entity.target_name != 0 {
            continue;
        }
        let model = models
            .get((-entity.model) as usize)
            .expect("train brush model");
        let Some(mut train) = QuakeTrain::from_entity(entity, model, &entities) else {
            continue;
        };
        for _ in 0..600 {
            let before = train.origin();
            train.tick(&entities);
            if train.state() == TrainState::Moving && train.origin() != before {
                return (train, model, entities);
            }
        }
    }
    panic!("no self-starting authored train found");
}

/// A player box parked in the sliver the train is about to sweep through:
/// clear of the pusher before the step, inside it after. That is the only
/// placement that isolates the blocked step itself from a body that was
/// already stuck.
fn blocker_in_the_path(before: Vec3I32, after: Vec3I32, model: BrushModel) -> RiderBody {
    const HALF_WIDTH: i32 = 16 << 12;
    const CLEARANCE: i32 = 3 << 12;
    let (mins_before, maxs_before) = volume(before, model);
    let delta = subtract(after, before);
    let travel = [delta.x, delta.y, delta.z];
    let axis = (0..3)
        .max_by_key(|index| travel[*index].abs())
        .expect("three axes");
    let center = Vec3I32 {
        x: (mins_before.x + maxs_before.x) / 2,
        y: (mins_before.y + maxs_before.y) / 2,
        z: (mins_before.z + maxs_before.z) / 2,
    };
    let leading = if travel[axis] > 0 {
        [maxs_before.x, maxs_before.y, maxs_before.z][axis]
    } else {
        [mins_before.x, mins_before.y, mins_before.z][axis]
    };
    let sign = if travel[axis] > 0 { 1 } else { -1 };
    let mut origin = [center.x, center.y, center.z];
    origin[axis] = leading + sign * (CLEARANCE + HALF_WIDTH);
    let origin = Vec3I32 {
        x: origin[0],
        y: origin[1],
        z: origin[2],
    };
    RiderBody::new(
        origin,
        Vec3I32 {
            x: origin.x - HALF_WIDTH,
            y: origin.y - HALF_WIDTH,
            z: origin.z - HALF_WIDTH,
        },
        Vec3I32 {
            x: origin.x + HALF_WIDTH,
            y: origin.y + HALF_WIDTH,
            z: origin.z + HALF_WIDTH,
        },
    )
}

fn advance(train: &mut QuakeTrain, entities: &RecordSlice<'_, MapEntity>) -> Vec3I32 {
    let before = train.origin();
    for _ in 0..FRAME_TICKS {
        train.tick(entities);
    }
    subtract(train.origin(), before)
}

/// The whole failure path, on E1M5's own train: the pusher and the body it
/// could not move both go back, nothing is left intersecting, the crush still
/// lands and then respects its re-arm, and the train takes the same leg again
/// once the obstruction clears.
#[test]
fn a_blocked_train_rolls_itself_and_its_rider_back_and_retries_the_same_leg() {
    let bytes = map("e1m5");
    let (mut train, model, entities) = moving_train(&bytes);
    let mut scratch = TraceScratch::default();
    let mut crush = BlockCrush::new();

    // Look one frame ahead to place the obstruction, then rewind.
    let probe_start = train;
    let before_origin = train.origin();
    let heading = advance(&mut train, &entities);
    assert_ne!(
        heading,
        Vec3I32 { x: 0, y: 0, z: 0 },
        "the fixture must be a train that is actually moving"
    );
    let blocker = blocker_in_the_path(before_origin, train.origin(), model);
    train = probe_start;

    let (mins_before, maxs_before) = volume(train.origin(), model);
    assert!(
        !penetrates(blocker, mins_before, maxs_before),
        "the obstruction must start clear of the train"
    );

    // One blocked step, run exactly the way the game layer runs it.
    let restore = train;
    let moved = advance(&mut train, &entities);
    let (mins, maxs) = volume(train.origin(), model);
    assert!(
        penetrates(blocker, mins, maxs),
        "the step must actually drive the train into the obstruction"
    );
    let outcome = push_move(&Pinned, &mut scratch, blocker, false, moved, mins, maxs);
    assert!(outcome.blocked, "a body that cannot move blocks the train");
    assert!(!outcome.carried);
    assert_eq!(outcome.origin, blocker.origin, "the body rolls back");

    // 1. Both bodies roll back.
    train = restore;
    assert_eq!(train.origin(), before_origin, "the train rolls back");

    // 2. Nothing is left intersecting.
    let (mins, maxs) = volume(train.origin(), model);
    assert!(
        !penetrates(blocker, mins, maxs),
        "the rolled-back step must leave no body inside the pusher"
    );

    // 3. The crush still lands, and then respects its re-arm.
    crush.tick(FRAME_TICKS as u16);
    let damage = train.damage().max(0) as u16;
    assert_eq!(crush.crush(damage, TRAIN_BLOCK_COOLDOWN_TICKS), damage);
    crush.tick(FRAME_TICKS as u16);
    assert_eq!(
        crush.crush(damage, TRAIN_BLOCK_COOLDOWN_TICKS),
        0,
        "a blocked pusher may not damage every frame"
    );
    crush.tick(TRAIN_BLOCK_COOLDOWN_TICKS);
    assert_eq!(
        crush.crush(damage, TRAIN_BLOCK_COOLDOWN_TICKS),
        damage,
        "and may damage again once it has re-armed"
    );

    // 4. The retry is the same leg in the same direction, and once the way is
    //    clear the train both moves and carries what rests on it.
    let retry = advance(&mut train, &entities);
    assert_eq!(
        retry, heading,
        "train_blocked does not reverse: the retry is the same leg"
    );
    let (deck_mins, deck_maxs) = volume(train.origin(), model);
    let deck_origin = Vec3I32 {
        x: (deck_mins.x + deck_maxs.x) / 2,
        y: (deck_mins.y + deck_maxs.y) / 2,
        z: deck_maxs.z + (24 << 12),
    };
    let deck_rider = RiderBody::new(
        deck_origin,
        Vec3I32 {
            x: deck_origin.x - (16 << 12),
            y: deck_origin.y - (16 << 12),
            z: deck_origin.z - (24 << 12),
        },
        Vec3I32 {
            x: deck_origin.x + (16 << 12),
            y: deck_origin.y + (16 << 12),
            z: deck_origin.z + (32 << 12),
        },
    );
    let carried = advance(&mut train, &entities);
    let (mins, maxs) = volume(train.origin(), model);
    let outcome = push_move(&Open, &mut scratch, deck_rider, true, carried, mins, maxs);
    assert!(!outcome.blocked);
    assert!(outcome.carried, "the rider rides once the way is clear");
    assert_eq!(subtract(outcome.origin, deck_rider.origin), carried);
}

/// A blocked step that is rolled back must not advance the train's own
/// progress either, or a train pinned for a while would silently jump forward
/// when the obstruction cleared.
#[test]
fn a_rolled_back_train_makes_no_progress_at_all() {
    let bytes = map("e1m5");
    let (mut train, model, entities) = moving_train(&bytes);
    let mut scratch = TraceScratch::default();

    let parked = train;
    let before_origin = train.origin();
    advance(&mut train, &entities);
    let blocker = blocker_in_the_path(before_origin, train.origin(), model);
    train = parked;

    for _ in 0..30 {
        let restore = train;
        let moved = advance(&mut train, &entities);
        let (mins, maxs) = volume(train.origin(), model);
        let outcome = push_move(&Pinned, &mut scratch, blocker, false, moved, mins, maxs);
        if outcome.blocked {
            train = restore;
        }
    }
    assert_eq!(
        train.origin(),
        parked.origin(),
        "two seconds pinned must leave the train exactly where it was"
    );
    assert_eq!(train.corner_arrivals(), parked.corner_arrivals());
}

/// Which authored Episode 1 bodies a pusher can actually carry.
///
/// The runtime carries the player and live monsters. That is not a guess about
/// what might be needed: this walks all nine cooked maps, places every
/// `func_plat` at both of its endpoints and every `func_train` on each corner
/// of its own chain, and reports every authored monster left standing on a
/// deck. E1M4's `func_train` #26 is the case, with two ogres on it at its
/// first corner and a knight on it further along the chain. A player-only
/// carry would leave all three hanging over the slime the train crosses.
#[test]
fn every_authored_body_a_pusher_can_carry_is_accounted_for() {
    const MAPS: [&str; 9] = [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ];
    const CLASS_FUNC_PLAT: u8 = 0x10;
    const CLASS_PATH_CORNER: u8 = 0x45;
    const FIRST_MONSTER_CLASS: u8 = 0x36;
    const LAST_MONSTER_CLASS: u8 = 0x44;
    // Quake's biggest monster hull is 32 wide and 64 tall; a body whose origin
    // is inside this band over a deck is standing on it.
    const HORIZONTAL_SLACK: i32 = 24 << 12;
    const FEET_BELOW: i32 = 16 << 12;
    const HEAD_ABOVE: i32 = 80 << 12;

    let mut found: Vec<(String, usize, usize)> = Vec::new();
    for name in MAPS {
        let bytes = map(name);
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader).expect("psb index");
        let entities = RecordSlice::<MapEntity>::new(lump(&bytes, &index, LumpKind::Entities))
            .expect("entities");
        let models =
            RecordSlice::<BrushModel>::new(lump(&bytes, &index, LumpKind::Models)).expect("models");

        let corner = |target_name: u16| -> Option<MapEntity> {
            (0..entities.len())
                .filter_map(|index| entities.get(index))
                .find(|entity| {
                    entity.class_name == CLASS_PATH_CORNER && entity.target_name == target_name
                })
        };

        for pusher_index in 0..entities.len() {
            let pusher = entities.get(pusher_index).expect("entity");
            if pusher.model >= 0
                || !matches!(pusher.class_name, CLASS_FUNC_PLAT | CLASS_FUNC_TRAIN)
            {
                continue;
            }
            let model = models
                .get((-pusher.model) as usize)
                .expect("pusher brush model");
            let mut stops: Vec<Vec3I32> = vec![Vec3I32 { x: 0, y: 0, z: 0 }];
            if pusher.class_name == CLASS_FUNC_PLAT {
                let travel = if pusher.height != 0 {
                    i32::from(pusher.height) << 12
                } else {
                    (((i32::from(model.maxs.z) - i32::from(model.mins.z)) << 12) - (8 << 12)).max(1)
                };
                stops.push(Vec3I32 {
                    x: 0,
                    y: 0,
                    z: -travel,
                });
            } else {
                // Only this train's own chain, never every corner on the map.
                let mut seen = Vec::new();
                let mut next = pusher.target;
                while next != 0 && !seen.contains(&next) {
                    seen.push(next);
                    let Some(stop) = corner(next) else { break };
                    stops.push(Vec3I32 {
                        x: stop.origin.x - (i32::from(model.mins.x) << 12),
                        y: stop.origin.y - (i32::from(model.mins.y) << 12),
                        z: stop.origin.z - (i32::from(model.mins.z) << 12),
                    });
                    next = stop.target;
                }
            }

            for stop in stops {
                let (lo, hi) = volume(stop, model);
                for body_index in 0..entities.len() {
                    let body = entities.get(body_index).expect("entity");
                    if !(FIRST_MONSTER_CLASS..=LAST_MONSTER_CLASS).contains(&body.class_name) {
                        continue;
                    }
                    let inside = body.origin.x >= lo.x - HORIZONTAL_SLACK
                        && body.origin.x <= hi.x + HORIZONTAL_SLACK
                        && body.origin.y >= lo.y - HORIZONTAL_SLACK
                        && body.origin.y <= hi.y + HORIZONTAL_SLACK
                        && body.origin.z >= hi.z - FEET_BELOW
                        && body.origin.z <= hi.z + HEAD_ABOVE;
                    if inside && !found.iter().any(|entry| entry.2 == body_index) {
                        found.push((name.to_string(), pusher_index, body_index));
                    }
                }
            }
        }
    }

    let summary: Vec<String> = found
        .iter()
        .map(|(map, pusher, body)| format!("{map}:pusher#{pusher}:monster#{body}"))
        .collect();
    assert_eq!(
        summary,
        vec![
            "e1m4:pusher#26:monster#243".to_string(),
            "e1m4:pusher#26:monster#244".to_string(),
            "e1m4:pusher#26:monster#314".to_string(),
        ],
        "the set of authored monsters a pusher can carry changed; the runtime \
         carries live bodies, so this is a data change to look at rather than a \
         missing feature"
    );
}
