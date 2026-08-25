//! Fixed-capacity presentation effects driven by gameplay events.
//!
//! These are deliberately not entities and never participate in collision or
//! damage. The guest owns one small pool and the renderer consumes snapshots;
//! spawning an explosion therefore cannot allocate or perturb simulation.

use quake_formats::{Vec3I16, Vec3I32};

/// Maximum simultaneous explosion flashes retained by the guest.
pub const MAX_EXPLOSION_EFFECTS: usize = 4;
/// Lifetime at the fixed 60 Hz simulation clock: three tenths of a second.
pub const EXPLOSION_EFFECT_TICKS: u16 = 18;
/// Maximum individual world particles retained by the guest.
///
/// The worst frame is a rocket in flight (two trail particles per frame over
/// the twelve ticks one lives, so about ten), its explosion ring (twelve), a
/// blood hit (eight) and gib chunks trailing blood (about six). Forty slots
/// covers that sum, so an explosion never evicts the blood that made it.
///
/// This is a drawing budget as much as a capacity: every live particle is one
/// projection and one flat rect in a frame that already sits on the two-vblank
/// cliff, so the pool is deliberately smaller than the sum of its callers'
/// appetites and the oldest particle loses.
pub const MAX_IMPACT_PARTICLES: usize = 40;
/// Retained Quake model ID for `progs/s_bubble.spr`.
pub const BUBBLE_SPRITE_MODEL_ID: i16 = 0x5d;
/// Blood droplets live for one fifth of a second at the 60 Hz simulation clock.
pub const IMPACT_PARTICLE_TICKS: u8 = 12;
/// Particles one `spawn_trail` call may emit however far the projectile flew.
const MAX_TRAIL_PARTICLES: i32 = 2;
/// Radius of the explosion ring, which starts where the renderer's expanding
/// star does so the two read as one burst.
pub const EXPLOSION_RING_UNITS: i32 = 16;

/// Live `cl_dlights` this port keeps at once.
///
/// The original carries thirty-two. This is ONE, and the number is a budget
/// rather than a shape: every live light is a box test against each world
/// face the frame already selected and a distance per corner of the faces it
/// reaches, and a second slot costs about a third of a kilobyte of image
/// (every loop over the array unrolls) on a build with under two kilobytes of
/// heap to spare. `push` below decides which single light that is.
pub const MAX_DYNAMIC_LIGHTS: usize = 1;
/// `TE_EXPLOSION`'s light: `dl->radius = 350`.
pub const EXPLOSION_LIGHT_RADIUS_UNITS: i16 = 350;
/// `dl->die = cl.time + 0.5`, at the fixed 60 Hz simulation clock.
const EXPLOSION_LIGHT_TICKS: u8 = 30;
/// `dl->decay = 300` units a second, at that same clock.
const EXPLOSION_LIGHT_DECAY: i16 = 5;
/// `MUZZLEFLASH`: `dl->radius = 200 + (rand()&31)`. The random lift is
/// dropped; at this falloff it moves no vertex by a whole light step.
const MUZZLE_LIGHT_RADIUS_UNITS: i16 = 200;
/// `dl->die = cl.time + 0.1`, and `MUZZLEFLASH` sets no decay.
const MUZZLE_LIGHT_TICKS: u8 = 6;

/// Colour ramp and physics class of one pooled particle.
///
/// The original carries a palette ramp and a `pt_` movement type per
/// particle; the port keeps only the five the gameplay events below use.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ParticleKind {
    /// `pt_grav` blood: falls, red ramp.
    Blood,
    /// The hot end of `ramp3`: the rocket trail and the explosion burst.
    Fire,
    /// The grey end of `ramp3`, which is where the grenade trail starts.
    Smoke,
    /// `R_TeleportSplash`'s pale ramp.
    Spark,
    /// Camera-facing `progs/s_bubble.spr` released by a submerged death.
    Bubble,
}

impl ParticleKind {
    /// Lifetime on the 60 Hz simulation clock. The original gives trails two
    /// whole seconds; at three-unit spacing that is hundreds of live
    /// particles, so the port keeps every kind under a third of a second.
    const fn life_ticks(self) -> u8 {
        match self {
            Self::Blood => IMPACT_PARTICLE_TICKS,
            Self::Fire => 12,
            Self::Smoke => 15,
            Self::Spark => 12,
            Self::Bubble => 120,
        }
    }

    /// Only blood falls; trail and splash particles hold their drift, which
    /// keeps the integrator to one add per axis.
    const fn falls(self) -> bool {
        matches!(self, Self::Blood)
    }

    /// Per-tick drift given to a trail particle, in the pool's Q8 units.
    const fn trail_drift(self) -> Vec3I16 {
        match self {
            // `R_RocketTrail` leaves the trail motionless; fire and smoke get
            // a slow rise so a corridor of them does not read as a dotted line.
            Self::Fire | Self::Smoke => Vec3I16 { x: 0, y: 0, z: 64 },
            _ => Vec3I16 { x: 0, y: 0, z: 0 },
        }
    }
}

/// One fixed-capacity world-space particle.
///
/// Velocity is Q8 world units per tick rather than the Q12 the rest of the
/// port carries: half a unit of gravity and four units a tick of blood both
/// fit an `i16`, which keeps a slot at twenty-four bytes even though the pool
/// grew a kind and a third more slots.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImpactParticle {
    /// Current Q20.12 position in Quake world axes.
    pub origin: Vec3I32,
    velocity: Vec3I16,
    age_ticks: u8,
    kind: ParticleKind,
    active: bool,
}

impl ImpactParticle {
    const EMPTY: Self = Self {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        velocity: Vec3I16 { x: 0, y: 0, z: 0 },
        age_ticks: 0,
        kind: ParticleKind::Blood,
        active: false,
    };

    /// The ramp this particle's kind fades along, without alpha blending.
    ///
    /// One table lookup and one divide, not a ramp per kind: this runs once
    /// per drawn particle inside the frame loop, which is I-cache bound.
    pub const fn color(self) -> (u8, u8, u8) {
        /// Newest colour of each kind's ramp, faded to black by its age.
        const RAMP: [(u8, u8, u8); 5] = [
            (0x90, 0x12, 0x09),
            (0xf0, 0x96, 0x1e),
            (0x80, 0x80, 0x70),
            (0xb4, 0xb4, 0xf0),
            (0x80, 0x80, 0x80),
        ];
        let life = self.kind.life_ticks();
        let remaining = life.saturating_sub(self.age_ticks) as u16;
        let fade = (remaining << 6) / life as u16;
        let (red, green, blue) = RAMP[self.kind as usize];
        (
            ((red as u16 * fade) >> 6) as u8,
            ((green as u16 * fade) >> 6) as u8,
            ((blue as u16 * fade) >> 6) as u8,
        )
    }

    /// Particle size in screen pixels after projection.
    pub const fn size(self) -> i16 {
        if self.age_ticks < self.kind.life_ticks() / 2 {
            2
        } else {
            1
        }
    }

    pub const fn is_bubble(self) -> bool {
        matches!(self.kind, ParticleKind::Bubble)
    }

    pub const fn bubble_frame(self) -> usize {
        if self.age_ticks >= self.kind.life_ticks() / 2 {
            1
        } else {
            0
        }
    }
}

/// Twelve points around a circle, components scaled by sixteen. Bursts and
/// splash rings both walk it, so the port carries one direction table.
const RING: [(i8, i8); 12] = [
    (16, 0),
    (14, 8),
    (8, 14),
    (0, 16),
    (-8, 14),
    (-14, 8),
    (-16, 0),
    (-14, -8),
    (-8, -14),
    (0, -16),
    (8, -14),
    (14, -8),
];

/// Lateral offset in whole units of each death bubble, which doubles as its
/// sideways drift. A fixed fan stands in for `bubble_bob`'s wander.
const BUBBLE_FAN: [(i8, i8); 5] = [(0, 0), (5, -4), (-4, 5), (4, 4), (-5, -3)];

/// Whole-unit stand-in for `rand()%6-3`, walked by the pool's write cursor so
/// a trail scatters without a random source.
const JITTER: [i8; 8] = [-3, 2, -1, 3, 0, -2, 1, 2];

/// Cheap 3D length, max plus a third of the other two. Good to about fifteen
/// percent, which is finer than the particle spacing it feeds.
fn approx_length(delta: Vec3I32) -> i32 {
    let x = delta.x.abs();
    let y = delta.y.abs();
    let z = delta.z.abs();
    let longest = x.max(y).max(z);
    longest + (x + y + z - longest) / 3
}

/// Reused fixed pool for every deterministic world particle: weapon-hit
/// blood, projectile trails, explosion bursts and the two splashes. One pool
/// means one draw loop and one bound on how much a bad frame can cost.
pub struct ImpactParticles {
    slots: [ImpactParticle; MAX_IMPACT_PARTICLES],
    next: usize,
    map_token: u32,
    map_known: bool,
}

impl ImpactParticles {
    /// Empty pool before the first map is entered.
    pub const fn new() -> Self {
        Self {
            slots: [ImpactParticle::EMPTY; MAX_IMPACT_PARTICLES],
            next: 0,
            map_token: 0,
            map_known: false,
        }
    }

    /// Drop particles when a new gameplay session becomes active.
    pub fn enter_map(&mut self, token: u32) {
        if !self.map_known || self.map_token != token {
            self.slots = [ImpactParticle::EMPTY; MAX_IMPACT_PARTICLES];
            self.next = 0;
            self.map_token = token;
            self.map_known = true;
        }
    }

    /// Spawn the original gameplay distinction for a damageable hit: blood,
    /// never the wall-impact spark used by non-damageable geometry.
    pub fn spawn_blood(&mut self, origin: Vec3I32) {
        const VELOCITIES: [(i16, i16, i16); 8] = [
            (-2, -1, 4),
            (2, 1, 4),
            (-1, 2, 3),
            (1, -2, 3),
            (-3, 1, 2),
            (3, -1, 2),
            (1, 3, 3),
            (-1, -3, 3),
        ];
        for (x, y, z) in VELOCITIES {
            self.push(
                origin,
                Vec3I16 {
                    x: x << 8,
                    y: y << 8,
                    z: z << 8,
                },
                ParticleKind::Blood,
            );
        }
    }

    /// `R_RocketTrail`, decimated. The original walks the segment in 3-unit
    /// steps with a 2-second life, which is hundreds of live particles per
    /// projectile. This takes one particle every `step_units` of travel,
    /// caps a single call at two, and returns the anchor the caller must
    /// store, so spacing follows distance flown rather than frame rate.
    pub fn spawn_trail(
        &mut self,
        from: Vec3I32,
        to: Vec3I32,
        kind: ParticleKind,
        step_units: i32,
    ) -> Vec3I32 {
        let delta = Vec3I32 {
            x: to.x - from.x,
            y: to.y - from.y,
            z: to.z - from.z,
        };
        let emitted = (approx_length(delta) / (step_units << 12)).min(MAX_TRAIL_PARTICLES);
        if emitted == 0 {
            // Not far enough yet: keep the anchor so the travel accumulates.
            return from;
        }
        let drift = kind.trail_drift();
        for index in 1..=emitted {
            let jitter = (JITTER[(self.next + index as usize) & 7] as i32) << 12;
            self.push(
                Vec3I32 {
                    x: from.x + delta.x * index / emitted + jitter,
                    y: from.y + delta.y * index / emitted + jitter,
                    z: from.z + delta.z * index / emitted,
                },
                drift,
                kind,
            );
        }
        to
    }

    /// The one decimated ring behind `R_ParticleExplosion`, `R_LavaSplash`
    /// and `R_TeleportSplash`. The originals spawn 1024, 1024 and 896
    /// particles from nested loops; on a 320-wide framebuffer the shape that
    /// survives is the outward ring, so all three share it and differ only in
    /// radius and ramp.
    ///
    /// Cold and never inlined: these fire on an explosion or a teleport, and
    /// the guest's frame loop pays for every byte of I-cache it does not use.
    #[cold]
    #[inline(never)]
    pub fn spawn_ring(&mut self, origin: Vec3I32, kind: ParticleKind, radius_units: i32) {
        for (index, (x, y)) in RING.iter().enumerate() {
            // Alternating lift turns the flat ring into a rough shell.
            let lift = if index % 2 == 0 { 12 } else { -8 };
            let scale = |component: i8| ((component as i32) * radius_units) >> 4;
            self.push(
                Vec3I32 {
                    x: origin.x + (scale(*x) << 12),
                    y: origin.y + (scale(*y) << 12),
                    z: origin.z + (lift << 12),
                },
                Vec3I16 {
                    x: (*x as i16) << 4,
                    y: (*y as i16) << 4,
                    z: (lift as i16) << 5,
                },
                kind,
            );
        }
    }

    /// Fixed-pool implementation of Quake's `DeathBubbles`.
    ///
    /// id1 calls `DeathBubbles(20)` from `DeathSound` (client.qc), which
    /// `PlayerDie` only reaches on the non-gib path, and only when the corpse
    /// is at `waterlevel == 3`. It spawns a bubble spawner that emits twenty
    /// `progs/s_bubble.spr` entities over two seconds, each one bobbing up
    /// under `bubble_bob` (misc.qc) and popping when it breaks the surface.
    ///
    /// The renderer now draws the original two-frame sprite. Five bounded
    /// slots are released in one burst rather than twenty over two seconds,
    /// with a fixed lateral fan standing in for `bubble_bob`'s wander. They do
    /// not test the water surface or split on contact, but they keep rising for
    /// two seconds and no longer substitute flat particles for the artwork.
    ///
    /// This is also NOT the `air_bubbles` map entity. That one is `remove
    /// (self)` in misc.qc, so the two E1M4 authors are no-ops in the original
    /// too and are deliberately left unimplemented.
    ///
    /// Cold and never inlined: it runs once per drowning death.
    #[cold]
    #[inline(never)]
    pub fn spawn_death_bubbles(&mut self, origin: Vec3I32) {
        let mut index = 0usize;
        while index < BUBBLE_FAN.len() {
            self.push_bubble(origin, index);
            index += 1;
        }
    }

    /// One bubble of the burst above, out of line on purpose: `push` writes a
    /// twenty-four byte slot and wraps the cursor, so five inlined copies of
    /// it cost about four hundred bytes of image that a build with under two
    /// kilobytes of heap to spare cannot pay for. One call each instead.
    #[cold]
    #[inline(never)]
    fn push_bubble(&mut self, origin: Vec3I32, index: usize) {
        /// `bubble_spawner.origin_z = origin_z + 24`, the one number this
        /// keeps from the original.
        const SPAWN_HEIGHT_UNITS: i32 = 24;
        /// Base rise in the pool's Q8 units per tick, about twenty-two units
        /// a second, which is the band `bubble_bob` randomises inside.
        const RISE: i16 = 96;
        let (x, y) = BUBBLE_FAN[index];
        self.push(
            Vec3I32 {
                x: origin.x + ((x as i32) << 12),
                y: origin.y + ((y as i32) << 12),
                z: origin.z + (SPAWN_HEIGHT_UNITS << 12),
            },
            Vec3I16 {
                x: (x as i16) << 2,
                y: (y as i16) << 2,
                z: RISE + ((index as i16) << 3),
            },
            ParticleKind::Bubble,
        );
    }

    /// Claim the next slot, replacing the oldest particle once full.
    ///
    /// Out of line on purpose: every spawner calls this in a fixed-count loop
    /// that LLVM unrolls, so inlining it copied the whole body per particle.
    #[inline(never)]
    fn push(&mut self, origin: Vec3I32, velocity: Vec3I16, kind: ParticleKind) {
        self.slots[self.next] = ImpactParticle {
            origin,
            velocity,
            age_ticks: 0,
            kind,
            active: true,
        };
        self.next = (self.next + 1) % self.slots.len();
    }

    /// Integrate the bounded pool on the same fixed clock as gameplay.
    pub fn tick(&mut self, elapsed_ticks: u16) {
        let ticks = elapsed_ticks.min(u8::MAX as u16) as u8;
        for slot in &mut self.slots {
            if !slot.active {
                continue;
            }
            let step = i32::from(ticks);
            // Q8 velocity into a Q12 origin: four bits of shift per axis.
            let travel = |velocity: i16| (i32::from(velocity) << 4).saturating_mul(step);
            slot.origin.x = slot.origin.x.saturating_add(travel(slot.velocity.x));
            slot.origin.y = slot.origin.y.saturating_add(travel(slot.velocity.y));
            slot.origin.z = slot.origin.z.saturating_add(travel(slot.velocity.z));
            if slot.kind.falls() {
                // `sv_gravity` at the pool's scale: half a unit a tick.
                slot.velocity.z = slot
                    .velocity
                    .z
                    .saturating_sub(128i16.saturating_mul(ticks as i16));
            }
            slot.age_ticks = slot.age_ticks.saturating_add(ticks);
            if slot.age_ticks >= slot.kind.life_ticks() {
                slot.active = false;
            }
        }
    }

    /// Active particles in stable slot order, without allocating.
    pub fn active(&self) -> impl Iterator<Item = ImpactParticle> + '_ {
        self.slots.iter().copied().filter(|slot| slot.active)
    }
}

impl Default for ImpactParticles {
    fn default() -> Self {
        Self::new()
    }
}

/// One expanding impact flash.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExplosionEffect {
    /// World-space Q20.12 origin reported by the damage path.
    pub origin: Vec3I32,
    age_ticks: u16,
    active: bool,
}

impl ExplosionEffect {
    const EMPTY: Self = Self {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        age_ticks: 0,
        active: false,
    };

    /// Whole simulation ticks elapsed since the impact.
    pub const fn age_ticks(self) -> u16 {
        self.age_ticks
    }

    /// Expanding star radius in Quake world units.
    pub const fn radius_units(self) -> i32 {
        8 + self.age_ticks as i32 * 2
    }

    /// Warm color which fades without alpha blending.
    pub const fn color(self) -> (u8, u8, u8) {
        let remaining = EXPLOSION_EFFECT_TICKS.saturating_sub(self.age_ticks) as u32;
        let fade = (remaining * 128 / EXPLOSION_EFFECT_TICKS as u32) as u8;
        (fade, ((fade as u16 * 3) / 4) as u8, fade / 4)
    }
}

/// Reused ring of short-lived explosion effects.
pub struct ExplosionEffects {
    slots: [ExplosionEffect; MAX_EXPLOSION_EFFECTS],
    next: usize,
    map_token: u32,
    map_known: bool,
}

impl ExplosionEffects {
    /// Empty pool before the first map is entered.
    pub const fn new() -> Self {
        Self {
            slots: [ExplosionEffect::EMPTY; MAX_EXPLOSION_EFFECTS],
            next: 0,
            map_token: 0,
            map_known: false,
        }
    }

    /// Drop effects when a different resident map becomes active.
    pub fn enter_map(&mut self, token: u32) {
        if !self.map_known || self.map_token != token {
            self.slots = [ExplosionEffect::EMPTY; MAX_EXPLOSION_EFFECTS];
            self.next = 0;
            self.map_token = token;
            self.map_known = true;
        }
    }

    /// Retain an impact, replacing the oldest slot once the pool is full.
    pub fn spawn(&mut self, origin: Vec3I32) {
        self.slots[self.next] = ExplosionEffect {
            origin,
            age_ticks: 0,
            active: true,
        };
        self.next = (self.next + 1) % self.slots.len();
    }

    /// Advance every active effect on the fixed simulation clock.
    pub fn tick(&mut self, elapsed_ticks: u16) {
        for slot in &mut self.slots {
            if !slot.active {
                continue;
            }
            slot.age_ticks = slot.age_ticks.saturating_add(elapsed_ticks);
            if slot.age_ticks >= EXPLOSION_EFFECT_TICKS {
                slot.active = false;
            }
        }
    }

    /// Active effects in stable slot order, without allocating.
    pub fn active(&self) -> impl Iterator<Item = ExplosionEffect> + '_ {
        self.slots.iter().copied().filter(|slot| slot.active)
    }
}

impl Default for ExplosionEffects {
    fn default() -> Self {
        Self::new()
    }
}

/// One live `cl_dlights` entry: a point, a radius that may decay, and a death
/// tick. That is the whole of `CL_NewDlight`'s state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DynamicLight {
    /// Origin in WHOLE Quake world units, not the Q20.12 the rest of the
    /// port carries. A light never moves, and everything that reads one (face
    /// bounds, materialized vertex positions, entity origins) compares in
    /// whole units, so the shift happens once here instead of per frame.
    pub origin: Vec3I16,
    radius_units: i16,
    decay_per_tick: i16,
    remaining_ticks: u8,
    active: bool,
}

impl DynamicLight {
    /// A slot with no light in it, which is also what a renderer holds before
    /// the first frame that has one.
    pub const DARK: Self = Self {
        origin: Vec3I16 { x: 0, y: 0, z: 0 },
        radius_units: 0,
        decay_per_tick: 0,
        remaining_ticks: 0,
        active: false,
    };

    /// Current radius in whole Quake world units, after this frame's decay.
    pub const fn radius_units(self) -> i32 {
        self.radius_units as i32
    }
}

/// The port's `cl_dlights`.
///
/// This holds the light, not its effect: the renderer decides what a light
/// reaches, exactly as `R_RenderDlights` does over the same array.
pub struct DynamicLights {
    slots: [DynamicLight; MAX_DYNAMIC_LIGHTS],
    map_token: u32,
    map_known: bool,
}

impl DynamicLights {
    /// Dark before the first map is entered.
    pub const fn new() -> Self {
        Self {
            slots: [DynamicLight::DARK; MAX_DYNAMIC_LIGHTS],
            map_token: 0,
            map_known: false,
        }
    }

    /// Drop every light when a new gameplay session becomes active.
    ///
    /// Every method below is kept out of line on purpose. They are all reached
    /// from `quake::run`, which is one inlined eighty-kilobyte body whose
    /// register allocation is worth more than the call: inlined, this pool's
    /// five short methods cost two kilobytes of image between them.
    #[inline(never)]
    pub fn enter_map(&mut self, token: u32) {
        if !self.map_known || self.map_token != token {
            self.slots = [DynamicLight::DARK; MAX_DYNAMIC_LIGHTS];
            self.map_token = token;
            self.map_known = true;
        }
    }

    /// `TE_EXPLOSION`'s light, for a rocket, a grenade or a barrel.
    ///
    /// Cold: this fires on an impact, and the frame loop pays for every byte
    /// of I-cache it does not use.
    #[cold]
    #[inline(never)]
    pub fn spawn_explosion(&mut self, origin: Vec3I32) {
        self.push(
            origin,
            EXPLOSION_LIGHT_RADIUS_UNITS,
            EXPLOSION_LIGHT_DECAY,
            EXPLOSION_LIGHT_TICKS,
        );
    }

    /// `MUZZLEFLASH`, which the original attaches to the firing player.
    #[inline(never)]
    pub fn spawn_muzzle_flash(&mut self, origin: Vec3I32) {
        self.push(origin, MUZZLE_LIGHT_RADIUS_UNITS, 0, MUZZLE_LIGHT_TICKS);
    }

    /// Claim a slot, longest-lived light wins.
    ///
    /// `CL_AllocDlight` takes the first dead entry and falls back to slot
    /// zero, which it can afford with thirty-two of them. With one, a muzzle
    /// flash on every shot would evict the explosion you are standing in on
    /// the next trigger pull, so a light only displaces one with less life
    /// left than it has. A fresh blast (half a second) therefore takes the
    /// slot from anything, holds it against the flashes fired during it, and
    /// hands it over once it has less than a tenth of a second to live.
    #[inline(never)]
    fn push(&mut self, origin: Vec3I32, radius_units: i16, decay_per_tick: i16, ticks: u8) {
        let mut slot = 0usize;
        for index in 1..MAX_DYNAMIC_LIGHTS {
            if self.slots[index].remaining_ticks < self.slots[slot].remaining_ticks {
                slot = index;
            }
        }
        if self.slots[slot].active && self.slots[slot].remaining_ticks >= ticks {
            return;
        }
        self.slots[slot] = DynamicLight {
            origin: Vec3I16 {
                x: (origin.x >> 12) as i16,
                y: (origin.y >> 12) as i16,
                z: (origin.z >> 12) as i16,
            },
            radius_units,
            decay_per_tick,
            remaining_ticks: ticks,
            active: true,
        };
    }

    /// `R_RenderDlights`' own bookkeeping: decay the radius and retire the
    /// light once it dies or shrinks away.
    #[inline(never)]
    pub fn tick(&mut self, elapsed_ticks: u16) {
        let ticks = elapsed_ticks.min(u8::MAX as u16) as u8;
        for slot in &mut self.slots {
            if !slot.active {
                continue;
            }
            // Widened on purpose: an i16 saturating multiply-subtract is a
            // dozen instructions on MIPS and this pool is image-bound.
            let radius =
                i32::from(slot.radius_units) - i32::from(slot.decay_per_tick) * i32::from(ticks);
            if slot.remaining_ticks <= ticks || radius <= 0 {
                slot.remaining_ticks = 0;
                slot.active = false;
                continue;
            }
            slot.radius_units = radius as i16;
            slot.remaining_ticks -= ticks;
        }
    }

    /// Live lights in stable slot order, without allocating.
    pub fn active(&self) -> impl Iterator<Item = DynamicLight> + '_ {
        self.slots.iter().copied().filter(|slot| slot.active)
    }
}

impl Default for DynamicLights {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    const fn point(x: i32) -> Vec3I32 {
        Vec3I32 { x, y: 0, z: 0 }
    }

    /// The bounded `DeathBubbles`: every sprite starts above the corpse, they
    /// all climb rather than falling like blood, and they spread as they go so
    /// the burst does not read as one image.
    #[test]
    fn death_bubbles_start_above_the_corpse_and_rise() {
        let mut particles = ImpactParticles::new();
        particles.enter_map(1);
        particles.spawn_death_bubbles(Vec3I32 { x: 0, y: 0, z: 0 });
        let born: Vec<ImpactParticle> = particles.active().collect();
        assert_eq!(born.len(), 5);
        assert!(born.iter().all(|slot| slot.origin.z >= 24 << 12));

        particles.tick(6);
        let risen: Vec<ImpactParticle> = particles.active().collect();
        assert_eq!(risen.len(), 5);
        for (before, after) in born.iter().zip(risen.iter()) {
            assert!(
                after.origin.z > before.origin.z,
                "a bubble sank: {} then {}",
                before.origin.z,
                after.origin.z
            );
        }
        // Four of the five carry lateral drift, so the column widens.
        assert!(risen.iter().any(|slot| slot.origin.x != 0));
        assert!(risen.iter().any(|slot| slot.origin.y != 0));

        // They age out of the shared pool like every other particle; nothing
        // here pops them at a water surface, because nothing tracks one.
        particles.tick(u16::from(ParticleKind::Bubble.life_ticks()));
        assert_eq!(particles.active().count(), 0);
    }

    #[test]
    fn explosions_expand_fade_and_expire_on_the_simulation_clock() {
        let mut effects = ExplosionEffects::new();
        effects.enter_map(1);
        effects.spawn(point(7));
        let born = effects.active().next().expect("spawned effect");
        assert_eq!(born.age_ticks(), 0);
        assert_eq!(born.radius_units(), 8);

        effects.tick(5);
        let older = effects.active().next().expect("live effect");
        assert_eq!(older.age_ticks(), 5);
        assert!(older.radius_units() > born.radius_units());
        assert!(older.color().0 < born.color().0);
        assert!(older.color().1 < born.color().1);

        effects.tick(EXPLOSION_EFFECT_TICKS - 5);
        assert_eq!(effects.active().count(), 0);
    }

    #[test]
    fn the_fixed_pool_replaces_oldest_and_a_map_change_clears_it() {
        let mut effects = ExplosionEffects::new();
        effects.enter_map(3);
        for index in 0..=MAX_EXPLOSION_EFFECTS {
            effects.spawn(point(index as i32));
        }
        let origins = effects
            .active()
            .map(|effect| effect.origin.x)
            .collect::<Vec<_>>();
        assert_eq!(origins.len(), MAX_EXPLOSION_EFFECTS);
        assert!(!origins.contains(&0));
        assert!(origins.contains(&(MAX_EXPLOSION_EFFECTS as i32)));

        effects.enter_map(3);
        assert_eq!(effects.active().count(), MAX_EXPLOSION_EFFECTS);
        effects.enter_map(4);
        assert_eq!(effects.active().count(), 0);
    }

    #[test]
    fn blood_particles_are_bounded_move_fade_and_reset() {
        let mut particles = ImpactParticles::new();
        particles.enter_map(1);
        for _ in 0..=MAX_IMPACT_PARTICLES / 8 {
            particles.spawn_blood(point(9));
        }
        assert_eq!(particles.active().count(), MAX_IMPACT_PARTICLES);
        let born = particles.active().next().expect("blood particle");
        particles.tick(2);
        let moved = particles.active().next().expect("moving blood particle");
        assert_ne!(moved.origin, born.origin);
        assert!(moved.color().0 < born.color().0);

        particles.tick(IMPACT_PARTICLE_TICKS as u16);
        assert_eq!(particles.active().count(), 0);
        particles.spawn_blood(point(3));
        particles.enter_map(2);
        assert_eq!(particles.active().count(), 0);
    }

    #[test]
    fn a_trail_steps_by_distance_travelled_and_is_capped_per_call() {
        let mut particles = ImpactParticles::new();
        particles.enter_map(1);

        // Short of one step: nothing spawns and the anchor is kept, so the
        // next frame measures the whole distance and not just its own leg.
        let anchor = particles.spawn_trail(point(0), point(6 << 12), ParticleKind::Fire, 12);
        assert_eq!(particles.active().count(), 0);
        assert_eq!(anchor, point(0));

        // Two steps of travel, two particles, anchor advanced to the head.
        let anchor = particles.spawn_trail(anchor, point(26 << 12), ParticleKind::Fire, 12);
        assert_eq!(particles.active().count(), 2);
        assert_eq!(anchor, point(26 << 12));

        // A long leg is capped, so a fast rocket cannot flood the pool.
        particles.enter_map(2);
        particles.spawn_trail(point(0), point(400 << 12), ParticleKind::Fire, 12);
        assert_eq!(particles.active().count(), MAX_TRAIL_PARTICLES as usize);
    }

    #[test]
    fn rings_are_bounded_and_fade_on_their_own_ramps() {
        let mut particles = ImpactParticles::new();
        particles.enter_map(1);
        particles.spawn_ring(point(0), ParticleKind::Fire, EXPLOSION_RING_UNITS);
        assert_eq!(particles.active().count(), RING.len());
        let born = particles.active().next().expect("burst particle");
        assert!(born.color().0 > born.color().2);
        particles.tick(6);
        let older = particles.active().next().expect("live burst particle");
        assert!(older.color().0 < born.color().0);
        particles.tick(ParticleKind::Fire.life_ticks() as u16);
        assert_eq!(particles.active().count(), 0);

        particles.spawn_ring(point(0), ParticleKind::Spark, 16);
        assert_eq!(particles.active().count(), RING.len());
        let spark = particles.active().next().expect("splash particle");
        assert!(spark.color().2 > spark.color().0);
    }

    #[test]
    fn an_explosion_light_decays_on_the_original_s_schedule() {
        let mut lights = DynamicLights::new();
        lights.enter_map(1);
        lights.spawn_explosion(point(4));
        let born = lights.active().next().expect("explosion light");
        assert_eq!(born.radius_units(), EXPLOSION_LIGHT_RADIUS_UNITS as i32);

        // `dl->decay = 300` a second: half a second of it is 150 units.
        lights.tick(EXPLOSION_LIGHT_TICKS as u16 - 1);
        let dying = lights.active().next().expect("live explosion light");
        assert_eq!(dying.radius_units(), 350 - 5 * 29);
        lights.tick(1);
        assert_eq!(lights.active().count(), 0);

        // A map change puts every light out.
        lights.spawn_explosion(point(0));
        lights.enter_map(2);
        assert_eq!(lights.active().count(), 0);
    }

    #[test]
    fn a_muzzle_flash_never_evicts_the_explosion_it_was_fired_at() {
        let mut lights = DynamicLights::new();
        lights.enter_map(1);
        lights.spawn_explosion(point(9 << 12));
        // Keep firing into it: with one slot, the half-second blast has to
        // hold it against every tenth-of-a-second flash.
        for _ in 0..4 {
            lights.spawn_muzzle_flash(point(0));
            lights.tick(MUZZLE_LIGHT_TICKS as u16);
            assert_eq!(lights.active().count(), MAX_DYNAMIC_LIGHTS);
            assert_eq!(lights.active().next().expect("blast").origin.x, 9);
        }

        // Once the blast has less life left than a flash, the flash takes it.
        lights.tick(EXPLOSION_LIGHT_TICKS as u16 - MUZZLE_LIGHT_TICKS as u16 * 4 - 1);
        lights.spawn_muzzle_flash(point(0));
        assert_eq!(lights.active().next().expect("flash").origin.x, 0);
    }

    #[test]
    fn an_explosion_with_a_trail_never_evicts_the_frame_s_blood() {
        let mut particles = ImpactParticles::new();
        particles.enter_map(1);
        particles.spawn_blood(point(0));
        for _ in 0..5 {
            particles.spawn_trail(point(0), point(64 << 12), ParticleKind::Fire, 12);
        }
        particles.spawn_ring(point(0), ParticleKind::Fire, EXPLOSION_RING_UNITS);
        particles.spawn_trail(point(0), point(64 << 12), ParticleKind::Blood, 24);
        assert!(particles.active().count() <= MAX_IMPACT_PARTICLES);
        assert_eq!(
            particles
                .active()
                .filter(|particle| particle.kind == ParticleKind::Blood
                    && particle.velocity.z == 4 << 8)
                .count(),
            2,
            "the two upward blood droplets survive a full worst-case frame"
        );
    }
}
