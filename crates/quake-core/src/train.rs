//! Original `func_train` riding an authored `path_corner` chain.
//!
//! The original places the train so its own `mins` corner sits on the current
//! `path_corner`, moves at `speed` units per second to the next corner named by
//! the chain, then waits for the corner it just reached. A corner with a
//! negative `wait` parks `nextthink` behind the pusher's local time, which the
//! engine never runs, so the train stops there permanently.
//!
//! Interpolation is the same shared [`BrushDoor`] driver the doors use, one
//! rebuild per leg, so render and collision keep consuming one drift-free
//! transform and no new collision code exists.

use psx_bsp::mover::BrushDoor;
use psx_bsp::pxbsp::{entity_class, entity_flags, PxbspBrushDoor, PxbspEntity};
use quake_formats::{BrushModel, MapEntity, Vec3I32};

use crate::bsp_axis_adapter::{psoxide_point_to_quake, quake_point_to_psoxide};
use crate::collision::BrushTransform;
use crate::targets::TargetEntitySource;

const CLASS_FUNC_TRAIN: u8 = 0x11;
const CLASS_PATH_CORNER: u8 = 0x45;

/// `if (!self.speed) self.speed = 100`.
pub const TRAIN_DEFAULT_SPEED: i16 = 100;
/// `if (!self.dmg) self.dmg = 2`.
pub const TRAIN_DEFAULT_DAMAGE: i16 = 2;
/// `train_wait`'s `nextthink = ltime + 0.1` when the reached corner has no
/// authored wait.
pub const TRAIN_LEG_GAP_TICKS: u16 = 6;
/// `train_blocked` re-arms after half a second.
pub const TRAIN_BLOCK_COOLDOWN_TICKS: u16 = 30;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrainState {
    /// Targetnamed and not yet used.
    Idle,
    Moving,
    Waiting,
    /// Parked on a corner whose authored wait is negative.
    Stopped,
}

/// One authored `path_corner`, read straight from the cooked entity table.
///
/// Corners are a data index into that table, not a runtime slot, so no pool
/// grows with the 174 corners Episode 1 authors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PathCorner {
    pub source_index: u16,
    pub origin: Vec3I32,
    pub wait: i32,
    pub next: u16,
    /// The matched entity's own `origin.x`, read straight off the lookup and
    /// never through the corner's own copy. The guest and the host disagree on
    /// exactly this component, so the two candidate stages are kept apart.
    pub probe_x: i32,
}

impl PathCorner {
    /// Caller-owned storage for [`find_path_corner_into`].
    pub const EMPTY: Self = Self {
        source_index: 0,
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        wait: 0,
        next: 0,
        probe_x: 0,
    };
}

/// `find (world, targetname, self.target)` restricted to path corners.
///
/// The corner is written into caller-owned storage rather than returned by
/// value. This is the only source-level path between a cooked corner and a leg
/// length, and the guest once computed 27804 ticks for a leg the host measures
/// at 87, which is the saturated `isqrt_i32(i32::MAX) * 60 / 100`: whatever the
/// guest fed `travel_ticks` was about four thousand times too large. The cause
/// was never identified. Rather than leave a 28 byte `Option<PathCorner>`
/// crossing an experimental MIPS-I backend on the one path that produced a
/// wrong number, the aggregate does not cross it at all. That removes a
/// suspect; it does not prove one, and the guest-side leg length in
/// `map-regress` is what says the train moves correctly on real hardware.
#[optimize(size)]
#[inline(never)]
pub fn find_path_corner_into<S>(source: &S, target_name: u16, corner: &mut PathCorner) -> bool
where
    S: TargetEntitySource + ?Sized,
{
    if target_name == 0 {
        return false;
    }
    for index in 0..source.entity_count() {
        let Some(candidate) = source.entity_at(index) else {
            return false;
        };
        if candidate.class_name != CLASS_PATH_CORNER || candidate.target_name != target_name {
            continue;
        }
        corner.source_index = index as u16;
        corner.probe_x = candidate.origin.x;
        corner.origin = candidate.origin;
        corner.wait = candidate.wait;
        corner.next = candidate.target;
        return true;
    }
    false
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QuakeTrain {
    model_index: u16,
    /// Q20.12 model-local `mins`. The origin is always `corner - mins`.
    mins_offset: Vec3I32,
    origin: Vec3I32,
    speed: i16,
    damage: i16,
    state: TrainState,
    /// `targetname` of the corner this train heads to next.
    next_corner: u16,
    /// Authored wait of the corner currently being approached.
    pending_wait: i32,
    wait_ticks: u16,
    driver: Option<BrushDoor>,
    leg_end: Vec3I32,
    corner_arrivals: u16,
    /// Guest diagnostics for the leg the driver was last built from, in whole
    /// units plus the distance `travel_ticks` derived from them. The guest and
    /// the host disagree here and nowhere else, so the inputs and the output of
    /// that one calculation are kept observable on both.
    leg_offset: Vec3I32,
    leg_distance: i32,
    leg_probe_x: i32,
    /// Ticks the current leg takes. Kept observable so both the host tests and
    /// the guest gate can read the same number off the same code.
    leg_ticks: u16,
}

impl QuakeTrain {
    /// Build a train from its cooked entity, or `None` for any other class.
    ///
    /// The train is placed on its first corner immediately. The original does
    /// this one frame late (`func_train_find` at `ltime + 0.1`) only so the
    /// corners have had a chance to spawn; this loader already has the whole
    /// authored table in hand.
    #[optimize(size)]
    pub fn from_entity<S>(source_entity: MapEntity, model: BrushModel, source: &S) -> Option<Self>
    where
        S: TargetEntitySource + ?Sized,
    {
        if source_entity.class_name != CLASS_FUNC_TRAIN {
            return None;
        }
        let model_index = source_entity
            .model
            .checked_neg()
            .and_then(|index| u16::try_from(index).ok())
            .filter(|&index| index != 0)?;
        let mins_offset = Vec3I32 {
            x: i32::from(model.mins.x) << 12,
            y: i32::from(model.mins.y) << 12,
            z: i32::from(model.mins.z) << 12,
        };
        let mut first = PathCorner::EMPTY;
        if !find_path_corner_into(source, source_entity.target, &mut first) {
            return None;
        }
        let speed = if source_entity.speed == 0 {
            TRAIN_DEFAULT_SPEED
        } else {
            source_entity.speed
        };
        let damage = if source_entity.damage == 0 {
            TRAIN_DEFAULT_DAMAGE
        } else {
            source_entity.damage
        };
        // Built inline, never through a `Vec3I32`-returning helper: see
        // `begin_leg` for the guest evidence that made that a rule here.
        let origin = Vec3I32 {
            x: first.origin.x.saturating_sub(mins_offset.x),
            y: first.origin.y.saturating_sub(mins_offset.y),
            z: first.origin.z.saturating_sub(mins_offset.z),
        };
        let mut train = Self {
            model_index,
            mins_offset,
            origin,
            speed,
            damage,
            state: TrainState::Idle,
            next_corner: first.next,
            pending_wait: 0,
            wait_ticks: 0,
            driver: None,
            leg_end: origin,
            corner_arrivals: 0,
            leg_offset: Vec3I32 { x: 0, y: 0, z: 0 },
            leg_distance: 0,
            leg_probe_x: 0,
            leg_ticks: 0,
        };
        if source_entity.target_name == 0 {
            // Not triggered, so start immediately.
            train.begin_leg(source);
        }
        Some(train)
    }

    #[optimize(size)]
    pub const fn state(&self) -> TrainState {
        self.state
    }

    #[optimize(size)]
    pub const fn origin(&self) -> Vec3I32 {
        self.origin
    }

    #[optimize(size)]
    pub const fn damage(&self) -> i16 {
        self.damage
    }

    #[optimize(size)]
    pub const fn speed(&self) -> i16 {
        self.speed
    }

    /// Ticks the current leg takes. Read by `map-regress` on the guest, which
    /// is where a leg length counts as evidence.
    #[optimize(size)]
    pub const fn leg_ticks(&self) -> u16 {
        self.leg_ticks
    }

    #[optimize(size)]
    pub const fn model_index(&self) -> u16 {
        self.model_index
    }

    /// Corners reached since the map loaded. Probe and test observable.
    #[optimize(size)]
    pub const fn corner_arrivals(&self) -> u16 {
        self.corner_arrivals
    }

    /// The last leg's inputs and derived length, into caller-owned storage,
    /// all in whole units: `[dx, dy, dz, distance, leg_end, origin,
    /// corner_x_as_read, mins_y, mins_z]`. `corner_x_as_read` is the matched
    /// corner's own `origin.x` taken at the lookup, so a wrong leg names which
    /// side of the copy it went wrong on.
    #[optimize(size)]
    pub fn leg_debug_into(&self, out: &mut [i32; 13]) {
        out[0] = self.leg_offset.x;
        out[1] = self.leg_offset.y;
        out[2] = self.leg_offset.z;
        out[3] = self.leg_distance;
        out[4] = self.leg_end.x >> 12;
        out[5] = self.leg_end.y >> 12;
        out[6] = self.leg_end.z >> 12;
        out[7] = self.origin.x >> 12;
        out[8] = self.origin.y >> 12;
        out[9] = self.origin.z >> 12;
        out[10] = self.leg_probe_x >> 12;
        out[11] = self.mins_offset.y >> 12;
        out[12] = self.mins_offset.z >> 12;
    }

    /// The one Quake-space transform shared by render and collision.
    #[optimize(size)]
    pub fn transform(&self) -> BrushTransform {
        BrushTransform::translated(self.origin)
    }

    /// `train_use`: only an idle train responds, every later use is ignored.
    #[optimize(size)]
    pub fn activate<S>(&mut self, source: &S) -> bool
    where
        S: TargetEntitySource + ?Sized,
    {
        if self.state != TrainState::Idle {
            return false;
        }
        self.begin_leg(source);
        true
    }

    /// Advance exactly one 60 Hz tick. Returns `true` when the origin moved.
    #[optimize(size)]
    pub fn tick<S>(&mut self, source: &S) -> bool
    where
        S: TargetEntitySource + ?Sized,
    {
        match self.state {
            TrainState::Idle | TrainState::Stopped => false,
            TrainState::Waiting => {
                self.wait_ticks = self.wait_ticks.saturating_sub(1);
                if self.wait_ticks == 0 {
                    self.begin_leg(source);
                }
                false
            }
            TrainState::Moving => {
                let before = self.origin;
                if let Some(driver) = self.driver.as_mut() {
                    driver.set_open(true);
                    driver.tick();
                    self.origin = psoxide_point_to_quake(driver.transform().origin);
                    if driver.fully_open() {
                        self.arrive();
                    }
                } else {
                    self.arrive();
                }
                self.origin != before
            }
        }
    }

    /// `train_next`: read the next corner, take its wait, and start moving.
    #[optimize(size)]
    fn begin_leg<S>(&mut self, source: &S)
    where
        S: TargetEntitySource + ?Sized,
    {
        let mut corner = PathCorner::EMPTY;
        if !find_path_corner_into(source, self.next_corner, &mut corner) {
            // `train_next` calls objerror on a broken chain. A shipping guest
            // cannot abort a map, so the train parks where it is.
            self.state = TrainState::Stopped;
            self.driver = None;
            return;
        }
        self.next_corner = corner.next;
        self.pending_wait = corner.wait;
        self.leg_probe_x = corner.probe_x;
        // Both of these are built inline rather than through the `subtract`
        // helper, and that is load-bearing rather than a style choice.
        // `map-regress` measured this exact leg on the guest at 27804 ticks,
        // the saturated `isqrt_i32(i32::MAX) * 60 / 100`, and reported the
        // offset it derived as `(524287, -144, 0)` units: y was right, z was
        // right, and x alone came back as `i32::MAX >> 12`. A single wrong
        // leading component out of a by-value `Vec3I32` return is the same
        // shape `fly_move` hit. The mechanism is still not identified, so this
        // removes the construct instead of theorising about it, and the guest
        // gate is what says it stayed removed.
        self.leg_end = Vec3I32 {
            x: corner.origin.x.saturating_sub(self.mins_offset.x),
            y: corner.origin.y.saturating_sub(self.mins_offset.y),
            z: corner.origin.z.saturating_sub(self.mins_offset.z),
        };
        let offset = Vec3I32 {
            x: self.leg_end.x.saturating_sub(self.origin.x),
            y: self.leg_end.y.saturating_sub(self.origin.y),
            z: self.leg_end.z.saturating_sub(self.origin.z),
        };
        self.state = TrainState::Moving;
        if offset == (Vec3I32 { x: 0, y: 0, z: 0 }) {
            // Stock maps chain two corners at the same point. The shared
            // driver rejects a zero-length interpolation, so arrive at once.
            self.driver = None;
            return;
        }
        self.leg_offset = Vec3I32 {
            x: offset.x >> 12,
            y: offset.y >> 12,
            z: offset.z >> 12,
        };
        self.leg_distance = leg_distance(offset);
        self.leg_ticks = travel_ticks(offset, self.speed);
        self.driver = BrushDoor::from_entity(
            PxbspEntity {
                class_id: entity_class::BRUSH_DOOR,
                flags: entity_flags::ENABLED,
                model: self.model_index,
                origin: quake_point_to_psoxide(self.origin),
                ..PxbspEntity::default()
            },
            PxbspBrushDoor::new(quake_point_to_psoxide(offset), self.leg_ticks),
        )
        .ok();
        if self.driver.is_none() {
            self.arrive();
        }
    }

    /// `train_wait`: hold for the reached corner's authored wait.
    #[optimize(size)]
    fn arrive(&mut self) {
        self.origin = self.leg_end;
        self.driver = None;
        self.corner_arrivals = self.corner_arrivals.saturating_add(1);
        if self.pending_wait < 0 {
            self.state = TrainState::Stopped;
            self.wait_ticks = 0;
            return;
        }
        self.state = TrainState::Waiting;
        self.wait_ticks = if self.pending_wait > 0 {
            seconds_to_ticks(self.pending_wait)
        } else {
            TRAIN_LEG_GAP_TICKS
        };
    }
}

/// Whole-unit length of one leg. Split out of [`travel_ticks`] so both the
/// host tests and the guest gate can read the same intermediate.
#[optimize(size)]
fn leg_distance(offset: Vec3I32) -> i32 {
    let x = offset.x >> 12;
    let y = offset.y >> 12;
    let z = offset.z >> 12;
    psx_math::int32::isqrt_i32(
        psx_math::int32::square_i32_saturating(x)
            .saturating_add(psx_math::int32::square_i32_saturating(y))
            .saturating_add(psx_math::int32::square_i32_saturating(z)),
    )
    .max(1)
}

#[optimize(size)]
fn travel_ticks(offset: Vec3I32, speed: i16) -> u16 {
    let distance = leg_distance(offset);
    let speed = i32::from(speed).max(1);
    distance
        .saturating_mul(60)
        .saturating_add(speed - 1)
        .checked_div(speed)
        .unwrap_or(1)
        .clamp(1, i32::from(u16::MAX)) as u16
}

#[optimize(size)]
fn seconds_to_ticks(value: i32) -> u16 {
    let whole = value >> 12;
    let fraction = value & 0x0fff;
    whole
        .saturating_mul(60)
        .saturating_add(fraction.saturating_mul(60) >> 12)
        .clamp(1, i32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::Vec3I16;

    #[optimize(size)]
    fn corner(target_name: u16, next: u16, x: i32, wait: i32) -> MapEntity {
        MapEntity {
            class_name: CLASS_PATH_CORNER,
            target_name,
            target: next,
            wait,
            origin: Vec3I32 {
                x: x << 12,
                y: 0,
                z: 0,
            },
            ..MapEntity::default()
        }
    }

    #[optimize(size)]
    fn train_entity(target: u16, target_name: u16, speed: i16) -> MapEntity {
        MapEntity {
            class_name: CLASS_FUNC_TRAIN,
            model: -2,
            target,
            target_name,
            speed,
            ..MapEntity::default()
        }
    }

    #[optimize(size)]
    fn model() -> BrushModel {
        BrushModel {
            mins: Vec3I16 { x: 8, y: 0, z: 0 },
            maxs: Vec3I16 { x: 24, y: 8, z: 8 },
            ..BrushModel::default()
        }
    }

    #[optimize(size)]
    fn run<S: TargetEntitySource + ?Sized>(train: &mut QuakeTrain, source: &S, ticks: usize) {
        for _ in 0..ticks {
            train.tick(source);
        }
    }

    #[optimize(size)]
    #[test]
    fn an_untargeted_train_spawns_on_its_first_corner_minus_its_own_mins() {
        let source = [
            train_entity(1, 0, 60),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, 0),
        ];
        let train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        assert_eq!(train.origin().x, (100 - 8) << 12);
        assert_eq!(train.state(), TrainState::Moving);
    }

    #[optimize(size)]
    #[test]
    fn a_targetnamed_train_waits_for_its_first_use_and_ignores_later_ones() {
        let source = [
            train_entity(1, 9, 60),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, 0),
        ];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        assert_eq!(train.state(), TrainState::Idle);
        run(&mut train, &source[..], 120);
        assert_eq!(train.origin().x, (100 - 8) << 12);

        assert!(train.activate(&source[..]));
        assert_eq!(train.state(), TrainState::Moving);
        assert!(!train.activate(&source[..]));
    }

    #[optimize(size)]
    #[test]
    fn the_train_walks_its_corner_chain_and_loops() {
        // 60 units at 60 units per second is exactly one second per leg.
        let source = [
            train_entity(1, 0, 60),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, 0),
        ];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        run(&mut train, &source[..], 60);
        assert_eq!(train.origin().x, (160 - 8) << 12);
        assert_eq!(train.corner_arrivals(), 1);
        // The reached corner has no authored wait, so the original still holds
        // for 0.1 seconds before the next leg.
        assert_eq!(train.state(), TrainState::Waiting);
        run(&mut train, &source[..], TRAIN_LEG_GAP_TICKS as usize + 60);
        assert_eq!(train.origin().x, (100 - 8) << 12);
        assert_eq!(train.corner_arrivals(), 2);
    }

    #[optimize(size)]
    #[test]
    fn an_authored_corner_wait_holds_the_train_for_that_many_ticks() {
        let source = [
            train_entity(1, 0, 60),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, 2 << 12),
        ];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        run(&mut train, &source[..], 60);
        assert_eq!(train.state(), TrainState::Waiting);
        run(&mut train, &source[..], 119);
        assert_eq!(train.state(), TrainState::Waiting);
        assert_eq!(train.origin().x, (160 - 8) << 12);
        run(&mut train, &source[..], 1);
        assert_eq!(train.state(), TrainState::Moving);
    }

    #[optimize(size)]
    #[test]
    fn a_negative_corner_wait_parks_the_train_there_forever() {
        let source = [
            train_entity(1, 0, 60),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, -4096),
        ];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        run(&mut train, &source[..], 60);
        assert_eq!(train.state(), TrainState::Stopped);
        run(&mut train, &source[..], 6_000);
        assert_eq!(train.state(), TrainState::Stopped);
        assert_eq!(train.origin().x, (160 - 8) << 12);
        assert_eq!(train.corner_arrivals(), 1);
    }

    #[optimize(size)]
    #[test]
    fn a_zero_length_leg_arrives_at_once_instead_of_rejecting_the_driver() {
        // E1M2 chains two corners at the same point.
        let source = [
            train_entity(1, 0, 60),
            corner(1, 2, 100, 0),
            corner(2, 3, 100, 0),
            corner(3, 1, 160, -4096),
        ];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        run(&mut train, &source[..], 1);
        assert_eq!(train.corner_arrivals(), 1);
        assert_eq!(train.origin().x, (100 - 8) << 12);
        run(&mut train, &source[..], TRAIN_LEG_GAP_TICKS as usize + 60);
        assert_eq!(train.state(), TrainState::Stopped);
        assert_eq!(train.origin().x, (160 - 8) << 12);
    }

    #[optimize(size)]
    #[test]
    fn a_broken_chain_parks_the_train_instead_of_aborting_the_map() {
        let source = [train_entity(1, 0, 60), corner(1, 77, 100, 0)];
        let mut train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        assert_eq!(train.state(), TrainState::Stopped);
        run(&mut train, &source[..], 600);
        assert_eq!(train.origin().x, (100 - 8) << 12);

        // No first corner at all is not a train.
        let orphan = [train_entity(5, 0, 60)];
        assert!(QuakeTrain::from_entity(orphan[0], model(), &orphan[..]).is_none());
    }

    #[optimize(size)]
    #[test]
    fn missing_speed_and_damage_take_the_original_defaults() {
        let source = [
            train_entity(1, 0, 0),
            corner(1, 2, 100, 0),
            corner(2, 1, 160, 0),
        ];
        let train = QuakeTrain::from_entity(source[0], model(), &source[..]).expect("train");
        assert_eq!(train.damage(), TRAIN_DEFAULT_DAMAGE);
        // 60 units at the default 100 units per second is 36 ticks.
        let mut moving = train;
        run(&mut moving, &source[..], 36);
        assert_eq!(moving.corner_arrivals(), 1);
    }
}
