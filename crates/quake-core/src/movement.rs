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
/// Player hull height (`-24..32`), the lift for the "open above" probe.
const PLAYER_HULL_HEIGHT_Q12: i32 = 56 << 12;
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
const AIR_GROUND_CUTOFF_Q12: i32 = 180 << 12;
const LAND_SPEED_Q12: i32 = -(300 << 12);
const HARD_LAND_SPEED_Q12: i32 = -(650 << 12);
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
    grounded: bool,
    water_level: u8,
    water_type: i16,
    jump_latched: bool,
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
            jump_latched: false,
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
    /// player with the destination's `v_forward * 300` push.
    pub fn teleport_with_velocity(&mut self, origin: Vec3I32, velocity: Vec3I32) {
        *self = Self::new(origin);
        self.velocity = velocity;
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
        // `SV_Physics_Client` runs `SV_CheckVelocity` before the move.
        self.velocity.x = self.velocity.x.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.velocity.y = self.velocity.y.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.velocity.z = self.velocity.z.clamp(-MAX_VELOCITY_Q12, MAX_VELOCITY_Q12);
        self.check_stuck(collision, scratch);
        self.categorize_position(collision, scratch);
        if self.grounded {
            self.steep_seam_recovered = false;
        }
        self.refresh_water(scratch, point_contents, &mut events);
        let started_grounded = self.grounded;

        // `SV_WaterJump`: the flag clears on `teleport_time` or on leaving
        // the water; while set, the climb owns the velocity and the player's
        // own input is ignored (`SV_ClientThink` returns early).
        if self.water_jump_ticks != 0 {
            if self.water_level == 0 {
                self.water_jump_ticks = 0;
            } else {
                self.water_jump_ticks -= 1;
            }
        }
        if self.water_jump_ticks != 0 {
            self.velocity.x = self.water_jump_push.x;
            self.velocity.y = self.water_jump_push.y;
            let _ = fly_move(collision, scratch, &mut self.origin, &mut self.velocity);
            self.categorize_position(collision, scratch);
            self.refresh_water(scratch, point_contents, &mut events);
            return events;
        }
        // `PlayerPreThink`: `if (self.waterlevel == 2) CheckWaterJump();`
        if self.water_level == 2 && self.check_water_jump(collision, scratch, input.yaw) {
            self.grounded = false;
            self.velocity.z = WATER_JUMP_OUT_SPEED_Q12;
            self.jump_latched = true;
            self.water_jump_ticks = WATER_JUMP_TICKS;
        }

        if input.jump {
            if self.water_level >= 2 {
                self.grounded = false;
                self.velocity.z = match self.water_type {
                    CONTENTS_SLIME => SLIME_JUMP_SPEED_Q12,
                    CONTENTS_LAVA => LAVA_JUMP_SPEED_Q12,
                    _ => WATER_JUMP_SPEED_Q12,
                };
                if !self.jump_latched {
                    events.insert(MovementEvents::SWIM);
                }
            } else if self.grounded && !self.jump_latched {
                self.grounded = false;
                self.velocity.z = self.velocity.z.saturating_add(JUMP_SPEED_Q12);
                events.insert(MovementEvents::JUMP);
            }
            self.jump_latched = true;
        } else {
            self.jump_latched = false;
        }

        let impact_velocity;
        if self.water_level >= 2 {
            apply_friction(&mut self.velocity, self.water_level);
            impact_velocity = self.velocity.z;
            self.water_move(collision, scratch, input);
        } else {
            if self.grounded {
                apply_friction(&mut self.velocity, 0);
            }
            let (wish_direction, wish_speed) = wish_velocity(input);
            if self.grounded {
                accelerate(&mut self.velocity, wish_direction, wish_speed, false);
                self.velocity.z = 0;
                impact_velocity = 0;
                self.ground_move(collision, scratch);
            } else {
                accelerate(&mut self.velocity, wish_direction, wish_speed, true);
                self.velocity.z = self.velocity.z.saturating_sub(gravity_per_tick_q12);
                impact_velocity = self.velocity.z;
                let original_origin = self.origin;
                let original_velocity = self.velocity;
                let clip = fly_move(collision, scratch, &mut self.origin, &mut self.velocity);
                // The captured E1M1 seam leaves the player technically
                // airborne on a steep bevel, so the grounded
                // `SV_TryUnstick` call cannot run. Retry from the last valid
                // pre-impact origin only when the frame made less than 1/8
                // unit of progress and a short floor probe confirms a steep
                // support plane. That distinguishes the fixed-point seam
                // from an ordinary airborne collision with a vertical wall.
                if clip != 0
                    && abs_i32(self.origin.x.saturating_sub(original_origin.x)) < Q12_ONE / 8
                    && abs_i32(self.origin.y.saturating_sub(original_origin.y)) < Q12_ONE / 8
                    && !self.steep_seam_recovered
                    && on_steep_support(collision, original_origin, scratch)
                {
                    let mut recovered_origin = original_origin;
                    let mut recovered_velocity = original_velocity;
                    if try_steep_seam_lift(
                        collision,
                        scratch,
                        &mut recovered_origin,
                        &mut recovered_velocity,
                        original_velocity,
                    ) {
                        self.origin = recovered_origin;
                        self.velocity = recovered_velocity;
                        self.steep_seam_recovered = true;
                    }
                }
            }
        }

        self.categorize_position(collision, scratch);
        self.refresh_water(scratch, point_contents, &mut events);
        if !started_grounded && self.grounded && impact_velocity < LAND_SPEED_Q12 {
            // `PlayerPostThink` swaps the landing sound for a splash only on
            // `self.watertype == CONTENT_WATER`; slime and lava landings still
            // take the fall.
            if self.water_level > 0 && self.water_type == CONTENTS_WATER {
                events.insert(MovementEvents::WATER_LAND);
            } else if impact_velocity < HARD_LAND_SPEED_Q12 {
                events.insert(MovementEvents::HARD_LAND);
            } else {
                events.insert(MovementEvents::LAND);
            }
        }
        events
    }

    /// `CheckWaterJump`: solid 24 units ahead at the waist but open at the
    /// top of the box means a ledge the player can hop onto. The original
    /// uses two point traces; this sweeps the hull forward at the origin and
    /// again lifted by its own height so the "open above" test asks for the
    /// standing room the player needs anyway.
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
        let waist_end = add(self.origin, forward);
        trace(collision, &self.origin, &waist_end, scratch);
        if scratch.trace.trapped() || scratch.trace.fraction >= Q12_ONE {
            return false;
        }
        let normal = scratch.trace.normal;
        let lifted = Vec3I32 {
            x: self.origin.x,
            y: self.origin.y,
            z: self.origin.z.saturating_add(PLAYER_HULL_HEIGHT_Q12),
        };
        if position_stuck(collision, lifted, scratch) {
            return false;
        }
        let lifted_end = add(lifted, forward);
        trace(collision, &lifted, &lifted_end, scratch);
        if scratch.trace.trapped() || scratch.trace.fraction < Q12_ONE {
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

    fn categorize_position<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
    ) {
        if self.velocity.z > AIR_GROUND_CUTOFF_Q12 {
            self.grounded = false;
            return;
        }
        let below = Vec3I32 {
            x: self.origin.x,
            y: self.origin.y,
            z: self.origin.z.saturating_sub(GROUND_PROBE_Q12),
        };
        // Standing on a surface is exactly the contact the tracer reports as
        // `all_solid` with a zero fraction and the floor's normal, so the ground
        // probe asks the same "trapped" question the slide move does.
        trace(collision, &self.origin, &below, scratch);
        self.grounded = !scratch.trace.trapped()
            && scratch.trace.fraction < Q12_ONE
            && scratch.trace.normal.z as i32 >= WALKABLE_NORMAL_Q12;
        if self.grounded {
            self.origin = scratch.trace.end;
            if self.velocity.z < 0 {
                self.velocity.z = 0;
            }
        }
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

    fn water_move<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
        input: MovementInput,
    ) {
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
        accelerate(&mut self.velocity, direction, speed, false);
        let _ = fly_move(collision, scratch, &mut self.origin, &mut self.velocity);
    }

    fn ground_move<C: MovementTrace + ?Sized>(
        &mut self,
        collision: &C,
        scratch: &mut MovementScratch,
    ) {
        if self.velocity.x == 0 && self.velocity.y == 0 {
            return;
        }
        let destination = add(self.origin, frame_displacement(self.velocity));
        trace(collision, &self.origin, &destination, scratch);
        if !scratch.trace.trapped() && scratch.trace.fraction == Q12_ONE {
            self.origin = scratch.trace.end;
            return;
        }

        let original_origin = self.origin;
        let original_velocity = self.velocity;
        let mut down_origin = self.origin;
        let mut down_velocity = self.velocity;
        let _ = fly_move(collision, scratch, &mut down_origin, &mut down_velocity);

        let step_target = Vec3I32 {
            x: original_origin.x,
            y: original_origin.y,
            z: original_origin.z.saturating_add(STEP_HEIGHT_Q12),
        };
        trace(collision, &original_origin, &step_target, scratch);
        // `SV_PushEntity` leaves the origin where it started when the upward
        // push is all-solid, but `SV_WalkMove` still attempts the forward
        // slide and its precision-seam recovery from there. Returning early
        // here bypassed the very `SV_TryUnstick` path this branch exists for.
        let mut up_origin = if scratch.trace.trapped() {
            original_origin
        } else {
            scratch.trace.end
        };
        let mut up_velocity = original_velocity;
        let up_clip = fly_move(collision, scratch, &mut up_origin, &mut up_velocity);
        // `SV_WalkMove` detects the precision seam where stepping up and
        // sliding made effectively no horizontal progress, then retries after
        // a two-unit nudge in each axial/diagonal direction. Without this,
        // E1M1's hull can confine the player to a 24-unit wedge forever.
        if up_clip != 0
            && abs_i32(up_origin.x.saturating_sub(original_origin.x)) < Q12_ONE / 32
            && abs_i32(up_origin.y.saturating_sub(original_origin.y)) < Q12_ONE / 32
        {
            let _ = try_unstick(
                collision,
                scratch,
                &mut up_origin,
                &mut up_velocity,
                original_velocity,
            );
        }
        let step_down_target = Vec3I32 {
            x: up_origin.x,
            y: up_origin.y,
            z: up_origin.z.saturating_sub(STEP_HEIGHT_Q12),
        };
        trace(collision, &up_origin, &step_down_target, scratch);
        if scratch.trace.trapped()
            || scratch.trace.fraction == Q12_ONE
            || (scratch.trace.normal.z as i32) < WALKABLE_NORMAL_Q12
        {
            self.origin = down_origin;
            self.velocity = down_velocity;
            return;
        }
        up_origin = scratch.trace.end;

        if horizontal_distance_squared(original_origin, down_origin)
            > horizontal_distance_squared(original_origin, up_origin)
        {
            self.origin = down_origin;
            self.velocity = down_velocity;
        } else {
            self.origin = up_origin;
            self.velocity = Vec3I32 {
                x: up_velocity.x,
                y: up_velocity.y,
                z: down_velocity.z,
            };
        }
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

fn wish_velocity(input: MovementInput) -> (Vec3I32, i32) {
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
    let sin = psx_math::sin_q12(yaw);
    let cos = psx_math::cos_q12(yaw);
    let direction = Vec3I32 {
        x: mul_q12_i32(cos, forward_q12).saturating_sub(mul_q12_i32(sin, strafe_q12)),
        y: mul_q12_i32(sin, forward_q12).saturating_add(mul_q12_i32(cos, strafe_q12)),
        z: 0,
    };
    let speed = MAX_SPEED_Q12.saturating_mul(magnitude.min(127)) / 127;
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

fn accelerate(velocity: &mut Vec3I32, direction: Vec3I32, wish_speed: i32, air: bool) {
    if wish_speed <= 0 {
        return;
    }
    let target_speed = if air {
        wish_speed.min(30 << 12)
    } else {
        wish_speed
    };
    let current_speed = dot(*velocity, direction);
    let add_speed = target_speed.saturating_sub(current_speed);
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

fn apply_friction(velocity: &mut Vec3I32, water_level: u8) {
    let source = if water_level >= 2 {
        *velocity
    } else {
        Vec3I32 {
            x: velocity.x,
            y: velocity.y,
            z: 0,
        }
    };
    let speed = vector_length(source);
    if speed < Q12_ONE {
        velocity.x = 0;
        velocity.y = 0;
        if water_level >= 2 {
            velocity.z = 0;
        }
        return;
    }
    let drop = if water_level >= 2 {
        // `SV_WaterMove`: `speed - host_frametime * speed * sv_friction`,
        // with no waterlevel term.
        speed / 15
    } else {
        speed.max(STOP_SPEED_Q12) / 15
    };
    let new_speed = speed.saturating_sub(drop).max(0);
    let scale = div_q12_i32(new_speed, speed);
    velocity.x = mul_q12_i32(velocity.x, scale);
    velocity.y = mul_q12_i32(velocity.y, scale);
    velocity.z = mul_q12_i32(velocity.z, scale);
}

fn fly_move<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
) -> u8 {
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
) -> u8 {
    let original_velocity = *velocity;
    let primal_velocity = *velocity;
    let mut planes = [Vec3I16 { x: 0, y: 0, z: 0 }; MAX_CLIP_PLANES];
    let mut plane_count = 0usize;
    let mut blocked = 0u8;

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
            return 3;
        }
        if scratch.trace.fraction > 0 {
            *origin = scratch.trace.end;
            plane_count = 0;
        }
        if scratch.trace.fraction == Q12_ONE {
            break;
        }
        if scratch.trace.normal.z as i32 > WALKABLE_NORMAL_Q12 {
            blocked |= 1;
        }
        if scratch.trace.normal.z == 0 {
            blocked |= 2;
        }
        time_left = mul_q12_i32(time_left, Q12_ONE.saturating_sub(scratch.trace.fraction));
        if plane_count == MAX_CLIP_PLANES {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            break;
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
            Vec3I32 { x: 0, y: 0, z: 0 }
        };
        if dot(*velocity, primal_velocity) <= 0 {
            *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
            break;
        }
    }
    blocked
}

/// Quake's `SV_TryUnstick`, in fixed point. `SV_WalkMove` invokes it only
/// after the raised slide was blocked without moving even 1/32 of a unit.
/// Each two-unit nudge is traced, then the original velocity gets a 0.1-second
/// retry. A result counts only after it escapes by more than four units.
fn try_unstick<C: MovementTrace + ?Sized>(
    collision: &C,
    scratch: &mut MovementScratch,
    origin: &mut Vec3I32,
    velocity: &mut Vec3I32,
    original_velocity: Vec3I32,
) -> bool {
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
        let _ = fly_move_for_time(
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
            return true;
        }
    }

    *origin = stuck_origin;
    *velocity = Vec3I32 { x: 0, y: 0, z: 0 };
    false
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

fn clip_velocity(velocity: Vec3I32, normal: Vec3I16) -> Vec3I32 {
    let backoff = dot_normal(velocity, normal);
    Vec3I32 {
        x: velocity
            .x
            .saturating_sub(mul_q12_i32(backoff, normal.x as i32)),
        y: velocity
            .y
            .saturating_sub(mul_q12_i32(backoff, normal.y as i32)),
        z: velocity
            .z
            .saturating_sub(mul_q12_i32(backoff, normal.z as i32)),
    }
}

fn crease_velocity(velocity: Vec3I32, left: Vec3I16, right: Vec3I16) -> Vec3I32 {
    let direction = Vec3I32 {
        x: mul_q12_i32(left.y as i32, right.z as i32)
            .saturating_sub(mul_q12_i32(left.z as i32, right.y as i32)),
        y: mul_q12_i32(left.z as i32, right.x as i32)
            .saturating_sub(mul_q12_i32(left.x as i32, right.z as i32)),
        z: mul_q12_i32(left.x as i32, right.y as i32)
            .saturating_sub(mul_q12_i32(left.y as i32, right.x as i32)),
    };
    let direction_length = vector_length(direction);
    if direction_length == 0 {
        return Vec3I32 { x: 0, y: 0, z: 0 };
    }
    let unit = Vec3I32 {
        x: div_q12_i32(direction.x, direction_length),
        y: div_q12_i32(direction.y, direction_length),
        z: div_q12_i32(direction.z, direction_length),
    };
    scale(unit, dot(velocity, unit))
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

fn frame_displacement(velocity: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: velocity.x / VIDEO_TICKS_PER_SECOND,
        y: velocity.y / VIDEO_TICKS_PER_SECOND,
        z: velocity.z / VIDEO_TICKS_PER_SECOND,
    }
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

fn horizontal_distance_squared(start: Vec3I32, end: Vec3I32) -> i32 {
    let dx = (end.x.saturating_sub(start.x)) >> 12;
    let dy = (end.y.saturating_sub(start.y)) >> 12;
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dy))
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

    #[test]
    fn swimming_forward_follows_the_view_pitch() {
        // Looking up 45 degrees (negative pitch in Quake's turn) and holding
        // forward must rise, level must not, and looking down must sink
        // faster than the idle drift; the horizontal speed matches the level
        // wish scaled by cos(pitch).
        fn swim(pitch: u16, ticks: usize) -> (Vec3I32, Vec3I32) {
            let mut state = MovementState::new(Vec3I32 {
                x: 0,
                y: 0,
                z: 256 << 12,
            });
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
            let mut state = MovementState::new(Vec3I32 {
                x: 0,
                y: 0,
                z: 256 << 12,
            });
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
            (up_velocity.x - down_velocity.x).abs() <= 16,
            "{} vs {}",
            up_velocity.x,
            down_velocity.x
        );
        assert!(up_velocity.z.abs() <= up_velocity.x + (1 << 12));
    }

    #[test]
    fn held_jump_only_launches_once_until_released() {
        let floor = Floor;
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
        let input = MovementInput {
            jump: true,
            ..MovementInput::default()
        };
        let mut scratch = MovementScratch::default();
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
        let mut state = MovementState::new(Vec3I32 {
            x: 0,
            y: 0,
            z: 1 << 12,
        });
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
}
