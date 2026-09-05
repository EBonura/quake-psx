//! Quake's `SV_PushMove` rider policy over an abstract collision provider.
//!
//! A Quake pusher is not an obstacle that happens to move. `SV_PushMove` moves
//! the pusher, then moves every body standing on it or caught inside its final
//! position, and only reports a block for a body it could not move clear. That
//! is what makes a `func_plat` a lift instead of a rising wall, and it is the
//! whole reason E1M7's upper ring is reachable at all.
//!
//! This module owns the policy: who is carried, where the carry ends, and
//! whether the pusher was blocked. The caller owns the collision provider and
//! the pusher's own motion, exactly like [`crate::movement`] owns locomotion
//! policy and leaves the hull to the caller.

use quake_formats::Vec3I32;

use crate::collision::TraceScratch;
use crate::movement::{MovementTrace, MovementTraceResult};

/// One body a pusher may carry, as an absolute Q20.12 world box.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RiderBody {
    pub origin: Vec3I32,
    pub mins: Vec3I32,
    pub maxs: Vec3I32,
}

impl RiderBody {
    pub const fn new(origin: Vec3I32, mins: Vec3I32, maxs: Vec3I32) -> Self {
        Self { origin, mins, maxs }
    }

    fn translated(self, delta: Vec3I32) -> Self {
        Self {
            origin: add(self.origin, delta),
            mins: add(self.mins, delta),
            maxs: add(self.maxs, delta),
        }
    }
}

/// What one pusher's push pass did to one body.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PushOutcome {
    /// Where the body ended up. Equal to the input origin when nothing carried
    /// it, and also when the push was refused: the original puts a blocked
    /// rider back where it started.
    pub origin: Vec3I32,
    /// The pusher moved this body.
    pub carried: bool,
    /// The body could not be moved clear of the pusher. This is the original's
    /// `block`: the caller must put the pusher back where it was and run its
    /// `blocked` function.
    pub blocked: bool,
    /// Dynamic body that caused the refusal, when the collision provider can
    /// identify one. A player pushed into a monster is blocked by that monster
    /// rather than by an anonymous world surface.
    pub blocking_body: Option<u16>,
}

/// `SV_TestEntityPosition` narrowed to one pusher: is the body driven into the
/// mover's volume, rather than merely resting against it?
///
/// The original asks the hull and takes `startsolid`, which is true for a box
/// merely touching the epsilon shell. A rider standing on a lift touches it
/// every single tick, so the port asks for real penetration instead. One unit
/// is the smallest threshold that clears both the tracer's plane epsilon and
/// the whole-unit truncation of a mover's clip bounds; the slowest lift Quake
/// authors still covers more than that in one tick, so no crush is missed.
pub const CONTACT_MARGIN_Q12: i32 = 1 << 12;

/// True when `body` has penetrated the absolute Q20.12 volume by more than the
/// resting-contact margin.
pub fn penetrates(body: RiderBody, mover_mins: Vec3I32, mover_maxs: Vec3I32) -> bool {
    let mins = add_scalar(mover_mins, CONTACT_MARGIN_Q12);
    let maxs = add_scalar(mover_maxs, -CONTACT_MARGIN_Q12);
    body.maxs.x > mins.x
        && body.mins.x < maxs.x
        && body.maxs.y > mins.y
        && body.mins.y < maxs.y
        && body.maxs.z > mins.z
        && body.mins.z < maxs.z
}

/// Is this body standing on the pusher's deck?
///
/// Quake answers with `FL_ONGROUND && groundentity == pusher`, which the player
/// motor's own downward probe reproduces exactly. Bodies whose physics do not
/// keep a ground entity get this instead: horizontally over the deck, with the
/// feet inside the shell the tracer parks a resting box in.
pub fn rests_on(body: RiderBody, mover_mins: Vec3I32, mover_maxs: Vec3I32) -> bool {
    body.maxs.x > mover_mins.x
        && body.mins.x < mover_maxs.x
        && body.maxs.y > mover_mins.y
        && body.mins.y < mover_maxs.y
        && body.mins.z >= mover_maxs.z.saturating_sub(CONTACT_MARGIN_Q12)
        && body.mins.z <= mover_maxs.z.saturating_add(2 * CONTACT_MARGIN_Q12)
}

/// Run one pusher's `SV_PushMove` pass over one body.
///
/// `collision` must be the world with this pusher taken out of it: the
/// original sets `pusher->v.solid = SOLID_NOT` around the push for exactly
/// this reason, so the thing carrying a body never also stops it.
///
/// `delta` is the motion the pusher already performed, and `mover_mins` and
/// `mover_maxs` are its absolute volume *after* that motion.
///
/// `standing_on` is Quake's `FL_ONGROUND && groundentity == pusher`. A body
/// with that flag is carried whether or not it overlaps anything, which is the
/// only reason a player riding the top of a lift moves at all.
pub fn push_move<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut TraceScratch,
    body: RiderBody,
    standing_on: bool,
    delta: Vec3I32,
    mover_mins: Vec3I32,
    mover_maxs: Vec3I32,
) -> PushOutcome {
    if delta == (Vec3I32 { x: 0, y: 0, z: 0 }) {
        return PushOutcome {
            origin: body.origin,
            carried: false,
            blocked: false,
            blocking_body: None,
        };
    }
    let mut moved = body;
    let mut carried = false;
    let mut blocking_body = None;
    if standing_on || penetrates(body, mover_mins, mover_maxs) {
        let destination = add(body.origin, delta);
        let mut trace = MovementTraceResult::unobstructed(destination);
        // A provider that cannot answer leaves the body where it was, which
        // can only ever under-move a rider. The original has no such path.
        if collision.trace(&body.origin, &destination, scratch, &mut trace) {
            // `SV_ClipMoveToEntity` leaves `endpos` at the destination for a
            // trace that never left solid, and `SV_PushEntity` takes that
            // answer as-is: a rider is moved and then tested, never refused up
            // front.
            moved = body.translated(subtract(trace.end, body.origin));
            carried = moved.origin != body.origin;
            blocking_body = trace.blocking_body;
        }
    }
    if penetrates(moved, mover_mins, mover_maxs) {
        // `SV_PushMove`'s failure path: the rider goes back to where it was and
        // so does the pusher, so a blocked pusher never leaves anything buried
        // in its own brush.
        return PushOutcome {
            origin: body.origin,
            carried: false,
            blocked: true,
            blocking_body,
        };
    }
    PushOutcome {
        origin: moved.origin,
        carried,
        blocked: false,
        blocking_body: None,
    }
}

/// The `blocked` half of a pusher: how often it may hurt what it is grinding
/// against.
///
/// `door_blocked` and `train_blocked` both gate their damage behind an
/// `attack_finished` style re-arm rather than damaging every tick, and the
/// rollback of a blocked step does not cancel that damage: the original runs
/// the `blocked` function on the way out of the failure path.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockCrush {
    cooldown_ticks: u16,
}

impl BlockCrush {
    pub const fn new() -> Self {
        Self { cooldown_ticks: 0 }
    }

    pub const fn cooling_down(self) -> bool {
        self.cooldown_ticks != 0
    }

    /// Advance the re-arm timer by one gameplay frame's worth of ticks.
    pub fn tick(&mut self, ticks: u16) {
        self.cooldown_ticks = self.cooldown_ticks.saturating_sub(ticks);
    }

    /// Damage for one blocked step, or zero while the pusher is still cooling
    /// down from the last one.
    pub fn crush(&mut self, damage: u16, rearm_ticks: u16) -> u16 {
        if self.cooldown_ticks != 0 {
            return 0;
        }
        self.cooldown_ticks = rearm_ticks;
        damage
    }
}

const fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}

const fn subtract(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_sub(right.x),
        y: left.y.saturating_sub(right.y),
        z: left.z.saturating_sub(right.z),
    }
}

const fn add_scalar(value: Vec3I32, amount: i32) -> Vec3I32 {
    Vec3I32 {
        x: value.x.saturating_add(amount),
        y: value.y.saturating_add(amount),
        z: value.z.saturating_add(amount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::Q12_ONE;
    use crate::mover::{QuakeMover, QuakeMoverState};
    use quake_formats::{BrushModel, MapEntity, Vec3I16};

    const PLAYER_MINS: Vec3I32 = Vec3I32 {
        x: -16 << 12,
        y: -16 << 12,
        z: -24 << 12,
    };
    const PLAYER_MAXS: Vec3I32 = Vec3I32 {
        x: 16 << 12,
        y: 16 << 12,
        z: 32 << 12,
    };

    fn body(x: i32, y: i32, z: i32) -> RiderBody {
        let origin = Vec3I32 {
            x: x << 12,
            y: y << 12,
            z: z << 12,
        };
        RiderBody::new(origin, add(origin, PLAYER_MINS), add(origin, PLAYER_MAXS))
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

    /// A room with a hard ceiling: an upward push stops with the player's head
    /// against it, which is the case `plat_crush` exists for.
    struct Ceiling(i32);

    impl MovementTrace for Ceiling {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            let limit = (self.0 << 12) - PLAYER_MAXS.z;
            if end.z > limit {
                let span = end.z - start.z;
                output.fraction = if span > 0 {
                    (((limit - start.z).max(0) as i64 * Q12_ONE as i64) / span as i64) as i32
                } else {
                    0
                };
                output.end = Vec3I32 {
                    x: start.x,
                    y: start.y,
                    z: start.z.max(limit).min(limit),
                };
                output.normal = Vec3I16 {
                    x: 0,
                    y: 0,
                    z: -(Q12_ONE as i16),
                };
            }
            true
        }
    }

    /// E1M7's own lift, cooked fields and all: a 126 x 142 x 46 brush with an
    /// authored `height` of 176 and no `targetname`, so `func_plat` spawns it
    /// 176 units low and it rises the whole way when a player steps on it.
    /// That travel is what reaches the `event_lightning` ring.
    fn e1m7_plat() -> (QuakeMover, BrushModel) {
        let model = BrushModel {
            mins: Vec3I16 {
                x: 1153,
                y: -7,
                z: 97,
            },
            maxs: Vec3I16 {
                x: 1279,
                y: 135,
                z: 143,
            },
            ..BrushModel::default()
        };
        let source = MapEntity {
            class_name: 0x10,
            model: -1,
            height: 176,
            ..MapEntity::default()
        };
        (
            QuakeMover::from_entity(source, model).unwrap().unwrap(),
            model,
        )
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

    /// Ride E1M7's authored lift from its low position to its top, the exact
    /// motion that reaches the `event_lightning` ring. The rider only ever gets
    /// its own `standing_on` flag; every unit of the rise comes from the push.
    #[test]
    fn a_player_standing_on_a_rising_plat_rises_with_it() {
        let (mut plat, model) = e1m7_plat();
        let mut scratch = TraceScratch::default();
        // Feet on the lift's low deck: the brush top is 143 and the lift
        // spawns 176 units below that, so the deck is at -33 and the player
        // origin sits its own 24-unit crouch above it.
        let mut rider = body(1216, 64, -33 + 24);
        let start_z = rider.origin.z;
        plat.activate();
        for _ in 0..240 {
            let before = plat.transform().origin;
            plat.tick();
            let after = plat.transform().origin;
            let (mins, maxs) = volume(after, model);
            let outcome = push_move(
                &Open,
                &mut scratch,
                rider,
                true,
                subtract(after, before),
                mins,
                maxs,
            );
            assert!(!outcome.blocked, "open air never blocks a lift");
            rider = rider.translated(subtract(outcome.origin, rider.origin));
            if plat.state() == QuakeMoverState::Top {
                break;
            }
        }
        assert_eq!(plat.state(), QuakeMoverState::Top);
        assert_eq!(
            (rider.origin.z - start_z) >> 12,
            176,
            "the rider must arrive exactly one lift travel higher"
        );
        // And the rider is still standing on the deck, not sunk into it.
        let (mins, maxs) = volume(plat.transform().origin, model);
        assert!(!penetrates(rider, mins, maxs));
        assert_eq!(rider.mins.z, maxs.z);
    }

    /// The same lift with the player stepped off it. Nothing about a moving
    /// brush may move a body that is neither standing on it nor inside it.
    #[test]
    fn a_player_that_stepped_off_the_plat_is_left_behind() {
        let (mut plat, model) = e1m7_plat();
        let mut scratch = TraceScratch::default();
        // One player width clear of the lift's west face.
        let rider = body(1153 - 40, 64, -33 + 24);
        plat.activate();
        for _ in 0..240 {
            let before = plat.transform().origin;
            plat.tick();
            let after = plat.transform().origin;
            let (mins, maxs) = volume(after, model);
            let outcome = push_move(
                &Open,
                &mut scratch,
                rider,
                false,
                subtract(after, before),
                mins,
                maxs,
            );
            assert!(!outcome.carried, "a body beside the lift is not a rider");
            assert!(!outcome.blocked);
            assert_eq!(outcome.origin, rider.origin);
        }
    }

    /// `plat_crush`: a rider the lift cannot push clear blocks it, and the lift
    /// answers by turning around instead of grinding the rider into the roof.
    #[test]
    fn a_rider_crushed_against_a_ceiling_blocks_the_lift_and_turns_it_around() {
        let (mut plat, model) = e1m7_plat();
        let mut scratch = TraceScratch::default();
        // A roof twenty units above the head of a player standing on the low
        // deck, so the lift runs out of room a fraction of the way up.
        let ceiling = Ceiling(-33 + 24 + 32 + 20);
        let mut rider = body(1216, 64, -33 + 24);
        plat.activate();
        let mut blocked_ticks = 0usize;
        let mut reversed = false;
        let mut rose = 0i32;
        for _ in 0..240 {
            // The whole pusher is snapshot the way the game layer snapshots
            // it, because a blocked pusher has to be put back.
            let restore = plat;
            let before = plat.transform().origin;
            plat.tick();
            let after = plat.transform().origin;
            let (mins, maxs) = volume(after, model);
            let outcome = push_move(
                &ceiling,
                &mut scratch,
                rider,
                true,
                subtract(after, before),
                mins,
                maxs,
            );
            rider = rider.translated(subtract(outcome.origin, rider.origin));
            if outcome.blocked {
                plat = restore;
                blocked_ticks += 1;
                reversed = plat.crush_reverse();
                break;
            }
            rose += (after.z - before.z) >> 12;
        }
        assert!(blocked_ticks > 0, "the roof must block the lift");
        assert!(reversed, "plat_crush reverses a blocked lift");
        assert_eq!(plat.state(), QuakeMoverState::Down);
        assert!(
            (1..176).contains(&rose),
            "the lift stopped short of its travel, rose {rose}"
        );
        // The blocked tick was given back, so the rider is still standing on
        // the deck rather than buried in it.
        let (mins, maxs) = volume(plat.transform().origin, model);
        assert!(!penetrates(rider, mins, maxs));
        assert_eq!(rider.mins.z, maxs.z);

        // And once the lift is going the other way the rider rides it back
        // down: the crush is a single event, not a grind.
        for _ in 0..16 {
            let before = plat.transform().origin;
            plat.tick();
            let after = plat.transform().origin;
            let (mins, maxs) = volume(after, model);
            let outcome = push_move(
                &ceiling,
                &mut scratch,
                rider,
                true,
                subtract(after, before),
                mins,
                maxs,
            );
            rider = rider.translated(subtract(outcome.origin, rider.origin));
            assert!(
                !outcome.blocked,
                "a retreating lift cannot still be crushing"
            );
        }
        assert!(rider.origin.z < (((-33 + 24) << 12) + (rose << 12)));
    }

    /// A door is a pusher too, and the carry is not a lift-only feature.
    ///
    /// E1M1 authors a START_OPEN `func_door` that drops to reveal a climb, and
    /// the reverse move lifts whatever stands on it. The policy here is
    /// class-agnostic on purpose: the game layer runs every `SceneMover`
    /// through the same pass, so a rider standing on a door is carried by it
    /// exactly like a rider standing on a lift.
    #[test]
    fn a_rising_door_carries_its_rider_too() {
        // `angles` -1 is Quake's "up" movedir, so this door travels vertically.
        let source = MapEntity {
            class_name: 0x0c,
            model: -1,
            speed: 100,
            angles: Vec3I16 { x: 0, y: -1, z: 0 },
            ..MapEntity::default()
        };
        let model = BrushModel {
            mins: Vec3I16 {
                x: -32,
                y: -32,
                z: 0,
            },
            maxs: Vec3I16 {
                x: 32,
                y: 32,
                z: 64,
            },
            ..BrushModel::default()
        };
        let mut door = QuakeMover::from_entity(source, model).unwrap().unwrap();
        let mut scratch = TraceScratch::default();
        let mut rider = body(0, 0, 64 + 24);
        let start_z = rider.origin.z;
        door.activate();
        for _ in 0..240 {
            let before = door.transform().origin;
            door.tick();
            let after = door.transform().origin;
            let (mins, maxs) = volume(after, model);
            let outcome = push_move(
                &Open,
                &mut scratch,
                rider,
                true,
                subtract(after, before),
                mins,
                maxs,
            );
            assert!(!outcome.blocked);
            rider = rider.translated(subtract(outcome.origin, rider.origin));
            if door.state() == QuakeMoverState::Top {
                break;
            }
        }
        assert_eq!(door.state(), QuakeMoverState::Top);
        // `func_door`'s travel is the size along movedir less the eight unit
        // lip, so a 64 unit door lifts its rider 56.
        assert_eq!((rider.origin.z - start_z) >> 12, 56);
        let (_, maxs) = volume(door.transform().origin, model);
        assert_eq!(rider.mins.z, maxs.z);
    }

    /// A `func_door` is a pusher too, and `door_blocked` turns it around the
    /// same way `plat_crush` does. A door held open (`wait < 0`) keeps the
    /// damage-only branch, because the original guards on `wait >= 0`.
    #[test]
    fn a_blocked_door_reverses_unless_it_is_held_open() {
        let source = MapEntity {
            class_name: 0x0c,
            model: -1,
            speed: 100,
            ..MapEntity::default()
        };
        let model = BrushModel {
            maxs: Vec3I16 { x: 64, y: 8, z: 8 },
            ..BrushModel::default()
        };
        let mut door = QuakeMover::from_entity(source, model).unwrap().unwrap();
        door.activate();
        door.tick();
        assert!(door.crush_reverse());
        assert_eq!(door.state(), QuakeMoverState::Down);

        let mut held = QuakeMover::from_entity(MapEntity { wait: -1, ..source }, model)
            .unwrap()
            .unwrap();
        held.activate();
        held.tick();
        assert!(!held.crush_reverse());
        assert_eq!(held.state(), QuakeMoverState::Up);
    }

    /// A body resting exactly on a mover's surface is contact, not penetration.
    /// Getting this wrong is the difference between riding a lift and being
    /// crushed by the lift you are standing on.
    #[test]
    fn resting_on_a_mover_is_not_penetrating_it() {
        let mins = Vec3I32 {
            x: -64 << 12,
            y: -64 << 12,
            z: -16 << 12,
        };
        let maxs = Vec3I32 {
            x: 64 << 12,
            y: 64 << 12,
            z: 0,
        };
        // Feet exactly on the deck.
        let resting = body(0, 0, 24);
        assert!(!penetrates(resting, mins, maxs));
        assert!(rests_on(resting, mins, maxs));
        // And one plane epsilon above it, where the tracer actually parks a
        // resting box.
        let mut shell = body(0, 0, 24);
        shell = shell.translated(Vec3I32 { x: 0, y: 0, z: 128 });
        assert!(!penetrates(shell, mins, maxs));
        assert!(rests_on(shell, mins, maxs));
        // Two units down is a body the mover has driven into.
        assert!(penetrates(body(0, 0, 22), mins, maxs));
    }
}
