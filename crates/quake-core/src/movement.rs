//! Fixed-point Quake player locomotion over an abstract collision provider.
//!
//! PSoXide supplies fixed-point primitives and the shared BSP implementation.
//! This module owns only game policy: Quake acceleration, friction, gravity,
//! jumping, water levels, stair stepping, and multi-plane velocity clipping.

use psx_engine::div_q12_i32;
use psx_math::int32::{abs_i32, isqrt_i32, mul_q12_i32, square_i32_saturating};
use quake_formats::{Vec3I16, Vec3I32};

use crate::collision::{CollisionHull, Trace, TraceScratch};
pub use crate::collision::{
    CONTENTS_EMPTY, CONTENTS_LAVA, CONTENTS_SLIME, CONTENTS_WATER, Q12_ONE,
};

const VIDEO_TICKS_PER_SECOND: i32 = 60;
const MAX_CATCHUP_TICKS: u16 = 4;
const MAX_SPEED_Q12: i32 = 320 << 12;
const STOP_SPEED_Q12: i32 = 100 << 12;
/// `sv_maxvelocity`: `SV_CheckVelocity` clamps each component to this.
const MAX_VELOCITY_Q12: i32 = 2000 << 12;
/// `CheckWaterJump`: the hop out of water onto a ledge, and its two-second
/// `teleport_time` safety net.
const WATER_JUMP_OUT_SPEED_Q12: i32 = 225 << 12;
const WATER_JUMP_PUSH_Q12: i32 = 50 << 12;
const WATER_JUMP_TICKS: u8 = 120;
const WATER_JUMP_PROBE_Q12: i32 = 24 << 12;
/// Quake's normal `sv_gravity`; E1M8 overrides it to 100 in the original
/// `worldspawn` program.
pub const DEFAULT_GRAVITY: u16 = 800;
const JUMP_SPEED_Q12: i32 = 270 << 12;
const WATER_JUMP_SPEED_Q12: i32 = 100 << 12;
const SLIME_JUMP_SPEED_Q12: i32 = 80 << 12;
const LAVA_JUMP_SPEED_Q12: i32 = 50 << 12;
const WATER_DRIFT_SPEED_Q12: i32 = 60 << 12;
const STEP_HEIGHT_Q12: i32 = 18 << 12;
const GROUND_PROBE_Q12: i32 = 2 << 12;
const WALKABLE_NORMAL_Q12: i32 = 2_867;
const LAND_SPEED_Q12: i32 = -(300 << 12);
const HARD_LAND_SPEED_Q12: i32 = -(650 << 12);
/// `WaterMove`'s `velocity -= 0.8 * waterlevel * frametime * velocity` at 60
/// ticks a second: one 75th of the velocity per water level.
const WATER_DRAG_DIVISOR: i32 = 75;
/// `teleport_time = time + 0.7`.
const TELEPORT_BACKPEDAL_TICKS: u8 = 42;
/// `PlayerJump` plays a swim stroke at most once a second.
const SWIM_STROKE_TICKS: u8 = 60;
/// `SV_UserFriction`'s edge probe: 16 units ahead of the origin, from the
/// feet down 34 units.
const EDGE_FRICTION_REACH_Q12: i32 = 16 << 12;
const EDGE_FRICTION_DROP_Q12: i32 = 34 << 12;
const FEET_BELOW_ORIGIN_Q12: i32 = 24 << 12;
/// `STOP_EPSILON` (0.1) of `ClipVelocity`.
const STOP_EPSILON_Q12: i32 = 410;
const MAX_SLIDE_BUMPS: usize = 4;
const MAX_CLIP_PLANES: usize = 5;

/// Per-tick abstract Quake movement input.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MovementInput {
    /// Forward/back intent in `-127..=127`.
    pub forward: i16,
    /// Right/left strafe intent in `-127..=127`.
    pub strafe: i16,
    /// World yaw in Quake's 4096-unit turn.
    pub yaw: u16,
    /// View pitch in Quake's 4096-unit turn, positive looking down. Only
    /// swimming reads it: `SV_WaterMove` builds the wish direction from the
    /// full view angles, so looking up and moving forward swims upward.
    pub pitch: u16,
    /// True while the jump/swim button is held.
    pub jump: bool,
}

/// Event bits raised while advancing one or more movement ticks.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MovementEvents(u16);

impl MovementEvents {
    pub const JUMP: u16 = 1 << 0;
    pub const SWIM: u16 = 1 << 1;
    pub const LAND: u16 = 1 << 2;
    pub const HARD_LAND: u16 = 1 << 3;
    pub const WATER_LAND: u16 = 1 << 4;
    pub const ENTER_LIQUID: u16 = 1 << 5;
    pub const LEAVE_LIQUID: u16 = 1 << 6;

    pub const fn contains(self, event: u16) -> bool {
        self.0 & event != 0
    }

    fn insert(&mut self, event: u16) {
        self.0 |= event;
    }

    fn merge(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// Collision queries the motor could not resolve while advancing a frame.
///
/// The original engine cannot fail these: `SV_PointContents` always answers
/// and `SV_RecursiveHullCheck` always produces a trace, with malformed BSP
/// data being a `Host_Error` rather than a return value. This port's providers
/// can fail, so the motor records what it had to assume and keeps running.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MovementStalls(u8);

impl MovementStalls {
    /// A hull trace reported malformed data or exhausted its scratch, so the
    /// motor treated that segment as blocked where it started.
    pub const TRACE: u8 = 1 << 0;
    /// A `point_contents` sample did not resolve to a leaf, so the motor
    /// treated that sample as open air.
    pub const CONTENTS: u8 = 1 << 1;
    /// The caller had no collision provider, so the motor never ran. Raised by
    /// the game layer rather than by this module.
    pub const NO_COLLISION: u8 = 1 << 2;

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, stall: u8) -> bool {
        self.0 & stall != 0
    }

    fn insert(&mut self, stall: u8) {
        self.0 |= stall;
    }
}

/// Aggregate result from advancing the player motor.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MovementFrame {
    pub moved: bool,
    pub events: MovementEvents,
    pub water_level: u8,
    pub water_type: i16,
    /// Queries the motor had to assume this frame. Empty on a healthy frame.
    pub stalls: MovementStalls,
}

/// Reusable caller-owned workspace for collision traces performed by the
/// player motor.
pub struct MovementScratch {
    bsp_trace: TraceScratch,
    trace: MovementTraceResult,
    stalls: MovementStalls,
}

impl MovementScratch {
    pub const fn new() -> Self {
        Self {
            bsp_trace: TraceScratch::new(),
            trace: MovementTraceResult::unobstructed(Vec3I32 { x: 0, y: 0, z: 0 }),
            stalls: MovementStalls(0),
        }
    }

    /// Queries the motor had to assume during the most recent
    /// [`MovementState::update_ticks`].
    pub const fn stalls(&self) -> MovementStalls {
        self.stalls
    }
}

impl Default for MovementScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Provider-neutral result needed by Quake's movement policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MovementTraceResult {
    pub all_solid: bool,
    pub start_solid: bool,
    pub fraction: i32,
    pub end: Vec3I32,
    pub normal: Vec3I16,
    /// Caller-owned body index when a dynamic body supplied the winning hit.
    /// World and brush-model contacts leave this unset.
    pub blocking_body: Option<u16>,
}

impl Default for MovementTraceResult {
    fn default() -> Self {
        Self::unobstructed(Vec3I32 { x: 0, y: 0, z: 0 })
    }
}

impl MovementTraceResult {
    pub const fn unobstructed(end: Vec3I32) -> Self {
        Self {
            all_solid: false,
            start_solid: false,
            fraction: Q12_ONE,
            end,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            blocking_body: None,
        }
    }

    /// Quake's "entity is trapped in another solid" case, the one `SV_FlyMove`
    /// answers by zeroing the velocity.
    ///
    /// `SV_FlyMove` tests `trace.allsolid` alone and so does this. The flag is
    /// only worth that trust because [`Self::restore_trace_invariants`] has
    /// already run: see there for the contradiction a provider can report and
    /// the original tracer cannot.
    pub const fn trapped(&self) -> bool {
        self.all_solid
    }

    /// Re-impose the invariants `SV_RecursiveHullCheck` cannot break.
    ///
    /// The original starts a trace with `allsolid` true and clears it the first
    /// time the walk reaches a non-solid leaf. If it is still set when the near
    /// side resolves, the recursion returns on `if (trace->allsolid) return
    /// false` before it can record either an impact plane or a fraction, so an
    /// `allsolid` trace always comes back with `fraction` 1 and a zero plane.
    /// A trace carrying a fraction below one, or any plane normal, therefore
    /// cannot also be `allsolid`.
    ///
    /// The providers here are separate implementations that can and do
    /// contradict that. The canonical tracer answers a box resting exactly on a
    /// surface and moving down with a zero-length near side, so the descent
    /// resolves straight into the solid leaf and reports `all_solid` beside
    /// `fraction` 0 and the floor's own normal. That is a contact, not a trap:
    /// it is what froze a player standing on the solid `func_bossgate`. The
    /// impact evidence wins.
    fn restore_trace_invariants(&mut self) {
        let impacted = self.fraction < Q12_ONE
            || self.normal.x != 0
            || self.normal.y != 0
            || self.normal.z != 0;
        if impacted {
            self.all_solid = false;
        }
    }
}

/// Collision source consumed by the Quake motor. The canonical BSP hull
/// implements this directly; game policy may also compose world and translated
/// submodel hits without duplicating movement or slide logic.
pub trait MovementTrace {
    fn trace(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool;

    /// A zero-size trace through the world and its brush entities that ignores
    /// monsters: `SV_Move (start, vec3_origin, vec3_origin, end, true, ent)`,
    /// which is what `traceline (.., TRUE, ..)` and the edge-friction probe
    /// ask. A provider with only the player hull answers with that hull.
    fn trace_line(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        self.trace(start, end, scratch, output)
    }
}

impl MovementTrace for CollisionHull<'_> {
    fn trace(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        let mut trace = Trace::default();
        if !self.trace_into(start, end, scratch, &mut trace) {
            return false;
        }
        *output = MovementTraceResult {
            all_solid: trace.all_solid,
            start_solid: trace.start_solid,
            fraction: trace.fraction,
            end: trace.end,
            normal: trace.normal,
            blocking_body: None,
        };
        true
    }
}

/// Persistent Quake locomotion state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MovementState {
    origin: Vec3I32,
    /// Last position the world accepted, matching Quake's `oldorigin` field.
    /// `SV_CheckStuck` falls back to it before trying one-unit hull nudges.
    old_origin: Vec3I32,
    velocity: Vec3I32,
    /// `FL_ONGROUND`. Like the original it is cleared at the start of every
    /// `SV_WalkMove` and raised only by a floor hit on world or brush geometry
    /// during that move, so a box on a monster's head is not grounded.
    grounded: bool,
    water_level: u8,
    water_type: i16,
    /// `FL_JUMPRELEASED`: set whenever the jump button is up, cleared when a
    /// jump (or a water jump) actually fires.
    jump_released: bool,
    /// Ticks until `PlayerJump` may play another swim stroke (`swim_flag`).
    swim_sound_ticks: u8,
    /// `jump_flag`: the vertical speed of the last airborne tick, which
    /// `PlayerPostThink` reads on landing.
    fall_speed: i32,
    /// Ticks left of `teleport_time` after a teleport, during which
    /// `SV_AirMove` ignores backward input.
    teleport_ticks: u8,
    steep_seam_recovered: bool,
    /// `FL_WATERJUMP` ticks remaining; zero when not climbing out of water.
    water_jump_ticks: u8,
    /// `self.movedir` for the climb: the ledge normal times -50, XY only.
    water_jump_push: Vec3I32,
}

impl MovementState {
    pub const fn new(origin: Vec3I32) -> Self {
        Self {
            origin,
            old_origin: origin,
            velocity: Vec3I32 { x: 0, y: 0, z: 0 },
            grounded: false,
            water_level: 0,
            water_type: CONTENTS_EMPTY,
            jump_released: false,
            swim_sound_ticks: 0,
            fall_speed: 0,
            teleport_ticks: 0,
            steep_seam_recovered: false,
            water_jump_ticks: 0,
            water_jump_push: Vec3I32 { x: 0, y: 0, z: 0 },
        }
    }

    pub const fn origin(&self) -> Vec3I32 {
        self.origin
    }

    pub const fn velocity(&self) -> Vec3I32 {
        self.velocity
    }

    pub const fn grounded(&self) -> bool {
        self.grounded
    }

    pub const fn water_level(&self) -> u8 {
        self.water_level
    }

    pub const fn water_type(&self) -> i16 {
        self.water_type
    }

    /// Teleport to a new origin and discard velocity/environment caches.
    pub fn teleport(&mut self, origin: Vec3I32) {
        *self = Self::new(origin);
    }

    /// Place the body at an origin a pusher already traced it to.
    ///
    /// `SV_PushMove` moves a rider by writing the push trace's endpoint into
    /// its origin and nothing else: velocity, water state and the ground flag
    /// belong to the rider's own physics and survive the carry.
    pub fn set_origin(&mut self, origin: Vec3I32) {
        self.origin = origin;
    }

    /// `teleport_touch`'s exit: relocate, clear the ground flag, and leave the
    /// player with the destination's `v_forward * 300` push. `teleport_time`
    /// is armed for 0.7 s so `SV_AirMove` refuses backward input.
    pub fn teleport_with_velocity(&mut self, origin: Vec3I32, velocity: Vec3I32) {
        *self = Self::new(origin);
        self.velocity = velocity;
        self.teleport_ticks = TELEPORT_BACKPEDAL_TICKS;
    }

    /// `T_Damage` knockback: `targ.velocity += dir * damage * 8`. An upward
    /// push takes the player off the ground so `SV_Physics_Client` does not
    /// re-clamp the vertical speed on the next tick.
    pub fn add_velocity(&mut self, impulse: Vec3I32) {
        self.velocity = Vec3I32 {
            x: self.velocity.x.saturating_add(impulse.x),
            y: self.velocity.y.saturating_add(impulse.y),
            z: self.velocity.z.saturating_add(impulse.z),
        };
        if impulse.z > 0 {
            self.grounded = false;
        }
    }

    /// `PlayerDie`: the body becomes a `MOVETYPE_TOSS` corpse that has left the
    /// ground, and `velocity_z += random() * 300` hops it when it was not
    /// already rising at 10 or more.
    pub fn die(&mut self, hop_units: i32) {
        self.grounded = false;
        self.water_jump_ticks = 0;
        if self.velocity.z < 10 << 12 {
            self.velocity.z = self.velocity.z.saturating_add(hop_units << 12);
        }
    }

    /// `SV_Physics_Toss` for the corpse: gravity, one push through the world,
    /// clip against what it hit, and a stop on the first walkable floor. A
    /// body that is already on the ground does not move at all.
    pub fn update_dead<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        elapsed_ticks: u16,
        gravity: u16,
    ) {
        let gravity_per_tick_q12 = (i32::from(gravity) << 12) / VIDEO_TICKS_PER_SECOND;
        scratch.stalls = MovementStalls::default();
        for _ in 0..elapsed_ticks.clamp(1, MAX_CATCHUP_TICKS) {
            if self.grounded {
                break;
            }
            self.velocity.z = self.velocity.z.saturating_sub(gravity_per_tick_q12);
            let step = Vec3I32 {
                x: self.velocity.x / VIDEO_TICKS_PER_SECOND,
                y: self.velocity.y / VIDEO_TICKS_PER_SECOND,
                z: self.velocity.z / VIDEO_TICKS_PER_SECOND,
            };
            push_entity(collision, scratch, &mut self.origin, step);
            if scratch.trace.fraction == Q12_ONE {
                continue;
            }
            self.velocity = clip_velocity(self.velocity, scratch.trace.normal);
            if scratch.trace.normal.z as i32 > WALKABLE_NORMAL_Q12 {
                self.grounded = true;
                self.velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            }
        }
    }

    /// Advance by elapsed 60 Hz video ticks. The small catch-up cap prevents
    /// a long load or debugger stop from turning into one enormous move.
    pub fn update_ticks<C, F>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        input: MovementInput,
        elapsed_ticks: u16,
        point_contents: F,
    ) -> MovementFrame
    where
        C: MovementTrace + ?Sized,
        F: FnMut(&Vec3I32) -> Option<i16>,
    {
        self.update_ticks_with_gravity(
            collision,
            scratch,
            input,
            elapsed_ticks,
            DEFAULT_GRAVITY,
            point_contents,
        )
    }

    /// Advance with the current server gravity in whole Quake units/second².
    ///
    /// The normal game passes [`DEFAULT_GRAVITY`]. E1M8 passes 100, matching
    /// the original shareware `worldspawn` rule that makes its platform jumps
    /// possible. Keeping the policy at the caller prevents a map exception
    /// from leaking into the provider-neutral motor.
    pub fn update_ticks_with_gravity<C, F>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        input: MovementInput,
        elapsed_ticks: u16,
        gravity: u16,
        mut point_contents: F,
    ) -> MovementFrame
    where
        C: MovementTrace + ?Sized,
        F: FnMut(&Vec3I32) -> Option<i16>,
    {
        let start = self.origin;
        let mut events = MovementEvents::default();
        let ticks = elapsed_ticks.clamp(1, MAX_CATCHUP_TICKS);
        let gravity_per_tick_q12 = (i32::from(gravity) << 12) / VIDEO_TICKS_PER_SECOND;
        scratch.stalls = MovementStalls::default();
        for _ in 0..ticks {
            events.merge(self.update_one(
                collision,
                scratch,
                input,
                gravity_per_tick_q12,
                &mut point_contents,
            ));
        }
        MovementFrame {
            moved: self.origin != start,
            events,
            water_level: self.water_level,
            water_type: self.water_type,
            stalls: scratch.stalls,
        }
    }

    /// One server frame of `SV_ClientThink`, `PlayerPreThink`,
    /// `SV_Physics_Client` and the landing half of `PlayerPostThink`, in the
    /// order the original runs them.
    fn update_one<C, F>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        input: MovementInput,
        gravity_per_tick_q12: i32,
        point_contents: &mut F,
    ) -> MovementEvents
    where
        C: MovementTrace + ?Sized,
        F: FnMut(&Vec3I32) -> Option<i16>,
    {
        let mut events = MovementEvents::default();
        self.swim_sound_ticks = self.swim_sound_ticks.saturating_sub(1);

        // `SV_ClientThink` reads `FL_ONGROUND` as the last move left it.
        let on_ground = self.grounded;
        if self.water_jump_ticks != 0 {
            // `SV_WaterJump`: the flag drops on the 2 s timer or on leaving
            // the water; while it is up the climb owns the horizontal
            // velocity and the player's own input is ignored.
            if self.water_level == 0 {
                self.water_jump_ticks = 0;
            } else {
                self.water_jump_ticks -= 1;
            }
            if self.water_jump_ticks != 0 {
                self.velocity.x = self.water_jump_push.x;
                self.velocity.y = self.water_jump_push.y;
            }
        } else if self.water_level >= 2 {
            self.swim_accelerate(input);
        } else {
            self.walk_accelerate(collision, scratch, input, on_ground);
        }

        // `PlayerPreThink`: `WaterMove`'s drag, `CheckWaterJump`, `PlayerJump`.
        if self.water_jump_ticks == 0 {
            let drag = i32::from(self.water_level);
            self.velocity.x -= self.velocity.x * drag / WATER_DRAG_DIVISOR;
            self.velocity.y -= self.velocity.y * drag / WATER_DRAG_DIVISOR;
            self.velocity.z -= self.velocity.z * drag / WATER_DRAG_DIVISOR;
        }
        if self.water_level == 2 && self.check_water_jump(collision, scratch, input.yaw) {
            self.velocity.z = WATER_JUMP_OUT_SPEED_Q12;
            self.jump_released = false;
            self.water_jump_ticks = WATER_JUMP_TICKS;
        }
        if !input.jump {
            self.jump_released = true;
        } else if self.water_jump_ticks == 0 {
            if self.water_level >= 2 {
                self.velocity.z = match self.water_type {
                    CONTENTS_SLIME => SLIME_JUMP_SPEED_Q12,
                    CONTENTS_LAVA => LAVA_JUMP_SPEED_Q12,
                    _ => WATER_JUMP_SPEED_Q12,
                };
                if self.swim_sound_ticks == 0 {
                    self.swim_sound_ticks = SWIM_STROKE_TICKS;
                    events.insert(MovementEvents::SWIM);
                }
            } else if on_ground && self.jump_released {
                self.jump_released = false;
                self.grounded = false;
                self.velocity.z = self.velocity.z.saturating_add(JUMP_SPEED_Q12);
                events.insert(MovementEvents::JUMP);
            }
        }

        // `SV_Physics_Client`.
        self.velocity.x = self.velocity.x.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.velocity.y = self.velocity.y.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.velocity.z = self.velocity.z.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.refresh_water(scratch, point_contents, &mut events);
        if self.water_level < 2 && self.water_jump_ticks == 0 {
            self.velocity.z = self.velocity.z.saturating_sub(gravity_per_tick_q12);
        }
        self.check_stuck(collision, scratch);
        if self.grounded {
            self.steep_seam_recovered = false;
        }
        self.walk_move(collision, scratch, input);
        self.refresh_water(scratch, point_contents, &mut events);

        // `PlayerPostThink`: land on the speed of the last airborne tick.
        if self.grounded && self.fall_speed < LAND_SPEED_Q12 {
            // The splash replaces the landing sound only for
            // `self.watertype == CONTENT_WATER`; slime and lava landings
            // still take the fall.
            if self.water_level > 0 && self.water_type == CONTENTS_WATER {
                events.insert(MovementEvents::WATER_LAND);
            } else if self.fall_speed < HARD_LAND_SPEED_Q12 {
                events.insert(MovementEvents::HARD_LAND);
            } else {
                events.insert(MovementEvents::LAND);
            }
            self.fall_speed = 0;
        }
        if !self.grounded {
            self.fall_speed = self.velocity.z;
        }
        events
    }

    /// `CheckWaterJump`: a point trace 24 units ahead at knee height that hits
    /// a wall, and a second one at chest height that does not, means a ledge
    /// the player can hop onto. Records `movedir` (the wall normal times -50).
    fn check_water_jump<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        yaw: u16,
    ) -> bool {
        let yaw = yaw & 0x0fff;
        let forward = Vec3I32 {
            x: mul_q12_i32(psx_math::cos_q12(yaw), WATER_JUMP_PROBE_Q12),
            y: mul_q12_i32(psx_math::sin_q12(yaw), WATER_JUMP_PROBE_Q12),
            z: 0,
        };
        let mut start = add(self.origin, Vec3I32 { x: 0, y: 0, z: 8 << 12 });
        trace_line(collision, &start, &add(start, forward), scratch);
        if scratch.trace.fraction >= Q12_ONE {
            return false;
        }
        let normal = scratch.trace.normal;
        start.z = self.origin.z.saturating_add(32 << 12);
        trace_line(collision, &start, &add(start, forward), scratch);
        if scratch.trace.fraction < Q12_ONE {
            return false;
        }
        self.water_jump_push = Vec3I32 {
            x: mul_q12_i32(-WATER_JUMP_PUSH_Q12, normal.x as i32),
            y: mul_q12_i32(-WATER_JUMP_PUSH_Q12, normal.y as i32),
            z: 0,
        };
        true
    }

        /// Quake's `SV_CheckStuck`: recover a player whose hull begins inside
    /// solid by first restoring the last valid origin, then by searching the
    /// original one-unit XY / 0..17-unit Z neighborhood. The original labels
    /// this a gameplay-oriented clipping-hull precision hack; keeping it here
    /// prevents a rare seam from becoming a permanent trap.
    fn check_stuck<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
    ) {
        if !position_stuck(collision, self.origin, scratch) {
            self.old_origin = self.origin;
            return;
        }

        let stuck_origin = self.origin;
        if !position_stuck(collision, self.old_origin, scratch) {
            self.origin = self.old_origin;
            return;
        }

        for z in 0..18 {
            for x in -1..=1 {
                for y in -1..=1 {
                    let candidate = Vec3I32 {
                        x: stuck_origin.x.saturating_add(x << 12),
                        y: stuck_origin.y.saturating_add(y << 12),
                        z: stuck_origin.z.saturating_add(z << 12),
                    };
                    if !position_stuck(collision, candidate, scratch) {
                        self.origin = candidate;
                        return;
                    }
                }
            }
        }
        self.origin = stuck_origin;
    }

    fn refresh_water<F>(
        &mut self,
        scratch: &mut MovementScratch,
        point_contents: &mut F,
        events: &mut MovementEvents,
    ) where
        F: FnMut(&Vec3I32) -> Option<i16>,
    {
        let previous_level = self.water_level;
        let previous_type = self.water_type;
        let mut resolved = true;
        // `SV_PointContents` cannot fail in the original: it walks hull zero
        // and always lands in a leaf. A sample this port cannot resolve is
        // treated as the open air that surrounds the player everywhere else,
        // never as a reason to stop moving them.
        let (water_level, water_type) = water_level(&self.origin, &mut |point| {
            point_contents(point).unwrap_or_else(|| {
                resolved = false;
                CONTENTS_EMPTY
            })
        });
        if !resolved {
            scratch.stalls.insert(MovementStalls::CONTENTS);
        }
        self.water_level = water_level;
        self.water_type = water_type;
        if previous_level == 0 && water_level > 0 {
            events.insert(MovementEvents::ENTER_LIQUID);
        } else if previous_level > 0 && water_level == 0 {
            events.insert(MovementEvents::LEAVE_LIQUID);
            self.water_type = CONTENTS_EMPTY;
        } else if previous_type != water_type && water_level > 0 {
            events.insert(MovementEvents::ENTER_LIQUID);
        }
    }

    /// `SV_ClientThink` for a walker: `SV_AirMove`'s wish velocity, then
    /// ground friction and acceleration when `FL_ONGROUND` was up, or the
    /// capped air acceleration when it was not.
    fn walk_accelerate<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        mut input: MovementInput,
        on_ground: bool,
    ) {
        // The hack that keeps a freshly teleported player from walking back
        // into the teleporter.
        if self.teleport_ticks != 0 {
            self.teleport_ticks -= 1;
            input.forward = input.forward.max(0);
        }
        let (direction, speed) = wish_velocity(input);
        if on_ground {
            self.user_friction(collision, scratch);
            let current = dot(self.velocity, direction);
            accelerate(&mut self.velocity, direction, speed, speed, current);
        } else {
            let current = dot(self.velocity, direction);
            accelerate(&mut self.velocity, direction, speed, speed.min(30 << 12), current);
        }
    }

    /// `SV_UserFriction`: ground friction, doubled when the leading edge of
    /// the player has nothing within 34 units below it (`sv_edgefriction`).
    fn user_friction<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
    ) {
        let speed = vector_length(Vec3I32 {
            x: self.velocity.x,
            y: self.velocity.y,
            z: 0,
        });
        if speed < Q12_ONE {
            self.velocity.x = 0;
            self.velocity.y = 0;
            return;
        }
        let ahead = |component: i32| {
            mul_q12_i32(div_q12_i32(component, speed), EDGE_FRICTION_REACH_Q12)
        };
        let start = Vec3I32 {
            x: self.origin.x.saturating_add(ahead(self.velocity.x)),
            y: self.origin.y.saturating_add(ahead(self.velocity.y)),
            z: self.origin.z.saturating_sub(FEET_BELOW_ORIGIN_Q12),
        };
        let stop = Vec3I32 {
            z: start.z.saturating_sub(EDGE_FRICTION_DROP_Q12),
            ..start
        };
        trace_line(collision, &start, &stop, scratch);
        let mut drop = speed.max(STOP_SPEED_Q12) / 15;
        if scratch.trace.fraction == Q12_ONE {
            drop *= 2;
        }
        let scale = div_q12_i32(speed.saturating_sub(drop).max(0), speed);
        self.velocity.x = mul_q12_i32(self.velocity.x, scale);
        self.velocity.y = mul_q12_i32(self.velocity.y, scale);
        self.velocity.z = mul_q12_i32(self.velocity.z, scale);
    }

    /// `SV_WaterMove`: friction on the full speed, then acceleration toward
    /// the view-direction wish at 70 percent, or a slow drift to the bottom
    /// with no input.
    fn swim_accelerate(&mut self, input: MovementInput) {
        let (mut direction, mut speed) = swim_wish_velocity(input);
        if speed == 0 {
            direction = Vec3I32 {
                x: 0,
                y: 0,
                z: -Q12_ONE,
            };
            speed = WATER_DRIFT_SPEED_Q12;
        }
        speed = speed.saturating_mul(7) / 10;
        apply_water_friction(&mut self.velocity);
        let current = vector_length(self.velocity);
        accelerate(&mut self.velocity, direction, speed, speed, current);
    }

    /// `SV_WalkMove`: slide, and when the slide ran into a vertical wall
    /// (`clip & 2`) retry from a step higher, keeping the stepped result only
    /// if the drop back down lands on walkable ground.
    fn walk_move<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        input: MovementInput,
    ) {
        let on_ground = self.grounded;
        self.grounded = false;
        let old_origin = self.origin;
        let old_velocity = self.velocity;
        let slide = fly_move(collision, scratch, &mut self.origin, &mut self.velocity);
        self.grounded |= slide.ground;
        // The captured E1M1 seam leaves the player technically airborne on a
        // steep bevel, so the grounded `SV_TryUnstick` call cannot run. Retry
        // from the last valid pre-impact origin only when the frame made less
        // than 1/8 unit of progress and a short floor probe confirms a steep
        // support plane. That distinguishes the fixed-point seam from an
        // ordinary airborne collision with a vertical wall. Not in id1.
        if !on_ground
            && slide.blocked != 0
            && abs_i32(self.origin.x.saturating_sub(old_origin.x)) < Q12_ONE / 8
            && abs_i32(self.origin.y.saturating_sub(old_origin.y)) < Q12_ONE / 8
            && !self.steep_seam_recovered
            && on_steep_support(collision, old_origin, scratch)
        {
            let mut recovered_origin = old_origin;
            let mut recovered_velocity = old_velocity;
            if try_steep_seam_lift(
                collision,
                scratch,
                &mut recovered_origin,
                &mut recovered_velocity,
                old_velocity,
            ) {
                self.origin = recovered_origin;
                self.velocity = recovered_velocity;
                self.steep_seam_recovered = true;
                return;
            }
        }
        if slide.blocked & 2 == 0 {
            return;
        }
        // Do not stair up while jumping, but swimmers do.
        if !on_ground && self.water_level == 0 {
            return;
        }
        if self.water_jump_ticks != 0 {
            return;
        }
        let no_step_origin = self.origin;
        let no_step_velocity = self.velocity;

        // Move up, then forward on the horizontal part of the old velocity.
        self.origin = old_origin;
        push_entity(
            collision,
            scratch,
            &mut self.origin,
            Vec3I32 {
                x: 0,
                y: 0,
                z: STEP_HEIGHT_Q12,
            },
        );
        self.velocity = Vec3I32 {
            x: old_velocity.x,
            y: old_velocity.y,
            z: 0,
        };
        let step = fly_move(collision, scratch, &mut self.origin, &mut self.velocity);
        self.grounded |= step.ground;
        let mut clip = step.blocked;
        // The limited precision of the clipping hulls can stop a step dead.
        if clip != 0
            && abs_i32(self.origin.x.saturating_sub(old_origin.x)) < Q12_ONE / 32
            && abs_i32(self.origin.y.saturating_sub(old_origin.y)) < Q12_ONE / 32
        {
            clip = try_unstick(
                collision,
                scratch,
                &mut self.origin,
                &mut self.velocity,
                old_velocity,
            );
        }
        if clip & 2 != 0 {
            self.wall_friction(step.wall, input);
        }
        // Move down. Landing on walkable ground keeps the step; anything else
        // (wall and slope corners) falls back to the plain slide. The original
        // never raises `FL_ONGROUND` on this path: its test reads the
        // player's own `solid`, which is never `SOLID_BSP`.
        push_entity(
            collision,
            scratch,
            &mut self.origin,
            Vec3I32 {
                x: 0,
                y: 0,
                z: (old_velocity.z / VIDEO_TICKS_PER_SECOND).saturating_sub(STEP_HEIGHT_Q12),
            },
        );
        if (scratch.trace.normal.z as i32) <= WALKABLE_NORMAL_Q12 {
            self.origin = no_step_origin;
            self.velocity = no_step_velocity;
        }
    }

    /// `SV_WallFriction`: running into a wall you are looking at bleeds the
    /// speed along it, by `1 + (normal . forward + 0.5)` when that is below 1.
    fn wall_friction(&mut self, wall: Vec3I16, input: MovementInput) {
        let yaw = input.yaw & 0x0fff;
        let pitch = input.pitch & 0x0fff;
        let level = psx_math::cos_q12(pitch);
        let forward = Vec3I32 {
            x: mul_q12_i32(level, psx_math::cos_q12(yaw)),
            y: mul_q12_i32(level, psx_math::sin_q12(yaw)),
            z: -psx_math::sin_q12(pitch),
        };
        let d = dot_normal(forward, wall).saturating_add(Q12_ONE / 2);
        if d >= 0 {
            return;
        }
        let into = dot_normal(self.velocity, wall);
        let keep = Q12_ONE + d;
        self.velocity.x = mul_q12_i32(
            self.velocity.x.saturating_sub(mul_q12_i32(into, wall.x as i32)),
            keep,
        );
        self.velocity.y = mul_q12_i32(
            self.velocity.y.saturating_sub(mul_q12_i32(into, wall.y as i32)),
            keep,
        );
    }
}

fn water_level<F>(origin: &Vec3I32, point_contents: &mut F) -> (u8, i16)
where
    F: FnMut(&Vec3I32) -> i16,
{
    let mut point = Vec3I32 {
        x: origin.x,
        y: origin.y,
        z: origin.z.saturating_sub(23 << 12),
    };
    let contents = point_contents(&point);
    if contents > CONTENTS_WATER {
        return (0, CONTENTS_EMPTY);
    }
    let water_type = contents;
    point.z = origin.z.saturating_add(4 << 12);
    if point_contents(&point) > CONTENTS_WATER {
        return (1, water_type);
    }
    point.z = origin.z.saturating_add(22 << 12);
    if point_contents(&point) > CONTENTS_WATER {
        return (2, water_type);
    }
    (3, water_type)
}

/// `SV_AirMove`'s wish: `forward * fmove + right * smove`, where `forward`
/// comes from `v.angles` and so carries only a third of the view pitch. The
/// stick scales the magnitude (analog), so the pitch only matters below the
/// 320 cap, exactly where the original keyboard walk speed is.
fn wish_velocity(input: MovementInput) -> (Vec3I32, i32) {
    let mut pitch = i32::from(input.pitch & 0x0fff);
    if pitch >= 2048 {
        pitch -= 4096;
    }
    // Stick units in 1/256ths, with the forward axis shrunk by cos(pitch / 3).
    let forward = (i32::from(input.forward.clamp(-127, 127))
        * psx_math::cos_q12(((pitch / 3) & 0x0fff) as u16))
        >> 4;
    let strafe = i32::from(input.strafe.clamp(-127, 127)) << 8;
    let magnitude =
        isqrt_i32(square_i32_saturating(forward).saturating_add(square_i32_saturating(strafe)));
    if magnitude == 0 {
        return (Vec3I32 { x: 0, y: 0, z: 0 }, 0);
    }
    let forward_q12 = div_q12_i32(forward, magnitude);
    let strafe_q12 = div_q12_i32(strafe, magnitude);
    let yaw = input.yaw & 0x0fff;
    let sin = psx_math::sin_q12(yaw);
    let cos = psx_math::cos_q12(yaw);
    let direction = Vec3I32 {
        x: mul_q12_i32(cos, forward_q12).saturating_sub(mul_q12_i32(sin, strafe_q12)),
        y: mul_q12_i32(sin, forward_q12).saturating_add(mul_q12_i32(cos, strafe_q12)),
        z: 0,
    };
    let speed = (MAX_SPEED_Q12 >> 8).saturating_mul(magnitude.min(127 << 8)) / 127;
    (direction, speed)
}

/// `SV_WaterMove`'s wish direction: `forward * fmove + right * smove` with
/// `forward` taken from the full view angles (`AngleVectors(v_angle)`), so the
/// pitch tilts the swim. Same horizontal basis and speed as [`wish_velocity`];
/// at level pitch the two agree exactly.
fn swim_wish_velocity(input: MovementInput) -> (Vec3I32, i32) {
    let forward = input.forward.clamp(-127, 127) as i32;
    let strafe = input.strafe.clamp(-127, 127) as i32;
    let magnitude =
        isqrt_i32(square_i32_saturating(forward).saturating_add(square_i32_saturating(strafe)));
    if magnitude == 0 {
        return (Vec3I32 { x: 0, y: 0, z: 0 }, 0);
    }
    let forward_q12 = div_q12_i32(forward, magnitude);
    let strafe_q12 = div_q12_i32(strafe, magnitude);
    let yaw = input.yaw & 0x0fff;
    let pitch = input.pitch & 0x0fff;
    let sin_yaw = psx_math::sin_q12(yaw);
    let cos_yaw = psx_math::cos_q12(yaw);
    let sin_pitch = psx_math::sin_q12(pitch);
    let cos_pitch = psx_math::cos_q12(pitch);
    // forward = (cp*cy, cp*sy, -sp); right keeps wish_velocity's basis.
    let level_forward = mul_q12_i32(cos_pitch, forward_q12);
    let direction = Vec3I32 {
        x: mul_q12_i32(cos_yaw, level_forward).saturating_sub(mul_q12_i32(sin_yaw, strafe_q12)),
        y: mul_q12_i32(sin_yaw, level_forward).saturating_add(mul_q12_i32(cos_yaw, strafe_q12)),
        z: -mul_q12_i32(sin_pitch, forward_q12),
    };
    let speed = MAX_SPEED_Q12.saturating_mul(magnitude.min(127)) / 127;
    (direction, speed)
}

/// `SV_Accelerate`, `SV_AirAccelerate` and the acceleration half of
/// `SV_WaterMove`: gain `wish_speed / 6` per tick along `direction` until the
/// speed along it reaches `cap_speed`. `current_speed` is what the caller
/// measures against (the speed along the wish, or the total water speed).
fn accelerate(
    velocity: &mut Vec3I32,
    direction: Vec3I32,
    wish_speed: i32,
    cap_speed: i32,
    current_speed: i32,
) {
    if wish_speed <= 0 {
        return;
    }
    let add_speed = cap_speed.saturating_sub(current_speed);
    if add_speed <= 0 {
        return;
    }
    let acceleration = (wish_speed / 6).min(add_speed);
    velocity.x = velocity
        .x
        .saturating_add(mul_q12_i32(acceleration, direction.x));
    velocity.y = velocity
        .y
        .saturating_add(mul_q12_i32(acceleration, direction.y));
    velocity.z = velocity
        .z
        .saturating_add(mul_q12_i32(acceleration, direction.z));
}

/// `SV_WaterMove`'s friction: `speed - frametime * speed * sv_friction` on
/// the full 3D speed, with no waterlevel term.
fn apply_water_friction(velocity: &mut Vec3I32) {
    let speed = vector_length(*velocity);
    if speed < Q12_ONE {
        *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
        return;
    }
    let scale = div_q12_i32(speed.saturating_sub(speed / 15).max(0), speed);
    velocity.x = mul_q12_i32(velocity.x, scale);
    velocity.y = mul_q12_i32(velocity.y, scale);
    velocity.z = mul_q12_i32(velocity.z, scale);
}

/// What `SV_FlyMove` reports back to `SV_WalkMove`.
#[derive(Copy, Clone, Default)]
struct Slide {
    /// 1 floor, 2 wall (`step`), 3 or 7 when it gave up.
    blocked: u8,
    /// A floor was hit on world or brush geometry (`FL_ONGROUND` was raised).
    ground: bool,
    /// The last wall plane (`steptrace`), zero when none was hit.
    wall: Vec3I16,
}

fn fly_move<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
) -> Slide {
    fly_move_for_time(
        collision,
        scratch,
        origin,
        velocity,
        Q12_ONE / VIDEO_TICKS_PER_SECOND,
    )
}

fn fly_move_for_time<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
    mut time_left: i32,
) -> Slide {
    let mut original_velocity = *velocity;
    let primal_velocity = *velocity;
    let mut planes = [Vec3I16 { x: 0, y: 0, z: 0 }; MAX_CLIP_PLANES];
    let mut plane_count = 0usize;
    let mut slide = Slide::default();

    for _ in 0..MAX_SLIDE_BUMPS {
        if velocity.x == 0 && velocity.y == 0 && velocity.z == 0 {
            break;
        }
        // Built inline on purpose. Moving this into a `Vec3I32`-returning
        // helper produced wrong destinations on the experimental MIPS-I
        // backend and correct ones on the host. The mechanism was never
        // identified, so this is an observation and a workaround, not a
        // diagnosis: keep the aggregate local rather than trusting a theory.
        let destination = Vec3I32 {
            x: origin.x.saturating_add(mul_q12_i32(velocity.x, time_left)),
            y: origin.y.saturating_add(mul_q12_i32(velocity.y, time_left)),
            z: origin.z.saturating_add(mul_q12_i32(velocity.z, time_left)),
        };
        trace(collision, origin, &destination, scratch);
        // `SV_FlyMove` gives up only on `trace.allsolid`. It deliberately does
        // not test `startsolid`: a trace that begins inside another solid can
        // still report a fraction, and refusing to move on it is what leaves a
        // player wedged forever.
        if scratch.trace.trapped() {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            slide.blocked = 3;
            return slide;
        }
        if scratch.trace.fraction > 0 {
            *origin = scratch.trace.end;
            // Re-aim at the velocity as it stands after the last clip, so the
            // next plane clips what is left of the move, not the first frame's.
            original_velocity = *velocity;
            plane_count = 0;
        }
        if scratch.trace.fraction == Q12_ONE {
            break;
        }
        if scratch.trace.normal.z as i32 > WALKABLE_NORMAL_Q12 {
            slide.blocked |= 1;
            slide.ground |= scratch.trace.blocking_body.is_none();
        }
        if scratch.trace.normal.z == 0 {
            slide.blocked |= 2;
            slide.wall = scratch.trace.normal;
        }
        time_left = mul_q12_i32(time_left, Q12_ONE.saturating_sub(scratch.trace.fraction));
        if plane_count == MAX_CLIP_PLANES {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            slide.blocked = 3;
            return slide;
        }
        planes[plane_count] = scratch.trace.normal;
        plane_count += 1;

        let mut accepted = None;
        for i in 0..plane_count {
            let candidate = clip_velocity(original_velocity, planes[i]);
            let mut clears = true;
            for (j, plane) in planes[..plane_count].iter().enumerate() {
                if i != j && dot_normal(candidate, *plane) < 0 {
                    clears = false;
                    break;
                }
            }
            if clears {
                accepted = Some(candidate);
                break;
            }
        }
        *velocity = if let Some(candidate) = accepted {
            candidate
        } else if plane_count == 2 {
            crease_velocity(*velocity, planes[0], planes[1])
        } else {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            slide.blocked = 7;
            return slide;
        };
        if dot(*velocity, primal_velocity) <= 0 {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            break;
        }
    }
    slide
}

/// `SV_PushEntity`: move by `delta` as far as the world allows. The motor's
/// trace workspace keeps the result, so the caller reads the plane it hit.
fn push_entity<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    delta: Vec3I32,
) {
    let end = add(*origin, delta);
    trace(collision, origin, &end, scratch);
    *origin = scratch.trace.end;
}

/// Quake's `SV_TryUnstick`, in fixed point. `SV_WalkMove` invokes it only
/// after the raised slide was blocked without moving even 1/32 of a unit.
/// Each two-unit nudge is traced, then the original velocity gets a 0.1-second
/// retry. A result counts only after it escapes by more than four units.
/// Returns the retry's clip bits, or 7 when every nudge failed.
fn try_unstick<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
    original_velocity: Vec3I32,
) -> u8 {
    const NUDGE_Q12: i32 = 2 << 12;
    const ESCAPE_Q12: i32 = 4 << 12;
    const RETRY_TIME_Q12: i32 = Q12_ONE / 10;
    const DIRECTIONS: [(i32, i32); 8] = [
        (NUDGE_Q12, 0),
        (0, NUDGE_Q12),
        (-NUDGE_Q12, 0),
        (0, -NUDGE_Q12),
        (NUDGE_Q12, NUDGE_Q12),
        (-NUDGE_Q12, NUDGE_Q12),
        (NUDGE_Q12, -NUDGE_Q12),
        (-NUDGE_Q12, -NUDGE_Q12),
    ];

    let stuck_origin = *origin;
    for (x, y) in DIRECTIONS {
        let target = Vec3I32 {
            x: stuck_origin.x.saturating_add(x),
            y: stuck_origin.y.saturating_add(y),
            z: stuck_origin.z,
        };
        trace(collision, &stuck_origin, &target, scratch);
        let mut candidate_origin = scratch.trace.end;
        let mut candidate_velocity = Vec3I32 {
            x: original_velocity.x,
            y: original_velocity.y,
            z: 0,
        };
        let retry = fly_move_for_time(
            collision,
            scratch,
            &mut candidate_origin,
            &mut candidate_velocity,
            RETRY_TIME_Q12,
        );
        if abs_i32(candidate_origin.x.saturating_sub(stuck_origin.x)) > ESCAPE_Q12
            || abs_i32(candidate_origin.y.saturating_sub(stuck_origin.y)) > ESCAPE_Q12
        {
            *origin = candidate_origin;
            *velocity = candidate_velocity;
            return retry.blocked;
        }
    }

    *origin = stuck_origin;
    *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
    7
}

/// True when a two-unit floor probe touches a plane that is too steep to count
/// as ground. This is the signature of the fixed-point bevel pocket recovered
/// by the airborne `SV_TryUnstick` fallback; a vertical wall in open air does
/// not satisfy it.
fn on_steep_support<C: MovementTrace + ?Sized>(
    collision: &C,
    origin: Vec3I32,
    scratch: &mut MovementScratch,
) -> bool {
    let below = Vec3I32 {
        x: origin.x,
        y: origin.y,
        z: origin.z.saturating_sub(GROUND_PROBE_Q12),
    };
    trace(collision, &origin, &below, scratch);
    let normal_z = scratch.trace.normal.z as i32;
    !scratch.trace.trapped()
        && scratch.trace.fraction < Q12_ONE
        && normal_z > 0
        && normal_z < WALKABLE_NORMAL_Q12
}

/// Lift one unit out of the fixed-point bevel pocket and retry the current
/// frame. This is the smallest vertical offset from Quake's 0..17-unit
/// `SV_CheckStuck` search and is accepted only when it immediately produces
/// real horizontal progress. The caller permits it once per airborne episode,
/// preventing repeated one-unit lifts from climbing a legitimate steep wall.
fn try_steep_seam_lift<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
    original_velocity: Vec3I32,
) -> bool {
    let start = *origin;
    let mut candidate_origin = Vec3I32 {
        x: start.x,
        y: start.y,
        z: start.z.saturating_add(Q12_ONE),
    };
    if position_stuck(collision, candidate_origin, scratch) {
        return false;
    }
    let mut candidate_velocity = original_velocity;
    let _ = fly_move(
        collision,
        scratch,
        &mut candidate_origin,
        &mut candidate_velocity,
    );
    if abs_i32(candidate_origin.x.saturating_sub(start.x)) < Q12_ONE / 8
        && abs_i32(candidate_origin.y.saturating_sub(start.y)) < Q12_ONE / 8
    {
        return false;
    }
    *origin = candidate_origin;
    *velocity = candidate_velocity;
    true
}

/// `ClipVelocity` with an overbounce of 1, zeroing components that fall
/// inside `STOP_EPSILON` (0.1 units per second).
fn clip_velocity(velocity: Vec3I32, normal: Vec3I16) -> Vec3I32 {
    let backoff = dot_normal(velocity, normal);
    let clip = |component: i32, axis: i16| {
        let out = component.saturating_sub(mul_q12_i32(backoff, axis as i32));
        if out > -STOP_EPSILON_Q12 && out < STOP_EPSILON_Q12 {
            0
        } else {
            out
        }
    };
    Vec3I32 {
        x: clip(velocity.x, normal.x),
        y: clip(velocity.y, normal.y),
        z: clip(velocity.z, normal.z),
    }
}

/// The crease case of `SV_FlyMove`: slide along `planes[0] x planes[1]`.
/// The original scales its unnormalised cross product by `dir . velocity`, so
/// the speed along the crease shrinks by the square of the planes' sine.
fn crease_velocity(velocity: Vec3I32, left: Vec3I16, right: Vec3I16) -> Vec3I32 {
    let direction = Vec3I32 {
        x: mul_q12_i32(left.y as i32, right.z as i32)
            .saturating_sub(mul_q12_i32(left.z as i32, right.y as i32)),
        y: mul_q12_i32(left.z as i32, right.x as i32)
            .saturating_sub(mul_q12_i32(left.x as i32, right.z as i32)),
        z: mul_q12_i32(left.x as i32, right.y as i32)
            .saturating_sub(mul_q12_i32(left.y as i32, right.x as i32)),
    };
    scale(direction, dot(velocity, direction))
}

fn vector_length(vector: Vec3I32) -> i32 {
    const SAFE_COMPONENT: i32 = 26_750;
    let largest = abs_i32(vector.x)
        .max(abs_i32(vector.y))
        .max(abs_i32(vector.z));
    let mut precision_shift = 0;
    while (largest >> precision_shift) > SAFE_COMPONENT {
        precision_shift += 1;
    }
    let x = vector.x >> precision_shift;
    let y = vector.y >> precision_shift;
    let z = vector.z >> precision_shift;
    isqrt_i32(
        square_i32_saturating(x)
            .saturating_add(square_i32_saturating(y))
            .saturating_add(square_i32_saturating(z)),
    )
    .saturating_mul(1 << precision_shift)
}

fn dot(left: Vec3I32, right: Vec3I32) -> i32 {
    mul_q12_i32(left.x, right.x)
        .saturating_add(mul_q12_i32(left.y, right.y))
        .saturating_add(mul_q12_i32(left.z, right.z))
}

fn dot_normal(vector: Vec3I32, normal: Vec3I16) -> i32 {
    mul_q12_i32(vector.x, normal.x as i32)
        .saturating_add(mul_q12_i32(vector.y, normal.y as i32))
        .saturating_add(mul_q12_i32(vector.z, normal.z as i32))
}

fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}

fn scale(vector: Vec3I32, factor: i32) -> Vec3I32 {
    Vec3I32 {
        x: mul_q12_i32(vector.x, factor),
        y: mul_q12_i32(vector.y, factor),
        z: mul_q12_i32(vector.z, factor),
    }
}

fn position_stuck<C: MovementTrace + ?Sized>(
    collision: &C,
    origin: Vec3I32,
    scratch: &mut MovementScratch,
) -> bool {
    trace(collision, &origin, &origin, scratch);
    scratch.trace.start_solid || scratch.trace.all_solid
}

/// Run one provider trace into the motor's workspace.
///
/// A provider that fails leaves the player blocked where the segment started,
/// which is the only answer that cannot teleport them through a wall, and the
/// failure is recorded so a gate can refuse the build. The original engine has
/// no such path at all: `SV_RecursiveHullCheck` always produces a trace.
fn trace_line<C: MovementTrace + ?Sized>(
    collision: &C,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut MovementScratch,
) {
    if !collision.trace_line(start, end, &mut scratch.bsp_trace, &mut scratch.trace) {
        scratch.stalls.insert(MovementStalls::TRACE);
        scratch.trace = MovementTraceResult::unobstructed(*end);
    }
}

fn trace<C: MovementTrace + ?Sized>(
    collision: &C,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut MovementScratch,
) {
    if !collision.trace(start, end, &mut scratch.bsp_trace, &mut scratch.trace) {
        scratch.stalls.insert(MovementStalls::TRACE);
        scratch.trace = MovementTraceResult {
            all_solid: false,
            start_solid: false,
            fraction: 0,
            end: *start,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            blocking_body: None,
        };
        return;
    }
    scratch.trace.restore_trace_invariants();
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::{ClipNode, Plane, RecordSlice};

    struct Floor;

    impl MovementTrace for Floor {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            if start.z < 0 {
                output.start_solid = true;
                output.all_solid = end.z < 0;
                output.fraction = 0;
                output.end = *start;
            } else if end.z < 0 {
                let fraction = div_q12_i32(start.z, start.z.saturating_sub(end.z));
                output.fraction = fraction;
                output.end = Vec3I32 {
                    x: start
                        .x
                        .saturating_add(mul_q12_i32(end.x.saturating_sub(start.x), fraction)),
                    y: start
                        .y
                        .saturating_add(mul_q12_i32(end.y.saturating_sub(start.y), fraction)),
                    z: 0,
                };
                output.normal = Vec3I16 {
                    x: 0,
                    y: 0,
                    z: Q12_ONE as i16,
                };
            }
            true
        }
    }

    #[test]
    fn canonical_quake_hull_uses_movement_owned_trace_scratch() {
        let mut plane = [0u8; 14];
        plane[4..6].copy_from_slice(&(Q12_ONE as i16).to_le_bytes());
        plane[10..14].copy_from_slice(&2i32.to_le_bytes());
        let mut node = [0u8; 6];
        node[2..4].copy_from_slice(&crate::collision::CONTENTS_EMPTY.to_le_bytes());
        node[4..6].copy_from_slice(&crate::collision::CONTENTS_SOLID.to_le_bytes());
        let hull = CollisionHull::new(
            RecordSlice::<Plane>::new(&plane).unwrap(),
            RecordSlice::<ClipNode>::new(&node).unwrap(),
            0,
        )
        .unwrap();
        let mut scratch = MovementScratch::new();
        assert!(MovementTrace::trace(
            &hull,
            &Vec3I32 {
                x: 0,
                y: 0,
                z: Q12_ONE,
            },
            &Vec3I32 {
                x: 0,
                y: 0,
                z: -Q12_ONE,
            },
            &mut scratch.bsp_trace,
            &mut scratch.trace,
        ));
        assert!(scratch.trace.fraction < Q12_ONE);
        assert_eq!(
            scratch.trace.normal,
            Vec3I16 {
                x: 0,
                y: 0,
                z: Q12_ONE as i16
            }
        );
    }

    #[test]
    fn diagonal_input_is_normalized_to_max_speed() {
        let (_, axial) = wish_velocity(MovementInput {
            forward: 127,
            ..MovementInput::default()
        });
        let (diagonal_direction, diagonal) = wish_velocity(MovementInput {
            forward: 127,
            strafe: 127,
            ..MovementInput::default()
        });
        assert_eq!(axial, MAX_SPEED_Q12);
        assert_eq!(diagonal, MAX_SPEED_Q12);
        assert!((vector_length(diagonal_direction) - Q12_ONE).abs() < 32);
    }

    /// Open water: nothing to hit, every sample point is water.
    struct OpenWater;

    impl MovementTrace for OpenWater {
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

    /// A body that has already spent a tick in the water: `SV_ClientThink`
    /// reads the water level the previous frame's `SV_CheckWater` left.
    fn swimming(origin: Vec3I32, level: u8) -> MovementState {
        let mut state = MovementState::new(origin);
        state.water_level = level;
        state.water_type = CONTENTS_WATER;
        state
    }

    #[test]
    fn ordinary_swimming_jump_still_processes_cross() {
        let initial = swimming(Vec3I32 { x: 0, y: 0, z: 256 << 12 }, 3);
        let mut held = initial;
        let mut released = initial;
        let held_events = held.update_ticks(
            &OpenWater, &mut MovementScratch::default(),
            MovementInput { jump: true, ..MovementInput::default() },
            1, |_| Some(CONTENTS_WATER),
        );
        released.update_ticks(
            &OpenWater, &mut MovementScratch::default(),
            MovementInput::default(), 1, |_| Some(CONTENTS_WATER),
        );
        assert_eq!(held.water_jump_ticks, 0);
        assert!(held.velocity.z > released.velocity.z);
        assert!(held_events.events.contains(MovementEvents::SWIM));
        assert!(held.swim_sound_ticks > 0, "the stroke sound is throttled");
    }

    #[test]
    fn swimming_forward_follows_the_view_pitch() {
        // Looking up 45 degrees (negative pitch in Quake's turn) and holding
        // forward must rise, level must not, and looking down must sink
        // faster than the idle drift; the horizontal speed matches the level
        // wish scaled by cos(pitch).
        fn swim(pitch: u16, ticks: usize) -> (Vec3I32, Vec3I32) {
            let mut state = swimming(
                Vec3I32 {
                    x: 0,
                    y: 0,
                    z: 256 << 12,
                },
                3,
            );
            let mut scratch = MovementScratch::default();
            let input = MovementInput {
                forward: 127,
                pitch,
                ..MovementInput::default()
            };
            for _ in 0..ticks {
                state.update_ticks(&OpenWater, &mut scratch, input, 1, |_| Some(CONTENTS_WATER));
            }
            (state.origin(), state.velocity())
        }
        let (level_origin, level_velocity) = swim(0, 60);
        let (up_origin, up_velocity) = swim(4096 - 512, 60);
        let (down_origin, down_velocity) = swim(512, 60);
        let (_, drift_velocity) = {
            let mut state = swimming(
                Vec3I32 {
                    x: 0,
                    y: 0,
                    z: 256 << 12,
                },
                3,
            );
            let mut scratch = MovementScratch::default();
            for _ in 0..60 {
                state.update_ticks(
                    &OpenWater,
                    &mut scratch,
                    MovementInput::default(),
                    1,
                    |_| Some(CONTENTS_WATER),
                );
            }
            (state.origin(), state.velocity())
        };
        assert!(
            level_velocity.z == 0 && level_origin.z == 256 << 12,
            "{level_velocity:?}"
        );
        assert!(
            up_velocity.z > 0 && up_origin.z > 256 << 12,
            "{up_velocity:?}"
        );
        assert!(
            down_velocity.z < 0 && down_origin.z < 256 << 12,
            "{down_velocity:?}"
        );
        assert!(
            down_velocity.z < drift_velocity.z,
            "{down_velocity:?} {drift_velocity:?}"
        );
        // cos(45 deg) of the level swim speed, within fixed-point rounding.
        let expected = mul_q12_i32(level_velocity.x, psx_math::cos_q12(512));
        assert!(
            (up_velocity.x - expected).abs() <= 2 << 12,
            "{} vs {expected}",
            up_velocity.x
        );
        // Symmetric up to fixed-point rounding of the pitch basis.
        assert!(
            (up_velocity.x - down_velocity.x).abs() <= 64,
            "{} vs {}",
            up_velocity.x,
            down_velocity.x
        );
        assert!(up_velocity.z.abs() <= up_velocity.x + (1 << 12));
    }

    #[test]
    fn held_jump_only_launches_once_until_released() {
        let floor = Floor;
        let mut state = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        let input = MovementInput {
            jump: true,
            ..MovementInput::default()
        };
        let mut scratch = MovementScratch::default();
        // Standing, with the button up: `FL_JUMPRELEASED` and `FL_ONGROUND`.
        state.update_ticks(&floor, &mut scratch, MovementInput::default(), 1, |_| {
            Some(CONTENTS_EMPTY)
        });
        assert!(state.grounded());
        let first = state.update_ticks(&floor, &mut scratch, input, 1, |_| Some(CONTENTS_EMPTY));
        assert!(first.events.contains(MovementEvents::JUMP));
        let second = state.update_ticks(&floor, &mut scratch, input, 1, |_| Some(CONTENTS_EMPTY));
        assert!(!second.events.contains(MovementEvents::JUMP));
        assert!(state.origin().z > 1 << 12);
    }

    struct Void;

    impl MovementTrace for Void {
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

    struct VerticalWall;

    impl MovementTrace for VerticalWall {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            if end.x > 0 {
                output.fraction = 0;
                output.end = *start;
                output.normal = Vec3I16 {
                    x: -(Q12_ONE as i16),
                    y: 0,
                    z: 0,
                };
            }
            true
        }
    }

    #[test]
    fn airborne_vertical_wall_does_not_trigger_seam_lift() {
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 64 << 12,
        });
        let mut scratch = MovementScratch::default();
        let input = MovementInput {
            forward: 127,
            yaw: 0,
            ..MovementInput::default()
        };
        for _ in 0..30 {
            let _ = state.update_ticks(&VerticalWall, &mut scratch, input, 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        assert_eq!(state.origin().x, 0);
        assert_eq!(state.origin().y, 0);
        assert!(!state.steep_seam_recovered);
    }

    fn drop_into(height_units: i32, contents: i16) -> MovementEvents {
        let floor = Floor;
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: height_units << 12,
        });
        let mut scratch = MovementScratch::default();
        let mut events = MovementEvents::default();
        for _ in 0..600 {
            let frame =
                state.update_ticks(&floor, &mut scratch, MovementInput::default(), 1, |point| {
                    // A shallow puddle sitting on the test floor: deep enough
                    // to be sampled at the landing, too shallow to swim in.
                    Some(if point.z < 8 << 12 {
                        contents
                    } else {
                        CONTENTS_EMPTY
                    })
                });
            events.merge(frame.events);
            if state.grounded() {
                break;
            }
        }
        events
    }

    fn drop_from(height_units: i32) -> MovementEvents {
        drop_into(height_units, CONTENTS_EMPTY)
    }

    /// Quake's fall-damage cutoff is `self.jump_flag < -650`, a velocity in
    /// units per second. This port carries velocity in the same units scaled
    /// by Q12: gravity subtracts `(800 << 12) / 60` per tick for Quake's
    /// 800 unit/second^2 `sv_gravity`, `frame_displacement` divides velocity
    /// by the same 60 to reach units of position, and the jump impulse is
    /// Quake's 270 unit/second literal. One second of free fall must
    /// therefore leave the player at Quake's terminal 800 units/second, which
    /// makes the port's threshold exactly `-(650 << 12)`.
    #[test]
    fn free_fall_proves_velocity_is_quake_units_per_second_in_q12() {
        let void = Void;
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
        let mut scratch = MovementScratch::default();
        for _ in 0..VIDEO_TICKS_PER_SECOND {
            state.update_ticks(&void, &mut scratch, MovementInput::default(), 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        let expected = -(800 << 12);
        assert!(
            (state.velocity().z - expected).abs() <= VIDEO_TICKS_PER_SECOND,
            "one second of free fall reached {} instead of {expected}",
            state.velocity().z
        );
        assert_eq!(JUMP_SPEED_Q12, 270 << 12);
        assert_eq!(HARD_LAND_SPEED_Q12, -(650 << 12));
        assert_eq!(LAND_SPEED_Q12, -(300 << 12));
    }

    /// Free fall reaches 650 units/second after `650 * 650 / (2 * 800)` =
    /// 264 units, so a drop shorter than that lands softly and a longer one
    /// raises the damaging landing.
    #[test]
    fn hard_land_starts_at_the_original_264_unit_drop() {
        assert!(drop_from(200).contains(MovementEvents::LAND));
        assert!(!drop_from(200).contains(MovementEvents::HARD_LAND));
        assert!(drop_from(300).contains(MovementEvents::HARD_LAND));
        assert!(!drop_from(300).contains(MovementEvents::LAND));
    }

    /// Only `CONTENT_WATER` turns a landing into a splash in the original;
    /// slime and lava landings still take the fall.
    #[test]
    fn only_water_replaces_a_hard_landing_with_a_splash() {
        let water = drop_into(300, CONTENTS_WATER);
        assert!(water.contains(MovementEvents::WATER_LAND));
        assert!(!water.contains(MovementEvents::HARD_LAND));
        for contents in [CONTENTS_SLIME, CONTENTS_LAVA] {
            let landed = drop_into(300, contents);
            assert!(landed.contains(MovementEvents::HARD_LAND));
            assert!(!landed.contains(MovementEvents::WATER_LAND));
        }
    }

    /// A provider that reports an impact plane and `allsolid` at once has
    /// contradicted `SV_RecursiveHullCheck`, which clears `allsolid` before it
    /// can record a plane. Trusting the stray flag freezes a player standing
    /// on ordinary floor: `categorize_position` refuses to ground them and
    /// `fly_move` calls them trapped, so the origin never changes again.
    struct ContradictoryFloor;

    impl MovementTrace for ContradictoryFloor {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            Floor.trace(start, end, scratch, output);
            // Exactly the shape E1M1 produced on the guest: an ordinary floor
            // contact, fraction zero with a walkable plane, carrying a stray
            // `all_solid`.
            if output.fraction < Q12_ONE {
                output.all_solid = true;
            }
            true
        }
    }

    #[test]
    fn a_stray_all_solid_beside_an_impact_plane_cannot_freeze_the_player() {
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
        let mut scratch = MovementScratch::default();
        let input = MovementInput {
            forward: 127,
            ..MovementInput::default()
        };
        for _ in 0..60 {
            state.update_ticks(&ContradictoryFloor, &mut scratch, input, 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        assert!(state.grounded(), "the floor contact must still ground");
        assert!(
            state.origin().x > 32 << 12,
            "the player walked {} units instead of moving off the spot",
            state.origin().x >> 12
        );
    }

    #[test]
    fn caller_owned_gravity_reproduces_e1m8s_one_eighth_fall() {
        let void = Void;
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
        let mut scratch = MovementScratch::default();
        for _ in 0..VIDEO_TICKS_PER_SECOND {
            state.update_ticks_with_gravity(
                &void,
                &mut scratch,
                MovementInput::default(),
                1,
                100,
                |_| Some(CONTENTS_EMPTY),
            );
        }
        assert!((state.velocity().z + (100 << 12)).abs() <= VIDEO_TICKS_PER_SECOND);
    }

    /// A trace that never leaves solid keeps `allsolid`, and that case really
    /// is `SV_FlyMove`'s trapped entity.
    struct Trapped;

    impl MovementTrace for Trapped {
        fn trace(
            &self,
            start: &Vec3I32,
            _end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult {
                all_solid: true,
                start_solid: true,
                fraction: Q12_ONE,
                end: *start,
                normal: Vec3I16 { x: 0, y: 0, z: 0 },
                blocking_body: None,
            };
            true
        }
    }

    #[test]
    fn a_genuinely_trapped_trace_is_still_trapped() {
        let origin = Vec3I32 {
            x: 0,
            y: 0,
            z: 24 << 12,
        };
        let mut state = MovementState::new(origin);
        let mut scratch = MovementScratch::default();
        let input = MovementInput {
            forward: 127,
            ..MovementInput::default()
        };
        for _ in 0..8 {
            let frame =
                state.update_ticks(&Trapped, &mut scratch, input, 1, |_| Some(CONTENTS_EMPTY));
            assert!(!frame.moved);
            assert!(frame.stalls.is_empty(), "a trapped trace is not a stall");
        }
        assert!(!state.grounded());
        assert_eq!(state.origin(), origin);
        assert_eq!(state.velocity(), Vec3I32 { x: 0, y: 0, z: 0 });
    }

    struct FailingProvider;

    impl MovementTrace for FailingProvider {
        fn trace(
            &self,
            _start: &Vec3I32,
            _end: &Vec3I32,
            _scratch: &mut TraceScratch,
            _output: &mut MovementTraceResult,
        ) -> bool {
            false
        }
    }

    /// Neither an unresolvable `point_contents` sample nor a provider that
    /// cannot trace may skip the motor. Both degrade and are reported.
    #[test]
    fn unresolvable_queries_degrade_instead_of_skipping_the_motor() {
        let mut state = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        let mut scratch = MovementScratch::default();
        let frame = state.update_ticks(&Floor, &mut scratch, MovementInput::default(), 1, |_| None);
        assert!(frame.stalls.contains(MovementStalls::CONTENTS));
        assert!(!frame.stalls.contains(MovementStalls::TRACE));
        // An unresolved sample reads as the open air Quake would have found.
        assert_eq!(frame.water_level, 0);
        assert_eq!(frame.water_type, CONTENTS_EMPTY);
        assert!(state.grounded(), "the motor still ran and found the floor");

        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
        let frame = state.update_ticks(
            &FailingProvider,
            &mut scratch,
            MovementInput {
                forward: 127,
                ..MovementInput::default()
            },
            1,
            |_| Some(CONTENTS_EMPTY),
        );
        assert!(frame.stalls.contains(MovementStalls::TRACE));
        // Blocked where it started, never teleported through the geometry.
        assert_eq!(state.origin().x, 0);
        assert!(!state.grounded());
    }

    #[test]
    fn water_samples_feet_waist_and_eyes() {
        let origin = Vec3I32 { x: 0, y: 0, z: 0 };
        let (level, kind) = water_level(&origin, &mut |point| {
            if point.z <= 5 << 12 {
                CONTENTS_WATER
            } else {
                CONTENTS_EMPTY
            }
        });
        assert_eq!(level, 2);
        assert_eq!(kind, CONTENTS_WATER);
    }

    /// The four shapes a provider can hand the motor, each read the way
    /// `SV_FlyMove` would read the original tracer's answer. Every one of them
    /// goes through `restore_trace_invariants` first, because that is where the
    /// flag is made honest; `trapped()` itself is then Quake's own bare
    /// `trace.allsolid`.
    #[test]
    fn only_a_trace_that_never_left_solid_counts_as_trapped() {
        let end = Vec3I32 { x: 0, y: 0, z: 0 };
        let floor = Vec3I16 {
            x: 0,
            y: 0,
            z: Q12_ONE as i16,
        };
        let restored = |mut trace: MovementTraceResult| {
            trace.restore_trace_invariants();
            trace
        };

        // A box resting on a surface and moving down: `all_solid` beside a
        // zero fraction and the floor's own normal. A contact, not a trap.
        let contact = MovementTraceResult {
            all_solid: true,
            start_solid: false,
            fraction: 0,
            end,
            normal: floor,
            blocking_body: None,
        };
        // Leaving the epsilon shell of a brush the box already overlaps.
        let shell = MovementTraceResult {
            all_solid: false,
            start_solid: true,
            ..contact
        };
        // What `SV_RecursiveHullCheck` actually returns when it never got out
        // of the solid area: it bails before recording either a plane or a
        // fraction, so `fraction` is still its initial 1 and the plane is zero.
        let trapped = MovementTraceResult {
            all_solid: true,
            start_solid: true,
            fraction: Q12_ONE,
            end,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            blocking_body: None,
        };
        // Both flags beside impact evidence. The original cannot emit this at
        // all, so the evidence wins over the flag.
        let contradictory = MovementTraceResult {
            start_solid: true,
            ..contact
        };

        assert!(
            !restored(contact).trapped(),
            "resting contact is not a trap"
        );
        assert!(
            !restored(shell).trapped(),
            "an exit from the epsilon shell is not a trap"
        );
        assert!(restored(trapped).trapped());
        assert!(
            !restored(contradictory).trapped(),
            "an impact plane outranks a flag the original could not have set"
        );
        assert!(!restored(MovementTraceResult::unobstructed(end)).trapped());
    }

    /// The surface a brush entity presents to a box already resting on it: a
    /// downward move reports the canonical tracer's contact result, everything
    /// else is clear. This is the shape that froze the Start route on
    /// `func_bossgate` once the episode runes made it solid.
    struct RestingContact;

    impl MovementTrace for RestingContact {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            if end.z < start.z {
                output.all_solid = true;
                output.start_solid = false;
                output.fraction = 0;
                output.end = *start;
                output.normal = Vec3I16 {
                    x: 0,
                    y: 0,
                    z: Q12_ONE as i16,
                };
            }
            true
        }
    }

    #[test]
    fn a_box_resting_on_a_brush_entity_still_walks() {
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 24 << 12,
        });
        let mut scratch = MovementScratch::new();
        let input = MovementInput {
            forward: 127,
            strafe: 0,
            yaw: 0,
            pitch: 0,
            jump: false,
        };
        for _ in 0..32 {
            state.update_ticks(&RestingContact, &mut scratch, input, 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        assert!(
            state.grounded(),
            "a resting contact is standing, not falling"
        );
        assert!(
            state.origin().x > 0,
            "player should walk forward off a resting contact, got {:?}",
            state.origin()
        );
    }

    /// A flat floor at `z = 0` for `x < 0` and a raised one of `height` units
    /// for `x >= 0`, both walkable, expanded exactly like a hull-1 step.
    struct Ledge {
        height: i32,
    }

    impl MovementTrace for Ledge {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            let floor = |x: i32| if x >= 0 { self.height << 12 } else { 0 };
            *output = MovementTraceResult::unobstructed(*end);
            // The riser is a vertical wall at x = 0 between the two floors.
            if start.x < 0 && end.x >= 0 && start.z < (self.height << 12) {
                output.fraction = 0;
                output.end = *start;
                output.normal = Vec3I16 {
                    x: -(Q12_ONE as i16),
                    y: 0,
                    z: 0,
                };
                return true;
            }
            let ground = floor(end.x);
            if end.z < ground {
                output.fraction = if start.z > ground {
                    div_q12_i32(start.z - ground, start.z.saturating_sub(end.z))
                } else {
                    0
                };
                output.end = Vec3I32 {
                    x: start.x.saturating_add(mul_q12_i32(
                        end.x.saturating_sub(start.x),
                        output.fraction,
                    )),
                    y: start.y.saturating_add(mul_q12_i32(
                        end.y.saturating_sub(start.y),
                        output.fraction,
                    )),
                    z: ground,
                };
                output.normal = Vec3I16 {
                    x: 0,
                    y: 0,
                    z: Q12_ONE as i16,
                };
            }
            true
        }

        fn trace_line(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            self.trace(start, end, scratch, output)
        }
    }

    #[test]
    fn water_jump_launch_ignores_cross_on_first_and_later_ticks() {
        let initial = swimming(
            Vec3I32 {
                x: -12 << 12,
                y: 0,
                z: 8 << 12,
            },
            2,
        );
        let collision = Ledge { height: 40 };
        let mut released = initial;
        let mut held = initial;
        let contents = |point: &Vec3I32| {
            Some(if point.z < 20 << 12 {
                CONTENTS_WATER
            } else {
                CONTENTS_EMPTY
            })
        };
        released.update_ticks(
            &collision,
            &mut MovementScratch::default(),
            MovementInput {
                forward: 127,
                jump: false,
                ..MovementInput::default()
            },
            1,
            contents,
        );
        held.update_ticks(
            &collision,
            &mut MovementScratch::default(),
            MovementInput {
                forward: 127,
                jump: true,
                ..MovementInput::default()
            },
            1,
            contents,
        );
        assert!(released.water_jump_ticks > 0 && held.water_jump_ticks > 0);
        // Only `FL_JUMPRELEASED` differs: the idle tick raises it again
        // after `CheckWaterJump` cleared it.
        assert!(released.jump_released && !held.jump_released);
        released.jump_released = false;
        assert_eq!(released, held);
        // Client input acceleration precedes PlayerPreThink's launch; with
        // a clear vertical trace, its 225-unit launch survives this move.
        assert_eq!(released.velocity.z, WATER_JUMP_OUT_SPEED_Q12);
        assert_eq!(released.water_jump_ticks, WATER_JUMP_TICKS);
        // Once the flag was already active at entry, held Cross is ignored.
        let mut a = released;
        let mut b = released;
        a.update_ticks(
            &collision,
            &mut MovementScratch::default(),
            MovementInput {
                jump: false,
                ..MovementInput::default()
            },
            1,
            contents,
        );
        b.update_ticks(
            &collision,
            &mut MovementScratch::default(),
            MovementInput {
                jump: true,
                ..MovementInput::default()
            },
            1,
            contents,
        );
        a.jump_released = b.jump_released;
        assert_eq!(a, b);
    }

    fn walks_up(height: i32) -> bool {
        let mut state = MovementState::new(Vec3I32 {
            x: -64 << 12,
            y: 0,
            z: 24 << 12,
        });
        let mut scratch = MovementScratch::new();
        let input = MovementInput {
            forward: 127,
            strafe: 0,
            yaw: 0,
            pitch: 0,
            jump: false,
        };
        for _ in 0..120 {
            state.update_ticks(&Ledge { height }, &mut scratch, input, 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        state.origin().x > 0
    }

    /// `SV_movestep`'s 18 unit `STEPSIZE` is the whole reason a route has to be
    /// authored around E1M1's lower-level west ledge, whose riser is 24. Pin
    /// both sides of the limit: a route that asks the motor to climb more than
    /// this is asking for something the original cannot do either, and it
    /// livelocks against the wall instead of failing.
    #[test]
    fn the_motor_climbs_a_step_but_never_a_ledge() {
        assert_eq!(STEP_HEIGHT_Q12, 18 << 12);
        assert!(walks_up(18), "an 18 unit step is walkable");
        assert!(!walks_up(24), "a 24 unit ledge is not a step");
    }

    fn grounded_on(floor: &Floor) -> MovementState {
        let _ = floor;
        let mut state = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        state.update_ticks(&Floor, &mut MovementScratch::default(), MovementInput::default(), 1, |_| {
            Some(CONTENTS_EMPTY)
        });
        state
    }

    /// A floor that ends at `x = 100` for the point trace `SV_UserFriction`
    /// uses, while the hull floor stays under the player.
    struct Plateau;

    impl MovementTrace for Plateau {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            Floor.trace(start, end, scratch, output)
        }

        fn trace_line(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            if start.x < 100 << 12 {
                output.fraction = Q12_ONE / 2;
            }
            true
        }
    }

    #[test]
    fn friction_doubles_when_the_leading_edge_overhangs_a_drop() {
        let run = |x: i32| {
            let mut state = grounded_on(&Floor);
            state.origin.x = x << 12;
            state.velocity.x = 200 << 12;
            state.user_friction(&Plateau, &mut MovementScratch::default());
            state.velocity.x
        };
        // Over the floor: 200 - 200/15. Hanging 16 units past x = 100: twice that.
        let drop = (200 << 12) / 15;
        assert!((run(0) - ((200 << 12) - drop)).abs() < 512, "{}", run(0));
        assert!(
            (run(90) - ((200 << 12) - 2 * drop)).abs() < 512,
            "{}",
            run(90)
        );
    }

    #[test]
    fn a_jump_pressed_in_the_air_fires_the_moment_the_player_lands() {
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 6 << 12,
        });
        let mut scratch = MovementScratch::default();
        let idle = MovementInput::default();
        let held = MovementInput {
            jump: true,
            ..idle
        };
        let mut tick = |state: &mut MovementState, input| {
            state.update_ticks(&Floor, &mut scratch, input, 1, |_| Some(CONTENTS_EMPTY))
        };
        // Button up while falling raises FL_JUMPRELEASED; pressing it while
        // still airborne must not be swallowed.
        assert!(!tick(&mut state, idle).events.contains(MovementEvents::JUMP));
        let mut jumped_at = None;
        for n in 0..60 {
            if tick(&mut state, held).events.contains(MovementEvents::JUMP) {
                jumped_at = Some(n);
                break;
            }
        }
        assert!(jumped_at.is_some(), "the held press fires on landing");
        assert!(state.velocity().z > 200 << 12);
    }

    /// A scripted provider: each trace answers with the next canned hit.
    struct Script {
        hits: core::cell::Cell<usize>,
        planes: [(i32, Vec3I16); 3],
    }

    impl MovementTrace for Script {
        fn trace(
            &self,
            start: &Vec3I32,
            end: &Vec3I32,
            _scratch: &mut TraceScratch,
            output: &mut MovementTraceResult,
        ) -> bool {
            *output = MovementTraceResult::unobstructed(*end);
            let n = self.hits.get();
            self.hits.set(n + 1);
            if let Some(&(fraction, normal)) = self.planes.get(n) {
                if fraction < Q12_ONE {
                    output.fraction = fraction;
                    output.end = Vec3I32 {
                        x: start.x + mul_q12_i32(end.x - start.x, fraction),
                        y: start.y + mul_q12_i32(end.y - start.y, fraction),
                        z: start.z + mul_q12_i32(end.z - start.z, fraction),
                    };
                    output.normal = normal;
                }
            }
            true
        }
    }

    #[test]
    fn a_partial_move_re_aims_the_clip_at_the_velocity_that_is_left() {
        let side = Vec3I16 {
            x: -2896,
            y: -2896,
            z: 0,
        };
        let front = Vec3I16 {
            x: 0,
            y: -Q12_ONE as i16,
            z: 0,
        };
        let world = Script {
            hits: core::cell::Cell::new(0),
            planes: [(Q12_ONE / 2, side), (Q12_ONE / 2, front), (Q12_ONE, front)],
        };
        let mut origin = Vec3I32 { x: 0, y: 0, z: 0 };
        let mut velocity = Vec3I32 {
            x: 600 << 12,
            y: 600 << 12,
            z: 0,
        };
        let mut scratch = MovementScratch::default();
        let _ = fly_move(&world, &mut scratch, &mut origin, &mut velocity);
        // Clipped against the diagonal first (sliding to +x,-y), then the
        // second plane clips what is left: no motion into the y wall remains.
        let after_first = clip_velocity(
            Vec3I32 {
                x: 600 << 12,
                y: 600 << 12,
                z: 0,
            },
            side,
        );
        let expected = clip_velocity(after_first, front);
        assert_eq!(velocity, expected);
        assert!(velocity.y == 0, "{velocity:?}");
    }

    #[test]
    fn a_floor_under_a_monster_is_not_ground() {
        struct Head;
        impl MovementTrace for Head {
            fn trace(
                &self,
                start: &Vec3I32,
                end: &Vec3I32,
                scratch: &mut TraceScratch,
                output: &mut MovementTraceResult,
            ) -> bool {
                let ok = Floor.trace(start, end, scratch, output);
                if output.fraction < Q12_ONE {
                    output.blocking_body = Some(0);
                }
                ok
            }
        }
        let mut state = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        let mut scratch = MovementScratch::default();
        for _ in 0..4 {
            state.update_ticks(&Head, &mut scratch, MovementInput::default(), 1, |_| {
                Some(CONTENTS_EMPTY)
            });
        }
        assert!(!state.grounded(), "only world and brush floors ground");
        let mut brush = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        brush.update_ticks(&Floor, &mut scratch, MovementInput::default(), 1, |_| {
            Some(CONTENTS_EMPTY)
        });
        assert!(brush.grounded());
    }

    #[test]
    fn running_into_a_wall_you_face_bleeds_the_speed_along_it() {
        let wall = Vec3I16 {
            x: -(Q12_ONE as i16),
            y: 0,
            z: 0,
        };
        let mut state = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        state.velocity = Vec3I32 {
            x: 100 << 12,
            y: 100 << 12,
            z: 0,
        };
        // Facing +x into the wall: d = -1 + 0.5, so half the tangent is kept.
        state.wall_friction(wall, MovementInput::default());
        assert_eq!(state.velocity.y, 50 << 12);
        // Facing away from it: no friction.
        let mut away = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        away.velocity = Vec3I32 {
            x: 0,
            y: 100 << 12,
            z: 0,
        };
        away.wall_friction(
            wall,
            MovementInput {
                yaw: 2048,
                ..MovementInput::default()
            },
        );
        assert_eq!(away.velocity.y, 100 << 12);
    }

    #[test]
    fn the_crease_scales_speed_by_the_square_of_the_planes_sine() {
        // Planes at 60 degrees: the unnormalised cross product has length
        // sin 60, so a 100 unit/s slide keeps 100 * sin^2 60 = 75.
        let left = Vec3I16 {
            x: Q12_ONE as i16,
            y: 0,
            z: 0,
        };
        let right = Vec3I16 {
            x: 2048,
            y: 3547,
            z: 0,
        };
        let out = crease_velocity(
            Vec3I32 {
                x: 0,
                y: 0,
                z: 100 << 12,
            },
            left,
            right,
        );
        assert!(
            (out.z - (75 << 12)).abs() < 300,
            "{} units",
            out.z as f32 / 4096.0
        );
    }

    #[test]
    fn looking_down_shortens_the_walk_by_a_third_of_the_pitch() {
        // cos(90 / 3 degrees) = 0.866: a full stick forward no longer reaches
        // the 320 cap, 277 units a second instead.
        let (_, level) = wish_velocity(MovementInput {
            forward: 127,
            ..MovementInput::default()
        });
        let (_, down) = wish_velocity(MovementInput {
            forward: 127,
            pitch: 1024,
            ..MovementInput::default()
        });
        let (_, up) = wish_velocity(MovementInput {
            forward: 127,
            pitch: 4096 - 1024,
            ..MovementInput::default()
        });
        assert_eq!(level, 320 << 12);
        assert!((down - (277 << 12)).abs() < 3 << 12, "{}", down >> 12);
        assert!((up - down).abs() < 512);
        // Strafing is untouched by pitch.
        let (_, side) = wish_velocity(MovementInput {
            strafe: 127,
            pitch: 1024,
            ..MovementInput::default()
        });
        assert_eq!(side, 320 << 12);
    }

    #[test]
    fn a_fresh_teleport_ignores_backward_input_for_seven_tenths_of_a_second() {
        let mut state = grounded_on(&Floor);
        state.teleport_with_velocity(Vec3I32 { x: 0, y: 0, z: 0 }, Vec3I32 { x: 0, y: 0, z: 0 });
        let back = MovementInput {
            forward: -127,
            ..MovementInput::default()
        };
        let mut scratch = MovementScratch::default();
        for _ in 0..41 {
            state.update_ticks(&Floor, &mut scratch, back, 1, |_| Some(CONTENTS_EMPTY));
        }
        assert_eq!(state.velocity().x, 0, "backing out of the arrival is refused");
        for _ in 0..60 {
            state.update_ticks(&Floor, &mut scratch, back, 1, |_| Some(CONTENTS_EMPTY));
        }
        assert!(state.velocity().x < 0, "after 0.7 s the player can walk back");
    }

    #[test]
    fn swimming_drags_the_velocity_by_the_water_level() {
        let drag = |level: u8| {
            let mut state = swimming(Vec3I32 { x: 0, y: 0, z: 256 << 12 }, level);
            state.velocity.x = 750 << 12;
            // No friction or acceleration: call only the drag arithmetic via a
            // tick with the level pinned and nothing but drag to act on.
            let mut scratch = MovementScratch::default();
            state.update_ticks(&OpenWater, &mut scratch, MovementInput::default(), 1, |_| {
                Some(match level {
                    1 => CONTENTS_EMPTY,
                    _ => CONTENTS_WATER,
                })
            });
            state.velocity.x
        };
        // Level 3 loses 3/75 of the speed on top of water friction (1/15).
        let after_friction = (750 << 12) - (750 << 12) / 15;
        let level_three = drag(3);
        assert!(
            level_three < after_friction - after_friction * 3 / 75 + 4096
                && level_three > after_friction - after_friction * 3 / 75 - 4096,
            "{level_three}"
        );
    }

    #[test]
    fn a_held_swim_button_strokes_once_a_second() {
        let mut state = swimming(Vec3I32 { x: 0, y: 0, z: 256 << 12 }, 3);
        let mut scratch = MovementScratch::default();
        let held = MovementInput {
            jump: true,
            ..MovementInput::default()
        };
        let strokes = (0..180)
            .filter(|_| {
                state
                    .update_ticks(&OpenWater, &mut scratch, held, 1, |_| Some(CONTENTS_WATER))
                    .events
                    .contains(MovementEvents::SWIM)
            })
            .count();
        assert_eq!(strokes, 3);
    }

    #[test]
    fn a_ledge_30_units_away_is_out_of_the_water_jump_probe() {
        // The original traces a point 24 units ahead; the old hull sweep
        // reported the wall from 40 units out.
        let mut state = swimming(
            Vec3I32 {
                x: -30 << 12,
                y: 0,
                z: 8 << 12,
            },
            2,
        );
        let contents = |point: &Vec3I32| {
            Some(if point.z < 20 << 12 {
                CONTENTS_WATER
            } else {
                CONTENTS_EMPTY
            })
        };
        state.update_ticks(
            &Ledge { height: 40 },
            &mut MovementScratch::default(),
            MovementInput {
                forward: 127,
                ..MovementInput::default()
            },
            1,
            contents,
        );
        assert_eq!(state.water_jump_ticks, 0);
    }

    fn swims_up(height: i32) -> bool {
        let mut state = swimming(
            Vec3I32 {
                x: -64 << 12,
                y: 0,
                z: 0,
            },
            3,
        );
        let mut scratch = MovementScratch::new();
        let input = MovementInput {
            forward: 127,
            ..MovementInput::default()
        };
        for _ in 0..120 {
            state.update_ticks(&Ledge { height }, &mut scratch, input, 1, |_| {
                Some(CONTENTS_WATER)
            });
        }
        state.origin().x > 0
    }

    #[test]
    fn a_swimmer_steps_up_a_low_ledge_but_not_a_high_one() {
        assert!(swims_up(12), "SV_WalkMove steps swimmers up 18 units");
        assert!(!swims_up(30));
    }

    #[test]
    fn a_corpse_falls_hops_and_comes_to_rest_on_the_floor() {
        let mut scratch = MovementScratch::default();
        // Killed in mid-air: it falls to the floor instead of hanging there.
        let mut falling = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 80 << 12,
        });
        falling.die(0);
        for _ in 0..120 {
            falling.update_dead(&Floor, &mut scratch, 1, DEFAULT_GRAVITY);
        }
        assert!(falling.grounded());
        assert_eq!(falling.origin().z, 0);
        assert_eq!(falling.velocity(), Vec3I32 { x: 0, y: 0, z: 0 });
        // Killed standing: the 300 unit/s hop leaves the floor and lands again.
        let mut standing = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        standing.update_ticks(&Floor, &mut scratch, MovementInput::default(), 1, |_| {
            Some(CONTENTS_EMPTY)
        });
        standing.die(300);
        standing.update_dead(&Floor, &mut scratch, 1, DEFAULT_GRAVITY);
        assert!(!standing.grounded() && standing.origin().z > 0);
        let apex = (0..120)
            .map(|_| {
                standing.update_dead(&Floor, &mut scratch, 1, DEFAULT_GRAVITY);
                standing.origin().z
            })
            .max()
            .unwrap();
        assert!(apex > 30 << 12, "a full hop rises about 56 units");
        assert!(standing.grounded() && standing.origin().z == 0);
        // Already rising at 10 or more: no extra hop.
        let mut launched = MovementState::new(Vec3I32 { x: 0, y: 0, z: 0 });
        launched.add_velocity(Vec3I32 {
            x: 0,
            y: 0,
            z: 50 << 12,
        });
        launched.die(300);
        assert_eq!(launched.velocity().z, 50 << 12);
    }
}
