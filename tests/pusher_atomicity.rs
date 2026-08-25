//! The game layer's `SV_PushMove` step is all-or-nothing, across every body.
//!
//! `crates/quake-core/tests/pusher_block.rs` pins the core-level properties of
//! one pusher against one body. This file is the layer above it, where the
//! bodies actually differ: the player lives in a borrowed [`pusher::Rider`] and
//! the monsters live in the entity list that the composed collision hull is
//! also borrowing. That split is what made the step non-atomic. The player was
//! carried the instant its own push came back clear, and only the monster moves
//! were held until the end, so a monster blocking afterwards rolled the pusher
//! and the monsters back and left the player one whole pusher step ahead of the
//! pusher that never moved.
//!
//! The module under test is the guest's own source, compiled here for the host.
//! Nothing is re-implemented.
//!
//! The pusher is E1M5's authored `func_train`, which starts moving at load with
//! no `targetname`, so the motion driving these cases is the shipping map's.

#[path = "../game/src/pusher.rs"]
mod pusher;

use pusher::{push_pass, PushBlocker, PushLedger, Rider, MAX_CARRIED_BODIES};
use quake_core::collision::TraceScratch;
use quake_core::movement::{MovementTrace, MovementTraceResult};
use quake_core::push::{penetrates, rests_on, BlockCrush, PushOutcome, RiderBody};
use quake_core::train::{QuakeTrain, TrainState, TRAIN_BLOCK_COOLDOWN_TICKS};
use quake_formats::{BrushModel, LumpKind, MapEntity, PsbIndex, RecordSlice, SliceReader, Vec3I32};

const CLASS_FUNC_TRAIN: u8 = 0x11;
/// One gameplay frame's worth of pusher ticks, the same 1..4 the game layer
/// clamps to.
const FRAME_TICKS: usize = 4;
const PLAYER_HALF_WIDTH: i32 = 16 << 12;
const PLAYER_BELOW: i32 = 24 << 12;
const PLAYER_ABOVE: i32 = 32 << 12;

/// Open air for every body except the one parked at `pinned`, which cannot
/// move at all. That is the shape a monster wedged between a train and a wall
/// presents, and it leaves the player's own carry completely unobstructed.
struct PinnedBody {
    pinned: Vec3I32,
}

impl MovementTrace for PinnedBody {
    fn trace(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        _scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        if *start == self.pinned {
            *output = MovementTraceResult::unobstructed(*start);
            output.fraction = 0;
        } else {
            *output = MovementTraceResult::unobstructed(*end);
        }
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
    let path = format!("{}/id1psx/maps/{name}.psb", env!("CARGO_MANIFEST_DIR"));
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

const fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x + right.x,
        y: left.y + right.y,
        z: left.z + right.z,
    }
}

const fn subtract(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x - right.x,
        y: left.y - right.y,
        z: left.z - right.z,
    }
}

/// The first authored train on `name` that is moving under its own power,
/// wound forward to a tick where it is actually travelling.
fn moving_train(bytes: &[u8]) -> (QuakeTrain, BrushModel, RecordSlice<'_, MapEntity>) {
    let mut reader = SliceReader::new(bytes);
    let index = PsbIndex::read(&mut reader).expect("psb index");
    let entities =
        RecordSlice::<MapEntity>::new(lump(bytes, &index, LumpKind::Entities)).expect("entities");
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

fn advance(train: &mut QuakeTrain, entities: &RecordSlice<'_, MapEntity>) -> Vec3I32 {
    let before = train.origin();
    for _ in 0..FRAME_TICKS {
        train.tick(entities);
    }
    subtract(train.origin(), before)
}

/// A monster-sized box parked in the sliver the train is about to sweep
/// through: clear of the pusher before the step, inside it after. That is the
/// only placement that isolates the blocked step itself from a body that was
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

/// A player standing on the middle of the pusher's deck, feet exactly on it.
fn player_on_deck(train_origin: Vec3I32, model: BrushModel) -> Rider {
    let (mins, maxs) = volume(train_origin, model);
    let origin = Vec3I32 {
        x: (mins.x + maxs.x) / 2,
        y: (mins.y + maxs.y) / 2,
        z: maxs.z + PLAYER_BELOW,
    };
    Rider::new(
        origin,
        Vec3I32 {
            x: origin.x - PLAYER_HALF_WIDTH,
            y: origin.y - PLAYER_HALF_WIDTH,
            z: origin.z - PLAYER_BELOW,
        },
        Vec3I32 {
            x: origin.x + PLAYER_HALF_WIDTH,
            y: origin.y + PLAYER_HALF_WIDTH,
            z: origin.z + PLAYER_ABOVE,
        },
        true,
    )
}

/// The whole failure path with two bodies on one pusher: the player's carry
/// succeeds, a monster then blocks, and nothing at all keeps its move.
///
/// Every property the original guarantees is checked here, in the order the
/// game layer produces them:
///
/// 1. The player really is carried when nothing blocks, so the withheld move in
///    the blocked case is a move that would otherwise have happened.
/// 2. A monster that cannot move clear blocks the step.
/// 3. The pusher, the player and the monster all end at their original
///    positions, and nothing is left inside the pusher's brush.
/// 4. `train_blocked` still damages, and still respects its re-arm.
/// 5. The retry is the same leg in the same direction.
#[test]
fn a_blocked_pusher_withholds_the_player_carry_along_with_every_monster() {
    let bytes = map("e1m5");
    let (mut train, model, entities) = moving_train(&bytes);
    let mut scratch = TraceScratch::default();
    let mut crush = BlockCrush::new();

    // Look one frame ahead to place the obstruction, then rewind.
    let parked = train;
    let before_origin = train.origin();
    let heading = advance(&mut train, &entities);
    assert_ne!(
        heading,
        Vec3I32 { x: 0, y: 0, z: 0 },
        "the fixture must be a train that is actually moving"
    );
    let monster = blocker_in_the_path(before_origin, train.origin(), model);
    train = parked;

    let rider_start = player_on_deck(before_origin, model);
    let (mins_before, maxs_before) = volume(train.origin(), model);
    assert!(
        !penetrates(monster, mins_before, maxs_before),
        "the obstruction must start clear of the train"
    );
    assert!(
        rests_on(rider_start.body(), mins_before, maxs_before),
        "the player must start standing on the deck"
    );

    // 1. The control: the same step with the deck to itself carries the player
    //    exactly one pusher step. Without this the withheld move below could be
    //    a move that was never going to happen.
    {
        let mut control = train;
        let moved = advance(&mut control, &entities);
        let (mins, maxs) = volume(control.origin(), model);
        let ledger = push_pass(
            &Open,
            &mut scratch,
            &rider_start,
            true,
            core::iter::empty(),
            moved,
            mins,
            maxs,
        );
        assert!(!ledger.blocked(), "open air never blocks");
        assert_eq!(
            ledger.player_move(),
            Some(add(rider_start.origin, moved)),
            "the player rides the deck when nothing blocks"
        );
    }

    // 2. The real step, run exactly the way the game layer runs it: snapshot,
    //    advance, resolve every body, then apply or roll back.
    let mut rider = rider_start;
    let restore = train;
    let moved = advance(&mut train, &entities);
    let (mins, maxs) = volume(train.origin(), model);
    assert!(
        penetrates(monster, mins, maxs),
        "the step must actually drive the train into the obstruction"
    );
    let collision = PinnedBody {
        pinned: monster.origin,
    };
    let ledger = push_pass(
        &collision,
        &mut scratch,
        &rider,
        true,
        core::iter::once((7u16, monster)),
        moved,
        mins,
        maxs,
    );
    assert!(ledger.blocked(), "a body that cannot move blocks the train");
    assert_eq!(
        ledger.blocker(),
        Some(PushBlocker::Body(7)),
        "the blocked function must receive the monster participant, not the player"
    );

    // This is the defect. The player's own push came back clear and carried,
    // and under the old arrangement it had already been applied by the time the
    // monster answered. A committed ledger hands out nothing at all.
    assert_eq!(
        ledger.player_move(),
        None,
        "a blocked step may not carry the player"
    );
    assert!(
        ledger.body_moves().is_empty(),
        "a blocked step may not carry a monster either"
    );

    // Apply the ledger, which is the game layer's whole write phase.
    if let Some(origin) = ledger.player_move() {
        rider.translate(subtract(origin, rider.origin));
    }
    let monster_after = ledger
        .body_moves()
        .iter()
        .copied()
        .find(|(index, _)| *index == 7)
        .map_or(monster.origin, |(_, origin)| origin);
    if ledger.blocked() {
        train = restore;
    }

    // 3. Everything is where it started, and nothing is inside the pusher.
    assert_eq!(train.origin(), before_origin, "the train rolls back");
    assert_eq!(
        rider.origin, rider_start.origin,
        "the player rolls back with it"
    );
    assert!(
        !rider.carried,
        "a rolled-back player was never carried, so the motor must not be told it was"
    );
    assert_ne!(
        rider.origin,
        add(rider_start.origin, moved),
        "the old pass left the player one whole pusher step ahead of a pusher that never moved"
    );
    assert_eq!(monster_after, monster.origin, "the monster rolls back too");
    let (mins, maxs) = volume(train.origin(), model);
    assert!(
        !penetrates(monster, mins, maxs),
        "the rolled-back step must leave no body inside the pusher"
    );
    assert!(!penetrates(rider.body(), mins, maxs));
    assert!(
        rests_on(rider.body(), mins, maxs),
        "the player is still standing on the deck, not sunk into it"
    );

    // 4. The crush still lands, and then respects its re-arm.
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

    // 5. `train_blocked` does not reverse: the retry is the same leg.
    let retry = advance(&mut train, &entities);
    assert_eq!(
        retry, heading,
        "the retry is the same leg in the same direction"
    );
}

/// A carry the pool cannot hold blocks the step instead of quietly dropping a
/// body.
///
/// The authored census bounds where bodies start, not how many can walk onto
/// one deck while it is moving. Dropping the overflow reads as the deck sliding
/// out from under a monster that was standing on it, with nothing anywhere
/// reporting that it happened; blocking is the answer the original already
/// gives to any body a pusher cannot take with it.
#[test]
fn a_carry_pool_that_overflows_blocks_the_step_instead_of_dropping_a_body() {
    let bytes = map("e1m5");
    let (mut train, model, entities) = moving_train(&bytes);
    let mut scratch = TraceScratch::default();

    let deck_origin = train.origin();
    let (deck_mins, deck_maxs) = volume(deck_origin, model);
    let rider = player_on_deck(deck_origin, model);

    // Bodies crowded onto one deck. They overlap each other, which the pusher
    // pass does not care about: it asks each body about the pusher, one at a
    // time.
    let crowd = |count: usize| -> Vec<(u16, RiderBody)> {
        (0..count)
            .map(|index| {
                let origin = Vec3I32 {
                    x: (deck_mins.x + deck_maxs.x) / 2 + ((index as i32) << 12),
                    y: (deck_mins.y + deck_maxs.y) / 2,
                    z: deck_maxs.z + (16 << 12),
                };
                (
                    index as u16,
                    RiderBody::new(
                        origin,
                        Vec3I32 {
                            x: origin.x - (16 << 12),
                            y: origin.y - (16 << 12),
                            z: origin.z - (16 << 12),
                        },
                        Vec3I32 {
                            x: origin.x + (16 << 12),
                            y: origin.y + (16 << 12),
                            z: origin.z + (16 << 12),
                        },
                    ),
                )
            })
            .collect()
    };

    let moved = advance(&mut train, &entities);
    let (mins, maxs) = volume(train.origin(), model);

    let full = crowd(MAX_CARRIED_BODIES);
    for (_, body) in &full {
        assert!(
            rests_on(*body, mins, maxs),
            "every fixture body must actually be riding the deck"
        );
    }
    let ledger = push_pass(
        &Open,
        &mut scratch,
        &rider,
        true,
        full.iter().copied(),
        moved,
        mins,
        maxs,
    );
    assert!(!ledger.blocked(), "a full but not overfull pool is fine");
    assert_eq!(ledger.body_moves().len(), MAX_CARRIED_BODIES);
    assert!(ledger.player_move().is_some());

    let overfull = crowd(MAX_CARRIED_BODIES + 1);
    let ledger = push_pass(
        &Open,
        &mut scratch,
        &rider,
        true,
        overfull.iter().copied(),
        moved,
        mins,
        maxs,
    );
    assert!(
        ledger.blocked(),
        "one body too many must block the step, not vanish from it"
    );
    assert_eq!(
        ledger.blocker(),
        Some(PushBlocker::Capacity),
        "capacity failure blocks without fabricating a damage victim"
    );
    assert_ne!(ledger.blocker(), Some(PushBlocker::Player));
    assert!(ledger.body_moves().is_empty());
    assert_eq!(ledger.player_move(), None);
}

/// A blocked ledger withholds every staged move.
#[test]
fn a_ledger_withholds_every_staged_move_once_anything_blocks() {
    let carried = |x: i32| PushOutcome {
        origin: Vec3I32 { x, y: 0, z: 0 },
        carried: true,
        blocked: false,
        blocking_body: None,
    };
    let blocked = PushOutcome {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        carried: false,
        blocked: true,
        blocking_body: None,
    };

    let mut ledger = PushLedger::new();
    ledger.stage_player(carried(10));
    ledger.stage_body(1, carried(20));
    assert!(!ledger.blocked());
    assert_eq!(ledger.player_move(), Some(Vec3I32 { x: 10, y: 0, z: 0 }));
    assert_eq!(ledger.body_moves().len(), 1);

    // The block arrives last, after both moves were already decided.
    ledger.stage_body(2, blocked);
    assert!(ledger.blocked());
    assert_eq!(ledger.blocker(), Some(PushBlocker::Body(2)));
    assert_eq!(ledger.player_move(), None);
    assert!(ledger.body_moves().is_empty());

    // And a block on the player alone withholds the monsters just the same.
    let mut ledger = PushLedger::new();
    ledger.stage_player(blocked);
    ledger.stage_body(1, carried(20));
    assert!(ledger.blocked());
    assert_eq!(ledger.blocker(), Some(PushBlocker::Player));
    assert_eq!(ledger.player_move(), None);
    assert!(ledger.body_moves().is_empty());
}

/// A dynamic body can stop the player's own carry trace before that body gets
/// its turn as a participant. The collision identity must survive the shared
/// movement and push layers so the pusher damages the body, not the player.
#[test]
fn a_player_carry_blocked_by_a_monster_names_the_monster() {
    let mut ledger = PushLedger::new();
    ledger.stage_player(PushOutcome {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        carried: false,
        blocked: true,
        blocking_body: Some(101),
    });
    assert_eq!(ledger.blocker(), Some(PushBlocker::Body(101)));
    assert_ne!(ledger.blocker(), Some(PushBlocker::Player));
    assert!(ledger.blocked());
    assert_eq!(ledger.player_move(), None);
    assert!(ledger.body_moves().is_empty());
}
