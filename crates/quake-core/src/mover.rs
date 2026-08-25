//! Quake brush-mover policy over PSoXide's canonical fixed-tick driver.
//!
//! Legacy Quake entities decide endpoints, automatic activation, waits, and
//! toggle behavior. [`psx_bsp::mover::BrushDoor`] owns interpolation and
//! reversal so render and collision always consume one drift-free transform.

use psx_bsp::mover::{BrushDoor, BrushDoorError};
use psx_bsp::pxbsp::{entity_class, entity_flags, PxbspBrushDoor, PxbspEntity};
use psx_math::int32::{isqrt_i32, square_i32_saturating};
use quake_formats::{BrushModel, MapEntity, Vec3I16, Vec3I32};

use crate::bsp_axis_adapter::{psoxide_point_to_quake, quake_point_to_psoxide};
use crate::collision::BrushTransform;

const CLASS_FUNC_BUTTON: u8 = 0x0b;
const CLASS_FUNC_DOOR: u8 = 0x0c;
const CLASS_FUNC_DOOR_SECRET: u8 = 0x0d;
const CLASS_FUNC_PLAT: u8 = 0x10;

/// Stable cooked sound IDs used by the original brush-mover callbacks.
pub const BUTTON_ACTIVATE_SOUND: i16 = 0x1c;
pub const DOOR_MOVE_SOUND: i16 = 0x34;
pub const DOOR_STOP_SOUND: i16 = 0x35;
/// `func_door_secret`'s default/base travel and stop pair. QuakeC assigns
/// `basesec1` to `noise2` (each moving leg) and `basesec2` to both `noise1`
/// and `noise3` (activation latch and each reached endpoint).
pub const SECRET_MOVE_SOUND: i16 = 0x2e;
pub const SECRET_STOP_SOUND: i16 = 0x2f;
pub const SECRET_MEDIEVAL_LATCH_SOUND: i16 = 0x38;
pub const SECRET_MEDIEVAL_MOVE_SOUND: i16 = 0x3f;
pub const SECRET_MEDIEVAL_STOP_SOUND: i16 = 0xe2;
pub const PLAT_MOVE_SOUND: i16 = 0x86;
pub const PLAT_STOP_SOUND: i16 = 0x87;

/// `func_plat` bit 1, `PLAT_LOW_TRIGGER`: `plat_spawn_inside_trigger` keeps
/// only the bottom eight units of the shaft, so a rider standing on the raised
/// deck is outside the volume and cannot send the lift away again.
pub const PLAT_LOW_TRIGGER: u16 = 1;

/// `func_door_secret` forces `speed = 50` regardless of the authored value.
const SECRET_SPEED: i16 = 50;
/// `fd_secret_move1` and `fd_secret_move5`: `nextthink = ltime + 1.0`, the
/// pause the door takes between its sideways and its forward leg.
const SECRET_PAUSE_TICKS: u16 = 60;

/// Up to two sounds emitted by one mover transition.
///
/// A short travel can begin and reach its endpoint inside one gameplay frame,
/// so the start and stop callbacks must both survive the fixed-tick batch.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MoverSoundEvents {
    sounds: [Option<i16>; 2],
}

impl MoverSoundEvents {
    /// Events in original callback order.
    pub fn iter(self) -> impl Iterator<Item = i16> {
        self.sounds.into_iter().flatten()
    }
}

/// Reproduce the original button, door, and platform sound callbacks from a
/// committed mover state transition.
///
/// Ordinary doors with authored `sounds 0` are silent. Secret doors always
/// use their fixed travel pair. Platforms default `sounds 0` to the medium
/// platform pair, exactly as `func_plat` does at spawn.
pub fn mover_sound_events(
    class_name: u8,
    authored_sounds: i8,
    before: QuakeMoverState,
    after: QuakeMoverState,
) -> MoverSoundEvents {
    let started = matches!(
        (before, after),
        (
            QuakeMoverState::Bottom,
            QuakeMoverState::Up | QuakeMoverState::Top
        ) | (
            QuakeMoverState::Top,
            QuakeMoverState::Down | QuakeMoverState::Bottom
        ) | (QuakeMoverState::Up, QuakeMoverState::Down)
            | (QuakeMoverState::Down, QuakeMoverState::Up)
    );
    let stopped = matches!(
        (before, after),
        (QuakeMoverState::Up, QuakeMoverState::Top)
            | (QuakeMoverState::Down, QuakeMoverState::Bottom)
            | (QuakeMoverState::Bottom, QuakeMoverState::Top)
            | (QuakeMoverState::Top, QuakeMoverState::Bottom)
    );
    if class_name == CLASS_FUNC_BUTTON {
        return MoverSoundEvents {
            sounds: [started.then_some(BUTTON_ACTIVATE_SOUND), None],
        };
    }
    if class_name == CLASS_FUNC_DOOR_SECRET {
        let secret_started = matches!(
            (before, after),
            (
                QuakeMoverState::Bottom,
                QuakeMoverState::Up | QuakeMoverState::Top
            )
        );
        return MoverSoundEvents {
            // `fd_secret_use` voices the latch first and immediately starts
            // the first movement sound on the same entity channel.
            sounds: if secret_started {
                if authored_sounds == 1 {
                    [
                        Some(SECRET_MEDIEVAL_LATCH_SOUND),
                        Some(SECRET_MEDIEVAL_MOVE_SOUND),
                    ]
                } else {
                    [Some(SECRET_STOP_SOUND), Some(SECRET_MOVE_SOUND)]
                }
            } else {
                [None, None]
            },
        };
    }
    if class_name == CLASS_FUNC_DOOR && authored_sounds != 0 {
        return MoverSoundEvents {
            sounds: [
                started.then_some(DOOR_MOVE_SOUND),
                stopped.then_some(DOOR_STOP_SOUND),
            ],
        };
    }
    if class_name == CLASS_FUNC_PLAT {
        return MoverSoundEvents {
            sounds: [
                started.then_some(PLAT_MOVE_SOUND),
                stopped.then_some(PLAT_STOP_SOUND),
            ],
        };
    }
    MoverSoundEvents {
        sounds: [None, None],
    }
}

/// Select the authored secret-door travel and endpoint callbacks. The first
/// medieval activation uses `latch2` above; every later stop is `drclos4`.
pub const fn secret_tick_sound(authored_sounds: i8, sound: i16) -> i16 {
    if authored_sounds != 1 {
        return sound;
    }
    match sound {
        SECRET_MOVE_SOUND => SECRET_MEDIEVAL_MOVE_SOUND,
        SECRET_STOP_SOUND => SECRET_MEDIEVAL_STOP_SOUND,
        _ => sound,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum QuakeMoverError {
    InvalidModel(i16),
    Shared(BrushDoorError),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum QuakeMoverState {
    Bottom,
    Top,
    Up,
    Down,
}

/// One fixed-tick mover result. Secret doors are the only movers with a sound
/// callback between their public `Up`/`Top`/`Down`/`Bottom` states, so the
/// event rides beside the existing movement bit without widening a mover.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MoverTickResult {
    pub moved: bool,
    pub sound: Option<i16>,
}

/// Whether a touch/use event may start or reverse this mover state.
///
/// A `func_plat` trigger encloses the rider through the entire shaft.  It may
/// start from the bottom, but touching it while descending must not reverse
/// it. A `func_door_secret` is the same shape for a different reason: only
/// `fd_secret_done` reinstalls `fd_secret_use`, so a secret door ignores every
/// use until its whole six-step sequence has run. Doors and buttons retain
/// their normal downward reversal/re-arm path.
pub const fn mover_state_admits_activation(class_name: u8, state: QuakeMoverState) -> bool {
    if class_name == CLASS_FUNC_PLAT || class_name == CLASS_FUNC_DOOR_SECRET {
        matches!(state, QuakeMoverState::Bottom)
    } else {
        matches!(state, QuakeMoverState::Bottom | QuakeMoverState::Down)
    }
}

/// State only one mover class ever carries, so the classes share one slot
/// instead of each paying for a field every other mover leaves empty.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MoverExtra {
    None,
    /// A zero-travel `func_button` still owns health and fires its target.
    /// The shared interpolator needs a non-zero offset, so it advances over a
    /// sub-unit private leg while every external transform stays here.
    FixedOrigin(Vec3I32),
    /// `func_door_secret`'s second leg. The shared interpolator carries the
    /// sideways leg to `dest1`; rebuilding it for the perpendicular leg costs
    /// far more image than carrying `dest2` on top of it as whole units and
    /// its own progress.
    SecretForwardLeg {
        leg: Vec3I16,
        progress: u16,
        travel_ticks: u16,
    },
}

/// One translated legacy Quake brush model driven by the shared mover kernel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QuakeMover {
    driver: BrushDoor,
    extra: MoverExtra,
    wait_ticks: u16,
    wait_remaining: u16,
    state: QuakeMoverState,
    automatic: bool,
    /// A named `func_plat` is lowered by its target, waits at the low stop,
    /// then returns only when the player touches its inside trigger.
    targeted_plat: bool,
    toggle: bool,
    hold_open: bool,
    /// `plat_crush` always reverses; `door_blocked` reverses only while the
    /// door still intends to close (`if (self.wait >= 0)`). The damage the two
    /// deal differs and is the caller's business.
    crush_reverses: bool,
    /// Whole-unit vertical travel, for a lift's own trigger volume. Zero for
    /// everything that is not a `func_plat`, and negative for a
    /// `PLAT_LOW_TRIGGER` lift. The sign carries the flag because a sixty-first
    /// byte in this struct rounds all 128 mover slots up by four.
    plat_travel_units: i16,
}

impl QuakeMover {
    /// Translate a supported legacy Quake mover recipe into the canonical
    /// PSoXide driver. Unsupported entity classes remain ordinary brush
    /// models and return `Ok(None)`.
    #[optimize(size)]
    pub fn from_entity(
        source: MapEntity,
        model: BrushModel,
    ) -> Result<Option<Self>, QuakeMoverError> {
        let class = source.class_name;
        if !matches!(
            class,
            CLASS_FUNC_BUTTON | CLASS_FUNC_DOOR | CLASS_FUNC_DOOR_SECRET | CLASS_FUNC_PLAT
        ) {
            return Ok(None);
        }
        let model_index = source
            .model
            .checked_neg()
            .and_then(|index| u16::try_from(index).ok())
            .filter(|&index| index != 0)
            .ok_or(QuakeMoverError::InvalidModel(source.model))?;
        let size = Vec3I32 {
            x: i32::from(model.maxs.x).saturating_sub(i32::from(model.mins.x)) << 12,
            y: i32::from(model.maxs.y).saturating_sub(i32::from(model.mins.y)) << 12,
            z: i32::from(model.maxs.z).saturating_sub(i32::from(model.mins.z)) << 12,
        };
        let (mut bottom, mut top) = (source.origin, source.origin);
        let mut extra = MoverExtra::None;
        // `SetMovedir` is what a button, a door, and a secret door's
        // `makevectors` all start from; the one shared call keeps the arms
        // below from each carrying their own copy of the sine table walk.
        let direction = move_direction(source.angles);
        let (speed, wait_ticks, automatic, toggle) = match class {
            CLASS_FUNC_BUTTON => {
                let lip = i32::from(if source.count == 0 { 4 } else { source.count }) << 12;
                let distance = dot_q12(direction, size)
                    .saturating_abs()
                    .saturating_sub(lip);
                top = add(bottom, scale_q12(direction, distance));
                (
                    source.speed.max(40),
                    fixed_seconds_to_ticks(source.wait, 60),
                    false,
                    false,
                )
            }
            CLASS_FUNC_PLAT => {
                // `func_plat`: `pos2_z = origin_z - height`, or
                // `origin_z - size_z + 8` when the map authors no height, so
                // the lift's own deck thickness minus eight is the travel.
                let fall = if source.height != 0 {
                    i32::from(source.height) << 12
                } else {
                    size.z.saturating_sub(8 << 12).max(1 << 12)
                };
                top.z = top.z.saturating_sub(fall);
                if source.target_name == 0 {
                    core::mem::swap(&mut bottom, &mut top);
                }
                // `plat_hit_top`: `nextthink = ltime + 3`, three seconds.
                (source.speed.max(150), 180, source.target_name == 0, false)
            }
            CLASS_FUNC_DOOR_SECRET => {
                let (sideways, forward) = secret_legs(source.spawn_flags, direction, size);
                top = add(bottom, sideways);
                extra = forward;
                // `if (!self.wait) self.wait = 5`.
                (
                    SECRET_SPEED,
                    fixed_seconds_to_ticks(source.wait, 300),
                    false,
                    false,
                )
            }
            _ => {
                let lip = i32::from(if source.count == 0 { 8 } else { source.count }) << 12;
                let distance = dot_q12(direction, size)
                    .saturating_abs()
                    .saturating_sub(lip);
                top = add(bottom, scale_q12(direction, distance));
                // `DOOR_START_OPEN` spawns the door one whole travel away from
                // the brush it was authored in and sends it BACK there when it
                // is triggered, which the original expresses by swapping the
                // endpoints. It is not cosmetic: E1M7's lava bridge is two
                // such doors, so its authored volume is a floor that only
                // exists after Chthon's death drives them.
                if source.spawn_flags & crate::door::DOOR_START_OPEN != 0 {
                    core::mem::swap(&mut bottom, &mut top);
                }
                (
                    source.speed.max(100),
                    fixed_seconds_to_ticks(source.wait, 180),
                    source.target_name == 0
                        && source.health == 0
                        && source.spawn_flags & (8 | 16) == 0,
                    source.spawn_flags & crate::door::DOOR_TOGGLE != 0,
                )
            }
        };
        let mut open_offset = subtract(top, bottom);
        if open_offset == (Vec3I32 { x: 0, y: 0, z: 0 }) && class == CLASS_FUNC_BUTTON {
            // One Q20.12 quantum is deliberately below the whole-unit bounds
            // and render precision. It exists only to drive Bottom -> Top.
            open_offset.x = 1;
            extra = MoverExtra::FixedOrigin(bottom);
        }
        if open_offset == (Vec3I32 { x: 0, y: 0, z: 0 }) {
            // Some stock maps contain zero-travel brush recipes. Quake keeps
            // them as inert solid submodels; the shared driver correctly
            // rejects the degenerate interpolation, so preserve them as
            // ordinary static brushes instead of failing the whole map.
            return Ok(None);
        }
        // `func_door` forces `wait = -1` on every key door, so a door opened
        // with a key never closes again. `fd_secret_move3` never schedules the
        // return of a `SECRET_OPEN_ONCE` secret door.
        let hold_open = source.wait < 0
            || (class == CLASS_FUNC_DOOR && crate::door::door_key_bit(source.spawn_flags) != 0)
            || (class == CLASS_FUNC_DOOR_SECRET
                && source.spawn_flags & crate::door::SECRET_OPEN_ONCE != 0);
        let travel_ticks = travel_ticks(open_offset, speed);
        let driver = BrushDoor::from_entity(
            PxbspEntity {
                class_id: entity_class::BRUSH_DOOR,
                flags: entity_flags::ENABLED,
                model: model_index,
                origin: quake_point_to_psoxide(bottom),
                ..PxbspEntity::default()
            },
            PxbspBrushDoor::new(quake_point_to_psoxide(open_offset), travel_ticks),
        )
        .map_err(QuakeMoverError::Shared)?;
        Ok(Some(Self {
            driver,
            extra,
            wait_ticks,
            wait_remaining: 0,
            state: QuakeMoverState::Bottom,
            automatic,
            targeted_plat: class == CLASS_FUNC_PLAT && source.target_name != 0,
            toggle,
            hold_open,
            // `door_blocked` reverses exactly like `plat_crush` does, guarded
            // by `if (self.wait >= 0)`. `fd_secret_blocked` has no such branch,
            // and `func_button` has no `blocked` function at all.
            crush_reverses: class == CLASS_FUNC_PLAT
                || (class == CLASS_FUNC_DOOR && !hold_open),
            plat_travel_units: if class == CLASS_FUNC_PLAT {
                let travel =
                    (open_offset.z.saturating_abs() >> 12).clamp(0, i32::from(i16::MAX)) as i16;
                if source.spawn_flags & PLAT_LOW_TRIGGER != 0 {
                    -travel
                } else {
                    travel
                }
            } else {
                0
            },
        }))
    }

    pub const fn state(&self) -> QuakeMoverState {
        self.state
    }

    /// `plat_spawn_inside_trigger`: the volume a lift starts from.
    ///
    /// The original does not start a lift from proximity. It spawns a
    /// `SOLID_TRIGGER` inside the lift's own footprint, shrunk 25 units on
    /// both horizontal axes, reaching from eight units above the raised deck
    /// down through the whole travel. A player has to be on or in the lift.
    /// Proximity instead of this sends the lift away before the player can
    /// board it, which is exactly what E1M7's ring exposed once lifts began
    /// carrying riders.
    ///
    /// Returns the authored volume in whole units given the lift's own raised
    /// model bounds, or `false` for anything that is not a lift.
    pub fn plat_trigger_volume(
        &self,
        raised_mins: [i16; 3],
        raised_maxs: [i16; 3],
        mins: &mut [i32; 3],
        maxs: &mut [i32; 3],
    ) -> bool {
        if self.plat_travel_units == 0 {
            return false;
        }
        const INSET: i32 = 25;
        const LIP: i32 = 8;
        let top = i32::from(raised_maxs[2]) + LIP;
        mins[0] = i32::from(raised_mins[0]) + INSET;
        mins[1] = i32::from(raised_mins[1]) + INSET;
        mins[2] = top - (i32::from(self.plat_travel_units.unsigned_abs()) + LIP);
        maxs[0] = i32::from(raised_maxs[0]) - INSET;
        maxs[1] = i32::from(raised_maxs[1]) - INSET;
        // `if (self.spawnflags & PLAT_LOW_TRIGGER) tmax_z = tmin_z + 8`. The
        // volume is an eight unit slab at the low stop instead of the whole
        // shaft, so the raised deck is somewhere the rider can stand.
        maxs[2] = if self.plat_travel_units < 0 {
            mins[2] + LIP
        } else {
            top
        };
        // A lift narrower than the inset keeps its own footprint: the original
        // would produce an inside-out box and touch nothing at all.
        for axis in 0..2 {
            if mins[axis] >= maxs[axis] {
                mins[axis] = i32::from(raised_mins[axis]);
                maxs[axis] = i32::from(raised_maxs[axis]);
            }
        }
        true
    }

    pub const fn automatic(&self) -> bool {
        self.automatic
    }

    /// `plat_center_touch` for the fixed mover state machine.
    ///
    /// An unnamed platform starts low and the touch sends it up. A named
    /// platform starts raised, is sent down by `plat_use`, and only this touch
    /// sends it back up from the low stop. Touching an unnamed platform while
    /// it is already raised postpones its return, matching Quake's one-second
    /// `nextthink` refresh.
    pub fn plat_center_touch(&mut self) -> bool {
        if self.plat_travel_units == 0 {
            return false;
        }
        if self.targeted_plat {
            if self.state != QuakeMoverState::Top {
                return false;
            }
            self.state = QuakeMoverState::Down;
            self.wait_remaining = 0;
            self.driver.set_open(false);
            return true;
        }
        match self.state {
            QuakeMoverState::Bottom => {
                self.activate();
                true
            }
            QuakeMoverState::Top => {
                self.wait_remaining = 60;
                false
            }
            QuakeMoverState::Up | QuakeMoverState::Down => false,
        }
    }

    pub const fn toggle(&self) -> bool {
        self.toggle
    }

    /// The one Quake-space transform shared by render, broad phase, and BSP
    /// collision for this mover.
    pub fn transform(&self) -> BrushTransform {
        if let MoverExtra::FixedOrigin(origin) = self.extra {
            return BrushTransform::translated(origin);
        }
        let mut origin = psoxide_point_to_quake(self.driver.transform().origin);
        if let MoverExtra::SecretForwardLeg {
            leg,
            progress,
            travel_ticks,
        } = self.extra
        {
            // A secret door's forward leg rides on top of `dest1`.
            origin = add(origin, secret_forward_offset(leg, progress, travel_ticks));
        }
        BrushTransform::translated(origin)
    }

    /// Whether a use/touch/trigger event has anything to do with this mover
    /// in its current state. Extends [`mover_state_admits_activation`] with
    /// `door_fire`'s toggle branch: a `DOOR_TOGGLE` door that is up or on its
    /// way up is sent back down instead of being ignored.
    pub const fn admits_activation(&self, class_name: u8) -> bool {
        mover_state_admits_activation(class_name, self.state)
            || (self.toggle && matches!(self.state, QuakeMoverState::Up | QuakeMoverState::Top))
    }

    pub fn activate(&mut self) {
        // `door_fire`: `if (self.spawnflags & DOOR_TOGGLE)` and the door is
        // `STATE_UP` or `STATE_TOP`, `door_go_down` instead of `door_go_up`.
        if self.toggle && matches!(self.state, QuakeMoverState::Up | QuakeMoverState::Top) {
            self.state = QuakeMoverState::Down;
            self.wait_remaining = 0;
            self.driver.set_open(false);
            return;
        }
        // Only `fd_secret_done` reinstalls `fd_secret_use`, so a secret door
        // that is still walking its legs back ignores the use entirely.
        let reversible = matches!(self.state, QuakeMoverState::Down) && !self.is_secret();
        if matches!(self.state, QuakeMoverState::Bottom) || reversible {
            self.state = QuakeMoverState::Up;
            self.wait_remaining = 0;
            self.driver.set_open(true);
        }
    }

    const fn is_secret(&self) -> bool {
        matches!(self.extra, MoverExtra::SecretForwardLeg { .. })
    }

    /// `fd_secret_move1` through `fd_secret_move6`: sideways, one second
    /// still, forward, `wait` at the top, then the same two legs back with
    /// the same second of stillness between them.
    ///
    /// Returns whether the sideways leg advanced; the forward leg is the
    /// caller's own `progress` comparison.
    fn secret_tick(&mut self) -> MoverTickResult {
        let MoverExtra::SecretForwardLeg {
            progress,
            travel_ticks,
            ..
        } = self.extra
        else {
            return MoverTickResult::default();
        };
        // The pause between the legs reuses the top-wait counter, since a
        // secret door is never between its legs and holding open at once.
        if self.wait_remaining > 0 && !matches!(self.state, QuakeMoverState::Top) {
            self.wait_remaining -= 1;
            return MoverTickResult::default();
        }
        let mut sideways = false;
        let mut forward = progress;
        let mut sound = None;
        match self.state {
            QuakeMoverState::Bottom => {}
            QuakeMoverState::Up if !self.driver.fully_open() => {
                self.driver.set_open(true);
                sideways = self.driver.tick();
                if self.driver.fully_open() {
                    self.wait_remaining = SECRET_PAUSE_TICKS;
                    sound = Some(SECRET_STOP_SOUND);
                }
            }
            QuakeMoverState::Up => {
                if forward == 0 {
                    sound = Some(SECRET_MOVE_SOUND);
                }
                forward += 1;
                if forward >= travel_ticks {
                    self.state = QuakeMoverState::Top;
                    self.wait_remaining = self.wait_ticks;
                    sound = Some(SECRET_STOP_SOUND);
                }
            }
            // `fd_secret_move3` never schedules `fd_secret_move4` for a
            // `SECRET_OPEN_ONCE` door.
            QuakeMoverState::Top if self.hold_open => {}
            QuakeMoverState::Top => {
                if self.wait_remaining > 1 {
                    self.wait_remaining -= 1;
                } else {
                    // `fd_secret_move4` calls `SUB_CalcMove` the moment the
                    // wait expires, so the return starts on this same tick.
                    self.state = QuakeMoverState::Down;
                    forward = forward.saturating_sub(1);
                    self.wait_remaining = if forward == 0 { SECRET_PAUSE_TICKS } else { 0 };
                    sound = Some(SECRET_MOVE_SOUND);
                }
            }
            QuakeMoverState::Down if forward > 0 => {
                forward -= 1;
                if forward == 0 {
                    self.wait_remaining = SECRET_PAUSE_TICKS;
                    sound = Some(SECRET_STOP_SOUND);
                }
            }
            QuakeMoverState::Down => {
                if self.driver.fully_open() {
                    sound = Some(SECRET_MOVE_SOUND);
                }
                self.driver.set_open(false);
                sideways = self.driver.tick();
                if self.driver.fully_closed() {
                    self.state = QuakeMoverState::Bottom;
                    sound = Some(SECRET_STOP_SOUND);
                }
            }
        }
        if let MoverExtra::SecretForwardLeg { progress, .. } = &mut self.extra {
            *progress = forward;
        }
        MoverTickResult {
            moved: sideways || progress != forward,
            sound,
        }
    }

    /// `plat_crush`: a lift that could not push its rider out of the way turns
    /// around, so the crush is one damage tick rather than a trap.
    ///
    /// The original also rewinds the pusher's own move for the blocked tick.
    /// This driver interpolates between fixed endpoints and has no tick
    /// rewind, so the lift keeps the fraction of a tick it already travelled
    /// and starts back from there. At the authored 150 units/second that is
    /// under three units.
    ///
    /// `door_blocked` turns around the same way behind `if (self.wait >= 0)`.
    /// It deals the door's own `dmg` rather than the lift's fixed point, so the
    /// caller picks the damage from the class, not from this return.
    ///
    /// Returns false for a mover that was not mid-travel, or whose original
    /// `blocked` function does not reverse at all.
    pub fn crush_reverse(&mut self) -> bool {
        if !self.crush_reverses {
            return false;
        }
        match self.state {
            QuakeMoverState::Up => {
                self.state = QuakeMoverState::Down;
                self.wait_remaining = 0;
                self.driver.set_open(false);
                true
            }
            QuakeMoverState::Down => {
                self.state = QuakeMoverState::Up;
                self.wait_remaining = 0;
                self.driver.set_open(true);
                true
            }
            QuakeMoverState::Bottom | QuakeMoverState::Top => false,
        }
    }

    /// Advance exactly one 60 Hz Quake simulation tick.
    ///
    /// Returns whether either leg advanced. The transform is a pure function of
    /// that progress, so a false answer is a brush that certainly did not move.
    /// A true answer is a brush that advanced by at least one interpolator
    /// tick, which on a very long, very slow leg can still round to the same
    /// Q20.12 origin; the placement rebuild the caller drives off this is
    /// idempotent, so the rounding case costs only the comparison it already
    /// makes. Asking the interpolator instead of differencing two transforms
    /// keeps three divides per mover per tick out of the frame.
    pub fn tick(&mut self) -> bool {
        self.tick_with_sound().moved
    }

    /// Advance one tick and retain secret-door callbacks that occur between
    /// the four public mover states. Ordinary movers still report their
    /// start/stop pair through [`mover_sound_events`].
    pub fn tick_with_sound(&mut self) -> MoverTickResult {
        if self.is_secret() {
            return self.secret_tick();
        }
        let mut progressed = false;
        match self.state {
            QuakeMoverState::Bottom => {}
            QuakeMoverState::Up => {
                self.driver.set_open(true);
                progressed = self.driver.tick();
                if self.driver.fully_open() {
                    self.state = QuakeMoverState::Top;
                    self.wait_remaining = self.wait_ticks;
                }
            }
            QuakeMoverState::Top if self.toggle || self.hold_open || self.targeted_plat => {}
            QuakeMoverState::Top => {
                if self.wait_remaining > 1 {
                    self.wait_remaining -= 1;
                } else {
                    self.wait_remaining = 0;
                    self.state = QuakeMoverState::Down;
                    self.driver.set_open(false);
                    progressed = self.driver.tick();
                    if self.driver.fully_closed() {
                        self.state = QuakeMoverState::Bottom;
                    }
                }
            }
            QuakeMoverState::Down => {
                self.driver.set_open(false);
                progressed = self.driver.tick();
                if self.driver.fully_closed() {
                    self.state = QuakeMoverState::Bottom;
                }
            }
        }
        MoverTickResult {
            moved: progressed,
            sound: None,
        }
    }
}

/// Rebuild absolute whole-unit render/broad-phase bounds from the mover's
/// canonical Q20.12 transform. Recomputing from the immutable model bounds
/// avoids accumulating the fractional loss of per-tick shifted deltas.
pub fn translated_model_bounds(origin: Vec3I32, bounds: Vec3I16) -> [i16; 3] {
    let source = [bounds.x, bounds.y, bounds.z];
    let offset = [origin.x, origin.y, origin.z];
    core::array::from_fn(|axis| {
        ((offset[axis] >> 12).saturating_add(i32::from(source[axis])))
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16
    })
}

fn fixed_seconds_to_ticks(value: i32, default_ticks: u16) -> u16 {
    if value <= 0 {
        default_ticks
    } else {
        let whole = value >> 12;
        let fraction = value & 0x0fff;
        whole
            .saturating_mul(60)
            .saturating_add(fraction.saturating_mul(60) >> 12)
            .clamp(1, i32::from(u16::MAX)) as u16
    }
}

fn travel_ticks(offset: Vec3I32, speed: i16) -> u16 {
    let x = offset.x >> 12;
    let y = offset.y >> 12;
    let z = offset.z >> 12;
    ticks_for_distance(
        isqrt_i32(
            square_i32_saturating(x)
                .saturating_add(square_i32_saturating(y))
                .saturating_add(square_i32_saturating(z)),
        ),
        speed,
    )
}

/// `SUB_CalcMove` rounded up to whole 60 Hz ticks, for a leg whose whole-unit
/// length is already known without a square root.
fn ticks_for_distance(distance: i32, speed: i16) -> u16 {
    let speed = i32::from(speed).max(1);
    distance
        .max(1)
        .saturating_mul(60)
        .saturating_add(speed - 1)
        .checked_div(speed)
        .unwrap_or(1)
        .clamp(1, i32::from(u16::MAX)) as u16
}

/// Original `SetMovedir`: angle -1 is up, -2 is down, anything else is the
/// yaw direction in the horizontal plane. Returns a Q12 unit vector.
pub fn move_direction(angles: Vec3I16) -> Vec3I32 {
    if angles.x == 0 && angles.z == 0 {
        if angles.y == -1 {
            return Vec3I32 {
                x: 0,
                y: 0,
                z: 4096,
            };
        }
        if angles.y == -2 {
            return Vec3I32 {
                x: 0,
                y: 0,
                z: -4096,
            };
        }
    }
    let yaw = angles.y as u16 & 0x0fff;
    Vec3I32 {
        x: psx_math::cos_q12(yaw),
        y: psx_math::sin_q12(yaw),
        z: 0,
    }
}

/// `fd_secret_use`'s `dest1` and `dest2`, off the door's own size.
///
/// `makevectors(self.mangle)` builds the basis; the engine turns an `angle`
/// key into pure yaw, which all eighteen authored secret doors carry, so
/// `v_forward` is the very vector `SetMovedir` builds, `v_right` is its
/// perpendicular, and `v_up` is world up. Returns the sideways leg to
/// `dest1` and the forward leg that rides on top of it.
///
/// Outlined: inlining this into the spawn's already large class match cost
/// three times its own size in the guest image.
#[inline(never)]
fn secret_legs(spawn_flags: u16, forward: Vec3I32, size: Vec3I32) -> (Vec3I32, MoverExtra) {
    // Whole units throughout. An authored brush size against the Q12 unit
    // basis stays far inside i32, so this needs none of the saturating Q12
    // helpers, whose guest image cost dominated an earlier Q12 version.
    let (x, y, z) = (size.x >> 12, size.y >> 12, size.z >> 12);
    let sideways = if spawn_flags & crate::door::SECRET_1ST_DOWN != 0 {
        // `t_width = fabs(v_up * size)` and `dest1` subtracts it.
        Vec3I32 {
            x: 0,
            y: 0,
            z: -(z << 12),
        }
    } else {
        // `t_width = fabs(v_right * size)`, signed by `temp`.
        let mut width = ((x * forward.y - y * forward.x) >> 12).abs();
        if spawn_flags & crate::door::SECRET_1ST_LEFT != 0 {
            width = -width;
        }
        Vec3I32 {
            x: forward.y * width,
            y: -forward.x * width,
            z: 0,
        }
    };
    // `t_length = fabs(v_forward * size)`. `v_forward` is a unit vector, so
    // the dot product is already the leg's length and the forward leg needs
    // no square root of its own.
    let length = ((x * forward.x + y * forward.y + z * forward.z) >> 12).abs();
    (
        sideways,
        MoverExtra::SecretForwardLeg {
            // Whole units: every authored secret door is yaw-aligned, so all
            // eighteen legs round exactly.
            leg: Vec3I16 {
                x: ((forward.x * length + (1 << 11)) >> 12) as i16,
                y: ((forward.y * length + (1 << 11)) >> 12) as i16,
                z: ((forward.z * length + (1 << 11)) >> 12) as i16,
            },
            progress: 0,
            travel_ticks: ticks_for_distance(length, SECRET_SPEED),
        },
    )
}

/// How far the forward leg has travelled, interpolated exactly the way the
/// shared driver interpolates the sideways one.
///
/// `unit << 12` times `progress` does not fit a word, but the answer always
/// does: the forward leg stops at `travel_ticks`, so the leg's own length is
/// the ceiling. Writing the numerator as `q * d + r` turns the wide quotient
/// into `q * p + r * p / d`, whose every intermediate is a word, so the three
/// axes share one outlined copy of the hardware divider instead of three trips
/// through a software 64-bit routine.
fn secret_forward_offset(leg: Vec3I16, progress: u16, travel_ticks: u16) -> Vec3I32 {
    #[inline(never)]
    fn axis(unit: i16, progress: u16, travel_ticks: u16) -> i32 {
        // `q * p` is at most the whole leg in Q12 and `r * p` is under `d * d`,
        // which is under 2^32 for any `u16` tick count. Both bounds need
        // `p <= d`, which the state machine already guarantees and the clamp
        // below makes local.
        let d = u32::from(travel_ticks).max(1);
        let p = u32::from(progress).min(d);
        let n = u32::from(unit.unsigned_abs()) << 12;
        let magnitude = ((n / d) * p + (n % d) * p / d) as i32;
        if unit < 0 {
            -magnitude
        } else {
            magnitude
        }
    }
    Vec3I32 {
        x: axis(leg.x, progress, travel_ticks),
        y: axis(leg.y, progress, travel_ticks),
        z: axis(leg.z, progress, travel_ticks),
    }
}

fn dot_q12(left: Vec3I32, right: Vec3I32) -> i32 {
    psx_math::int32::mul_q12_i32(left.x, right.x)
        .saturating_add(psx_math::int32::mul_q12_i32(left.y, right.y))
        .saturating_add(psx_math::int32::mul_q12_i32(left.z, right.z))
}

fn scale_q12(vector: Vec3I32, factor: i32) -> Vec3I32 {
    Vec3I32 {
        x: psx_math::int32::mul_q12_i32(vector.x, factor),
        y: psx_math::int32::mul_q12_i32(vector.y, factor),
        z: psx_math::int32::mul_q12_i32(vector.z, factor),
    }
}

const fn subtract(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_sub(right.x),
        y: left.y.saturating_sub(right.y),
        z: left.z.saturating_sub(right.z),
    }
}

const fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::segment_overlaps_i16_bounds;

    fn model(maxs: Vec3I16) -> BrushModel {
        BrushModel {
            maxs,
            ..BrushModel::default()
        }
    }

    fn source(class_name: u8) -> MapEntity {
        MapEntity {
            class_name,
            model: -1,
            ..MapEntity::default()
        }
    }

    #[test]
    fn an_automatic_plat_does_not_reverse_while_its_rider_descends() {
        assert!(mover_state_admits_activation(
            CLASS_FUNC_PLAT,
            QuakeMoverState::Bottom
        ));
        assert!(!mover_state_admits_activation(
            CLASS_FUNC_PLAT,
            QuakeMoverState::Down
        ));
        assert!(mover_state_admits_activation(
            CLASS_FUNC_DOOR,
            QuakeMoverState::Down
        ));
    }

    #[test]
    fn mover_sounds_match_the_original_callbacks() {
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_BUTTON,
                0,
                QuakeMoverState::Bottom,
                QuakeMoverState::Up,
            )
            .sounds,
            [Some(BUTTON_ACTIVATE_SOUND), None]
        );
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_DOOR,
                1,
                QuakeMoverState::Bottom,
                QuakeMoverState::Top,
            )
            .sounds,
            [Some(DOOR_MOVE_SOUND), Some(DOOR_STOP_SOUND)]
        );
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_PLAT,
                0,
                QuakeMoverState::Top,
                QuakeMoverState::Down,
            )
            .sounds,
            [Some(PLAT_MOVE_SOUND), None]
        );
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_PLAT,
                2,
                QuakeMoverState::Down,
                QuakeMoverState::Bottom,
            )
            .sounds,
            [None, Some(PLAT_STOP_SOUND)]
        );
        assert!(mover_sound_events(
            CLASS_FUNC_DOOR,
            0,
            QuakeMoverState::Bottom,
            QuakeMoverState::Up,
        )
        .iter()
        .next()
        .is_none());
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_DOOR_SECRET,
                0,
                QuakeMoverState::Bottom,
                QuakeMoverState::Up,
            )
            .sounds,
            [Some(SECRET_STOP_SOUND), Some(SECRET_MOVE_SOUND)]
        );
        assert_eq!(
            mover_sound_events(
                CLASS_FUNC_DOOR_SECRET,
                1,
                QuakeMoverState::Bottom,
                QuakeMoverState::Up,
            )
            .sounds,
            [
                Some(SECRET_MEDIEVAL_LATCH_SOUND),
                Some(SECRET_MEDIEVAL_MOVE_SOUND),
            ]
        );
        assert_eq!(
            secret_tick_sound(1, SECRET_MOVE_SOUND),
            SECRET_MEDIEVAL_MOVE_SOUND
        );
        assert_eq!(
            secret_tick_sound(1, SECRET_STOP_SOUND),
            SECRET_MEDIEVAL_STOP_SOUND
        );
        assert!(mover_sound_events(
            CLASS_FUNC_DOOR_SECRET,
            0,
            QuakeMoverState::Top,
            QuakeMoverState::Down,
        )
        .iter()
        .next()
        .is_none());
    }

    #[test]
    fn a_secret_door_voices_every_original_leg_and_endpoint() {
        let mut mover = QuakeMover::from_entity(
            source(CLASS_FUNC_DOOR_SECRET),
            model(Vec3I16 { x: 16, y: 8, z: 8 }),
        )
        .unwrap()
        .unwrap();
        let mut sounds = [None; 9];
        let mut count = 0usize;
        mover.activate();
        for sound in mover_sound_events(
            CLASS_FUNC_DOOR_SECRET,
            0,
            QuakeMoverState::Bottom,
            mover.state(),
        )
        .iter()
        {
            sounds[count] = Some(sound);
            count += 1;
        }
        for _ in 0..1_000 {
            let tick = mover.tick_with_sound();
            if let Some(sound) = tick.sound {
                sounds[count] = Some(sound);
                count += 1;
            }
            if mover.state() == QuakeMoverState::Bottom {
                break;
            }
        }
        assert_eq!(count, sounds.len());
        assert_eq!(
            sounds,
            [
                Some(SECRET_STOP_SOUND),
                Some(SECRET_MOVE_SOUND),
                Some(SECRET_STOP_SOUND),
                Some(SECRET_MOVE_SOUND),
                Some(SECRET_STOP_SOUND),
                Some(SECRET_MOVE_SOUND),
                Some(SECRET_STOP_SOUND),
                Some(SECRET_MOVE_SOUND),
                Some(SECRET_STOP_SOUND),
            ]
        );
    }

    #[test]
    fn door_uses_shared_integer_progress_and_reverses_without_drift() {
        let mut source = source(CLASS_FUNC_DOOR);
        source.speed = 240;
        let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 16, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        mover.activate();
        assert!(mover.tick());
        let midpoint = mover.transform().origin;
        assert_eq!(midpoint.x, 4 << 12);
        mover.state = QuakeMoverState::Down;
        mover.driver.set_open(false);
        assert!(mover.tick());
        assert_eq!(mover.transform().origin.x, 0);
        mover.activate();
        assert!(mover.tick());
        assert_eq!(mover.transform().origin, midpoint);
    }

    #[test]
    fn zero_travel_button_keeps_its_brush_fixed_but_reaches_top() {
        let mut source = source(CLASS_FUNC_BUTTON);
        source.health = 1;
        source.wait = -4096;
        // E1M4 #238/#239 are four units thick and use the default lip of
        // four, so Quake computes pos1 == pos2 but still installs
        // button_killed and fires their targets.
        let model = BrushModel {
            mins: Vec3I16 {
                x: -73,
                y: 1617,
                z: 1233,
            },
            maxs: Vec3I16 {
                x: -69,
                y: 1663,
                z: 1279,
            },
            ..BrushModel::default()
        };
        let mut mover = QuakeMover::from_entity(source, model).unwrap().unwrap();
        let fixed = mover.transform();
        mover.activate();
        for _ in 0..4 {
            mover.tick();
            assert_eq!(mover.transform(), fixed);
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
    }

    #[test]
    fn a_start_open_door_spawns_displaced_and_its_travel_restores_the_authored_brush() {
        // E1M7's lava bridge in miniature: a door whose authored volume is the
        // floor, which therefore must not be there until it is triggered.
        let mut source = source(CLASS_FUNC_DOOR);
        source.spawn_flags = crate::door::DOOR_START_OPEN;
        source.speed = 240;
        let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 16, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        // Lip defaults to eight, so the travel is the sixteen-unit x size less
        // eight, and the spawn position is one whole travel off the brush.
        assert_eq!(mover.transform().origin.x, 8 << 12);
        assert_eq!(mover.state(), QuakeMoverState::Bottom);
        mover.activate();
        while mover.state() != QuakeMoverState::Top {
            assert!(mover.tick(), "a start-open door has to reach its brush");
        }
        assert_eq!(mover.transform().origin.x, 0);
    }

    #[test]
    fn mover_bounds_rebuild_from_fractional_progress_and_keep_occlusion_broadphase_live() {
        let mut source = source(CLASS_FUNC_DOOR);
        source.speed = 100;
        let model = BrushModel {
            mins: Vec3I16 { x: 0, y: -8, z: -8 },
            maxs: Vec3I16 { x: 64, y: 8, z: 8 },
            ..BrushModel::default()
        };
        let mut mover = QuakeMover::from_entity(source, model).unwrap().unwrap();
        mover.activate();

        let mut previous = mover.transform().origin;
        let mut incrementally_shifted_mins = translated_model_bounds(previous, model.mins);
        let mut incrementally_shifted_maxs = translated_model_bounds(previous, model.maxs);
        for _ in 0..17 {
            assert!(mover.tick());
            let current = mover.transform().origin;
            let whole_delta = current.x.saturating_sub(previous.x) >> 12;
            incrementally_shifted_mins[0] = i32::from(incrementally_shifted_mins[0])
                .saturating_add(whole_delta)
                .clamp(i16::MIN as i32, i16::MAX as i32)
                as i16;
            incrementally_shifted_maxs[0] = i32::from(incrementally_shifted_maxs[0])
                .saturating_add(whole_delta)
                .clamp(i16::MIN as i32, i16::MAX as i32)
                as i16;
            previous = current;
        }

        let origin = mover.transform().origin;
        let mins = translated_model_bounds(origin, model.mins);
        let maxs = translated_model_bounds(origin, model.maxs);
        assert_eq!(mins[0], (origin.x >> 12) as i16);
        assert_ne!(mins[0], incrementally_shifted_mins[0]);

        // Aim through the portion of the live door omitted by the drifted
        // incremental cull. The canonical absolute bounds must retain it.
        let x = i32::from(incrementally_shifted_maxs[0]) + 1;
        let start = Vec3I32 {
            x: x << 12,
            y: -32 << 12,
            z: 0,
        };
        let end = Vec3I32 {
            x: x << 12,
            y: 32 << 12,
            z: 0,
        };
        assert!(segment_overlaps_i16_bounds(start, end, mins, maxs));
        assert!(!segment_overlaps_i16_bounds(
            start,
            end,
            incrementally_shifted_mins,
            incrementally_shifted_maxs,
        ));
    }

    #[test]
    fn vertical_platform_uses_exact_quake_z_up_endpoints() {
        let mut source = source(CLASS_FUNC_PLAT);
        source.height = 16;
        source.target_name = 1;
        let mut mover = QuakeMover::from_entity(
            source,
            model(Vec3I16 {
                x: 64,
                y: 64,
                z: 32,
            }),
        )
        .unwrap()
        .unwrap();
        mover.activate();
        for _ in 0..7 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
        assert_eq!(mover.transform().origin.z, -16 << 12);
    }

    #[test]
    fn a_targeted_platform_waits_low_until_its_inside_trigger_is_touched() {
        let mut source = source(CLASS_FUNC_PLAT);
        source.height = 16;
        source.target_name = 88;
        let mut mover = QuakeMover::from_entity(
            source,
            model(Vec3I16 {
                x: 64,
                y: 64,
                z: 32,
            }),
        )
        .unwrap()
        .unwrap();

        // `plat_use` fires through the target graph and lowers the named
        // platform to its physical low stop (the driver's open/Top endpoint).
        mover.activate();
        for _ in 0..7 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
        assert_eq!(mover.transform().origin.z, -16 << 12);

        // The old generic mover timer returned after 120 ticks, making E1M3's
        // underwater approach impossible. The original waits indefinitely.
        for _ in 0..600 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
        assert_eq!(mover.transform().origin.z, -16 << 12);

        assert!(mover.plat_center_touch());
        assert_eq!(mover.state(), QuakeMoverState::Down);
        for _ in 0..7 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Bottom);
        assert_eq!(mover.transform().origin.z, 0);
    }

    #[test]
    fn automatic_and_toggle_flags_remain_quake_policy() {
        let automatic = QuakeMover::from_entity(
            source(CLASS_FUNC_DOOR),
            model(Vec3I16 { x: 32, y: 8, z: 8 }),
        )
        .unwrap()
        .unwrap();
        assert!(automatic.automatic());
        assert!(!automatic.toggle());

        let mut toggle_source = source(CLASS_FUNC_DOOR);
        toggle_source.spawn_flags = 32;
        let toggle = QuakeMover::from_entity(toggle_source, model(Vec3I16 { x: 32, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        assert!(toggle.automatic());
        assert!(toggle.toggle());
    }

    #[test]
    fn a_toggle_door_is_sent_back_down_when_fired_while_up_or_at_top() {
        let mut source = source(CLASS_FUNC_DOOR);
        source.spawn_flags = crate::door::DOOR_TOGGLE;
        source.speed = 240;
        let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 16, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        mover.activate();
        while mover.state() != QuakeMoverState::Top {
            mover.tick();
        }
        for _ in 0..600 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top, "no automatic return");
        assert!(mover.admits_activation(CLASS_FUNC_DOOR));
        mover.activate();
        assert_eq!(mover.state(), QuakeMoverState::Down);
        while mover.state() != QuakeMoverState::Bottom {
            mover.tick();
        }
        assert_eq!(mover.transform().origin.x, 0);
        // `door_fire` also reverses a toggle door that is still on its way up.
        mover.activate();
        assert!(mover.tick());
        assert_eq!(mover.state(), QuakeMoverState::Up);
        mover.activate();
        assert_eq!(mover.state(), QuakeMoverState::Down);
    }

    #[test]
    fn secret_door_spawnflags_are_not_func_door_spawnflags() {
        // Bit 1 is SECRET_OPEN_ONCE, not DOOR_START_OPEN: the door spawns in
        // its authored brush and never returns once opened. Bit 32 is unused
        // by func_door_secret and must not make it a toggle door.
        let mut source = source(CLASS_FUNC_DOOR_SECRET);
        source.spawn_flags = crate::door::SECRET_OPEN_ONCE | crate::door::DOOR_TOGGLE;
        source.speed = 240;
        let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 16, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        assert_eq!(mover.transform().origin.x, 0);
        assert!(!mover.toggle());
        assert!(!mover.automatic());
        mover.activate();
        for _ in 0..600 {
            mover.tick();
        }
        // `fd_secret_move3` never schedules the return of an OPEN_ONCE door,
        // so it holds at the far end of both legs forever.
        assert_eq!(mover.state(), QuakeMoverState::Top);
        assert_eq!(mover.transform().origin.x, 16 << 12);
        assert_eq!(mover.transform().origin.y, -8 << 12);

        // Without the flag a secret door returns after its five-second wait,
        // and it always travels at the forced 50 units per second.
        let mut returning = self::source(CLASS_FUNC_DOOR_SECRET);
        returning.speed = 240;
        let mut mover = QuakeMover::from_entity(returning, model(Vec3I16 { x: 16, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        mover.activate();
        let mut ticks = 0;
        while mover.state() != QuakeMoverState::Top {
            mover.tick();
            ticks += 1;
        }
        // Eight units sideways then sixteen forward at fifty units per second
        // is 9.6 and 19.2 ticks, each rounded up, with the one-second pause of
        // `fd_secret_move1` between them.
        assert_eq!(ticks, 10 + SECRET_PAUSE_TICKS + 20);
        for _ in 0..(300 + ticks + 1) {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Bottom);
    }

    /// The whole authored shape of `fd_secret_use`: the door steps sideways
    /// by its own width, stands still for a second, and only then slides
    /// forward by its own length.
    #[test]
    fn a_secret_door_walks_its_two_legs_in_order_with_the_pause_between_them() {
        let mut mover = QuakeMover::from_entity(
            source(CLASS_FUNC_DOOR_SECRET),
            model(Vec3I16 { x: 16, y: 8, z: 8 }),
        )
        .unwrap()
        .unwrap();
        mover.activate();

        // Leg one: eight units of width along -Y, and nothing along forward.
        for _ in 0..10 {
            assert!(mover.tick(), "the sideways leg has to move every tick");
        }
        let after_first = mover.transform().origin;
        assert_eq!(after_first.x, 0, "the forward leg has not started yet");
        assert_eq!(after_first.y, -8 << 12);
        assert_eq!(mover.state(), QuakeMoverState::Up);

        // `fd_secret_move1` holds the door still for exactly one second.
        for _ in 0..SECRET_PAUSE_TICKS {
            assert!(!mover.tick(), "the door pauses between its legs");
            assert_eq!(mover.transform().origin, after_first);
        }

        // Leg two: sixteen units forward along +X, width untouched.
        for _ in 0..20 {
            assert!(mover.tick(), "the forward leg has to move every tick");
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
        assert_eq!(mover.transform().origin.x, 16 << 12);
        assert_eq!(mover.transform().origin.y, -8 << 12);
    }

    #[test]
    fn first_left_and_first_down_choose_the_authored_first_leg() {
        let brush = model(Vec3I16 { x: 16, y: 8, z: 8 });
        // The default sideways leg is `v_right`, which for yaw zero is -Y.
        let mut left = source(CLASS_FUNC_DOOR_SECRET);
        left.spawn_flags = crate::door::SECRET_1ST_LEFT;
        let mut down = source(CLASS_FUNC_DOOR_SECRET);
        down.spawn_flags = crate::door::SECRET_1ST_DOWN;

        for (source, expected) in [
            (
                source(CLASS_FUNC_DOOR_SECRET),
                Vec3I32 { x: 0, y: -8, z: 0 },
            ),
            (left, Vec3I32 { x: 0, y: 8, z: 0 }),
            // `t_width` becomes `fabs(v_up * size)` and `dest1` subtracts it,
            // so the door drops by its own eight-unit height.
            (down, Vec3I32 { x: 0, y: 0, z: -8 }),
        ] {
            let mut mover = QuakeMover::from_entity(source, brush).unwrap().unwrap();
            mover.activate();
            for _ in 0..10 {
                mover.tick();
            }
            let origin = mover.transform().origin;
            assert_eq!(origin.x, expected.x << 12);
            assert_eq!(origin.y, expected.y << 12);
            assert_eq!(origin.z, expected.z << 12);
            assert_eq!(mover.state(), QuakeMoverState::Up, "the pause is next");
        }
    }

    #[test]
    fn a_secret_door_closes_by_reversing_both_legs_in_turn() {
        let mut mover = QuakeMover::from_entity(
            source(CLASS_FUNC_DOOR_SECRET),
            model(Vec3I16 { x: 16, y: 8, z: 8 }),
        )
        .unwrap()
        .unwrap();
        mover.activate();
        while mover.state() != QuakeMoverState::Top {
            mover.tick();
        }
        // `fd_secret_move4` waits the authored five seconds first.
        for _ in 0..299 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);

        // `fd_secret_move4` retraces the forward leg back to `dest1`.
        for _ in 0..20 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Down);
        let back_at_dest1 = mover.transform().origin;
        assert_eq!(back_at_dest1.x, 0);
        assert_eq!(back_at_dest1.y, -8 << 12);

        // `fd_secret_move5` pauses another whole second before the last leg.
        for _ in 0..SECRET_PAUSE_TICKS {
            assert!(!mover.tick());
            assert_eq!(mover.transform().origin, back_at_dest1);
        }

        // `fd_secret_move6` retraces the sideways leg back to `oldorigin`.
        for _ in 0..10 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Bottom);
        assert_eq!(mover.transform().origin, Vec3I32 { x: 0, y: 0, z: 0 });

        // And the door is armed for a second use, legs in the same order.
        assert!(mover_state_admits_activation(
            CLASS_FUNC_DOOR_SECRET,
            mover.state()
        ));
        mover.activate();
        for _ in 0..10 {
            mover.tick();
        }
        assert_eq!(mover.transform().origin.y, -8 << 12);
    }

    #[test]
    fn a_secret_door_ignores_every_use_until_its_sequence_has_run() {
        // `fd_secret_use` is reinstalled only by `fd_secret_done`, so unlike a
        // `func_door` a closing secret door does not reverse back open.
        assert!(!mover_state_admits_activation(
            CLASS_FUNC_DOOR_SECRET,
            QuakeMoverState::Down
        ));
        let mut mover = QuakeMover::from_entity(
            source(CLASS_FUNC_DOOR_SECRET),
            model(Vec3I16 { x: 16, y: 8, z: 8 }),
        )
        .unwrap()
        .unwrap();
        mover.activate();
        while mover.state() != QuakeMoverState::Top {
            mover.tick();
        }
        for _ in 0..(300 + 20) {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Down);
        mover.activate();
        assert_eq!(mover.state(), QuakeMoverState::Down);
        for _ in 0..(SECRET_PAUSE_TICKS + 10) {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Bottom);
    }

    #[test]
    fn a_platform_waits_three_seconds_at_the_top_before_returning() {
        let mut mover = QuakeMover::from_entity(
            source(CLASS_FUNC_PLAT),
            model(Vec3I16 {
                x: 64,
                y: 64,
                z: 32,
            }),
        )
        .unwrap()
        .unwrap();
        mover.activate();
        while mover.state() != QuakeMoverState::Top {
            mover.tick();
        }
        for _ in 0..179 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
        mover.tick();
        assert_eq!(mover.state(), QuakeMoverState::Down);
    }

    #[test]
    fn a_key_door_stays_open_because_func_door_forces_wait_to_minus_one() {
        let mut source = source(CLASS_FUNC_DOOR);
        source.spawn_flags = crate::door::DOOR_SILVER_KEY;
        source.speed = 600;
        let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 32, y: 8, z: 8 }))
            .unwrap()
            .unwrap();
        // A key door never gets a trigger field, so it cannot auto-open.
        assert!(!mover.automatic());
        mover.activate();
        for _ in 0..600 {
            mover.tick();
        }
        assert_eq!(mover.state(), QuakeMoverState::Top);
    }

    #[test]
    fn negative_wait_keeps_canonical_button_and_door_open() {
        for class_name in [CLASS_FUNC_BUTTON, CLASS_FUNC_DOOR] {
            let mut source = source(class_name);
            source.wait = -1;
            source.speed = 600;
            let mut mover = QuakeMover::from_entity(source, model(Vec3I16 { x: 32, y: 8, z: 8 }))
                .unwrap()
                .unwrap();
            mover.activate();
            for _ in 0..240 {
                mover.tick();
            }
            assert_eq!(mover.state(), QuakeMoverState::Top);
        }
    }

    #[test]
    fn fixed_wait_conversion_uses_guest_safe_exact_q12_parts() {
        assert_eq!(fixed_seconds_to_ticks(i32::MIN, 17), 17);
        assert_eq!(fixed_seconds_to_ticks(0, 17), 17);
        assert_eq!(fixed_seconds_to_ticks(1, 17), 1);
        assert_eq!(fixed_seconds_to_ticks(4095, 17), 59);
        assert_eq!(fixed_seconds_to_ticks(4096, 17), 60);
        assert_eq!(fixed_seconds_to_ticks(4097, 17), 60);
        assert_eq!(fixed_seconds_to_ticks(i32::MAX, 17), u16::MAX);
    }
    /// `plat_spawn_inside_trigger`'s `PLAT_LOW_TRIGGER` branch. The three
    /// authored lifts that carry the flag would otherwise be sent away again
    /// by the rider they just delivered, because the ordinary volume reaches
    /// eight units above the raised deck.
    #[test]
    fn a_low_trigger_plat_is_not_retriggered_from_its_raised_deck() {
        const MINS: [i16; 3] = [0, 0, 0];
        const MAXS: [i16; 3] = [64, 64, 32];
        // A player standing on the raised deck: feet on top of it, and the
        // 56 unit Quake hull above.
        const RIDER_FEET: i32 = 32;
        let deck = model(Vec3I16 {
            x: MAXS[0],
            y: MAXS[1],
            z: MAXS[2],
        });
        let mut authored = source(CLASS_FUNC_PLAT);
        authored.height = 16;

        let whole_shaft = QuakeMover::from_entity(authored, deck).unwrap().unwrap();
        let mut low = authored;
        low.spawn_flags = PLAT_LOW_TRIGGER;
        let low = QuakeMover::from_entity(low, deck).unwrap().unwrap();

        let (mut mins, mut maxs) = ([0i32; 3], [0i32; 3]);
        assert!(whole_shaft.plat_trigger_volume(MINS, MAXS, &mut mins, &mut maxs));
        // `tmax_z = maxs_z + 8`, `tmin_z = tmax_z - (travel + 8)`.
        assert_eq!((mins[2], maxs[2]), (16, 40));
        assert!(
            RIDER_FEET <= maxs[2],
            "the ordinary volume is what the flag exists to shrink"
        );

        assert!(low.plat_trigger_volume(MINS, MAXS, &mut mins, &mut maxs));
        // The 25 unit horizontal inset is untouched by the flag.
        assert_eq!((mins[0], maxs[0]), (25, 39));
        assert_eq!((mins[1], maxs[1]), (25, 39));
        // `tmax_z = tmin_z + 8`: an eight unit slab at the low stop only.
        assert_eq!((mins[2], maxs[2]), (16, 24));
        assert!(
            RIDER_FEET > maxs[2],
            "a rider on the deck is outside the low trigger"
        );
    }

    /// `changelevel_touch`: `if (self.spawnflags & 1) { GotoNextMap(); }`.
    /// Five authored `trigger_changelevel` volumes carry the bit; the one the
    /// shareware runtime can reach is Start's own door into E1M1, which should
    /// hand the player straight to the map with no panel in between.
    #[test]
    fn a_no_intermission_changelevel_reports_the_skip() {
        use crate::targets::CHANGELEVEL_NO_INTERMISSION;
        assert_eq!(CHANGELEVEL_NO_INTERMISSION, 1);
        assert!(1 & CHANGELEVEL_NO_INTERMISSION != 0);
        // Every other authored changelevel keeps the panel.
        assert!(0 & CHANGELEVEL_NO_INTERMISSION == 0);
    }

    /// `door_blocked`: `T_Damage(other, self.dmg)` and then, for a door that
    /// still means to close, the same turn-around `plat_crush` performs.
    #[test]
    fn a_blocked_door_reverses_and_a_blocked_plat_still_crushes() {
        let panel = model(Vec3I16 { x: 64, y: 8, z: 8 });
        let mut door = QuakeMover::from_entity(source(CLASS_FUNC_DOOR), panel)
            .unwrap()
            .unwrap();
        door.activate();
        for _ in 0..10 {
            door.tick();
        }
        assert_eq!(door.state(), QuakeMoverState::Up);
        assert!(door.crush_reverse());
        assert_eq!(door.state(), QuakeMoverState::Down);
        // And on the way back down it turns around again, exactly as
        // `door_blocked`'s `else door_go_up()` leg does.
        door.tick();
        assert!(door.crush_reverse());
        assert_eq!(door.state(), QuakeMoverState::Up);

        // `if (self.wait >= 0)` guards the whole reversal, so a door authored
        // to stay open keeps the damage-only behaviour.
        let mut held = source(CLASS_FUNC_DOOR);
        held.wait = -1;
        let mut held = QuakeMover::from_entity(held, panel).unwrap().unwrap();
        held.activate();
        held.tick();
        assert!(!held.crush_reverse());
        assert_eq!(held.state(), QuakeMoverState::Up);

        // A key door is `wait = -1` too, and `fd_secret_blocked` has no
        // reversal leg at all.
        let mut key = source(CLASS_FUNC_DOOR);
        key.spawn_flags = crate::door::DOOR_SILVER_KEY;
        let mut key = QuakeMover::from_entity(key, panel).unwrap().unwrap();
        key.activate();
        key.tick();
        assert!(!key.crush_reverse());
        let mut secret = QuakeMover::from_entity(source(CLASS_FUNC_DOOR_SECRET), panel)
            .unwrap()
            .unwrap();
        secret.activate();
        secret.tick();
        assert!(!secret.crush_reverse());

        // The lift is untouched: it reverses from either direction, and the
        // one point of `plat_crush` damage is picked from the class, not from
        // this return.
        let mut plat = source(CLASS_FUNC_PLAT);
        plat.height = 16;
        let mut plat = QuakeMover::from_entity(
            plat,
            model(Vec3I16 {
                x: 64,
                y: 64,
                z: 32,
            }),
        )
        .unwrap()
        .unwrap();
        plat.activate();
        plat.tick();
        assert_eq!(plat.state(), QuakeMoverState::Up);
        assert!(plat.crush_reverse());
        assert_eq!(plat.state(), QuakeMoverState::Down);
        assert!(plat.crush_reverse());
        assert_eq!(plat.state(), QuakeMoverState::Up);
    }
}

/// `func_button`'s spawn is an either/or, straight off `func_button`'s own
/// spawn function: with `health` it gets `th_die = button_killed` and
/// `takedamage = DAMAGE_YES` and NO touch function, so it is shot open; with
/// no health it gets `button_touch` and is opened by walking into it or
/// using it. Four shareware buttons are authored shootable, all at health 1:
/// E1M2's, E1M3's, and E1M4's pair.
pub const fn button_is_shootable(class_name: u8, health: i16) -> bool {
    class_name == CLASS_FUNC_BUTTON && health > 0
}

/// Whether ordinary touch or use opens this button. The complement of
/// [`button_is_shootable`] for a button, and false for anything else.
pub const fn button_admits_touch(class_name: u8, health: i16) -> bool {
    class_name == CLASS_FUNC_BUTTON && health <= 0
}

/// Whether the player's USE key can activate this mover directly.
///
/// `func_button` has no use function in the original at all: a plain one is
/// opened by `button_touch`, and one with health only by `button_killed`. A
/// door or lift with no `targetname` is the mover a player can walk into and
/// use, which is what the `target_name == 0` arm carries.
///
/// The shootable arm matters because every authored shareware shootable
/// button is UNNAMED: without this the `target_name == 0` arm reached them
/// and USE opened all four, which made their damage path decoration.
pub const fn mover_admits_use(class_name: u8, health: i16, target_name: u16) -> bool {
    if button_is_shootable(class_name, health) {
        return false;
    }
    button_admits_touch(class_name, health) || target_name == 0
}

/// A shootable `func_button`'s damage state.
///
/// `button_killed` sets `self.health = self.max_health` before it fires, so a
/// button that returns can be shot again. It also clears `takedamage` until
/// the return, which is what [`Self::is_live`] reports.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShootableButton {
    health: i16,
    max_health: i16,
    killed: bool,
}

impl ShootableButton {
    /// `None` for a button the map did not author shootable.
    pub const fn from_entity(class_name: u8, health: i16) -> Option<Self> {
        if !button_is_shootable(class_name, health) {
            return None;
        }
        Some(Self {
            health,
            max_health: health,
            killed: false,
        })
    }

    pub const fn health(&self) -> i16 {
        self.health
    }

    /// Whether the button still accepts damage this frame.
    pub const fn is_live(&self) -> bool {
        !self.killed
    }

    /// `T_Damage`. Returns true when this hit ran `button_killed`, which is
    /// the frame the button fires its targets.
    pub fn take_damage(&mut self, damage: i16) -> bool {
        if self.killed || damage <= 0 {
            return false;
        }
        self.health = self.health.saturating_sub(damage);
        if self.health > 0 {
            return false;
        }
        // `button_killed` hands the authored health straight back.
        self.health = self.max_health;
        self.killed = true;
        true
    }

    /// The button has finished its trip and takes damage again.
    pub fn rearm(&mut self) {
        self.killed = false;
    }
}

#[cfg(test)]
mod button_tests {
    use super::*;

    const PLAT: u8 = CLASS_FUNC_PLAT;

    #[test]
    fn a_button_with_health_is_shot_and_never_touched() {
        assert!(button_is_shootable(CLASS_FUNC_BUTTON, 1));
        assert!(!button_admits_touch(CLASS_FUNC_BUTTON, 1));
        assert!(ShootableButton::from_entity(CLASS_FUNC_BUTTON, 1).is_some());
    }

    #[test]
    fn a_button_without_health_is_touched_and_never_shot() {
        assert!(!button_is_shootable(CLASS_FUNC_BUTTON, 0));
        assert!(button_admits_touch(CLASS_FUNC_BUTTON, 0));
        assert!(ShootableButton::from_entity(CLASS_FUNC_BUTTON, 0).is_none());
        // A negative health key is not a shootable button either.
        assert!(button_admits_touch(CLASS_FUNC_BUTTON, -5));
    }

    #[test]
    fn no_other_mover_class_is_shootable_or_button_touched() {
        for class in [PLAT, CLASS_FUNC_DOOR, CLASS_FUNC_DOOR_SECRET] {
            assert!(!button_is_shootable(class, 1));
            assert!(!button_admits_touch(class, 0));
            assert!(ShootableButton::from_entity(class, 1).is_none());
        }
    }

    #[test]
    fn use_opens_a_plain_button_and_an_unnamed_mover_but_never_a_shootable_one() {
        // A plain button: touch and use both work, named or not.
        assert!(mover_admits_use(CLASS_FUNC_BUTTON, 0, 0));
        assert!(mover_admits_use(CLASS_FUNC_BUTTON, 0, 42));
        // An unnamed door or lift is the mover a player walks into and uses.
        assert!(mover_admits_use(PLAT, 0, 0));
        assert!(mover_admits_use(CLASS_FUNC_DOOR, 0, 0));
        // A named one waits for its trigger instead.
        assert!(!mover_admits_use(PLAT, 0, 7));
        assert!(!mover_admits_use(CLASS_FUNC_DOOR, 0, 7));
        // A shootable button refuses USE even though it is unnamed, which is
        // exactly the case every authored shareware one falls into.
        assert!(!mover_admits_use(CLASS_FUNC_BUTTON, 1, 0));
        assert!(!mover_admits_use(CLASS_FUNC_BUTTON, 1, 42));
    }

    #[test]
    fn damage_kills_a_button_and_button_killed_hands_its_health_back() {
        let mut button = ShootableButton::from_entity(CLASS_FUNC_BUTTON, 1).expect("shootable");
        assert!(button.is_live());
        assert!(!button.take_damage(0), "a zero-damage hit does nothing");
        assert!(!button.take_damage(-4), "so does a negative one");
        assert!(
            button.take_damage(1),
            "one point kills an authored health 1"
        );
        assert_eq!(button.health(), 1, "button_killed restores max_health");
        assert!(!button.is_live(), "and clears takedamage until it returns");
        assert!(
            !button.take_damage(50),
            "a killed button cannot be killed again on its way up"
        );
        button.rearm();
        assert!(button.is_live());
        assert!(
            button.take_damage(1),
            "a returned button is shootable again"
        );
    }

    #[test]
    fn a_tougher_button_needs_its_whole_health() {
        let mut button = ShootableButton::from_entity(CLASS_FUNC_BUTTON, 40).expect("shootable");
        assert!(!button.take_damage(15));
        assert_eq!(button.health(), 25);
        assert!(!button.take_damage(24));
        assert_eq!(button.health(), 1);
        assert!(button.take_damage(1));
        assert_eq!(button.health(), 40);
    }
}
