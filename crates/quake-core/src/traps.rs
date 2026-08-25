//! Original `trap_spikeshooter` and `misc_fireball` hazards.

use psx_math::int32::mul_q12_i32;
use quake_formats::Vec3I32;

const Q12_ONE: i32 = 4096;
const TICKS_PER_SECOND: i32 = 60;

/// `trap_spikeshooter` spawnflag 1: `superspike_touch` instead of
/// `spike_touch`.
pub const SPAWNFLAG_SUPERSPIKE: u16 = 1;
/// `trap_spikeshooter` spawnflag 2: `LaunchLaser`. No shareware Episode 1
/// shooter sets it, so this port launches spikes only.
pub const SPAWNFLAG_LASER: u16 = 2;
/// `newmis.velocity = self.movedir * 500`.
pub const SPIKE_SPEED_UNITS: i32 = 500;
/// `spike_touch`'s `T_Damage (other, self, self.owner, 9)`.
pub const SPIKE_DAMAGE: i16 = 9;
/// `superspike_touch`'s `T_Damage (other, self, self.owner, 18)`.
pub const SUPERSPIKE_DAMAGE: i16 = 18;
/// `launch_spike` sets `nextthink = time + 6`.
pub const SPIKE_LIFETIME_TICKS: u16 = 360;
/// `spikeshooter_use` plays weapons/spike2.
pub const SPIKE_SOUND_ID: i16 = 0xcf;

/// `misc_fireball`'s `if (!self.speed) self.speed = 1000`.
pub const FIREBALL_DEFAULT_SPEED: i16 = 1_000;
/// `fire_touch`'s `T_Damage (other, self, self, 20)`.
pub const FIREBALL_DAMAGE: i16 = 20;
/// The lava ball removes itself at `time + 5`.
pub const FIREBALL_LIFETIME_TICKS: u16 = 300;
/// `progs/lavaball.mdl`, already in the cooked resource list for this class.
pub const FIREBALL_MODEL_ID: i16 = 0x3d;
/// `fire_fly` re-arms at `time + random() * 5 + 3`.
const FIREBALL_MIN_INTERVAL_TICKS: u32 = 180;
const FIREBALL_INTERVAL_SPREAD_TICKS: u32 = 300;
/// `misc_fireball` first thinks at `time + random() * 5`.
const FIREBALL_FIRST_DELAY_SPREAD_TICKS: u32 = 300;
/// `fireball.velocity_x = random() * 100 - 50`, same for y.
const FIREBALL_LATERAL_SPREAD_UNITS: u32 = 100;
/// `fireball.velocity_z = self.speed + random() * 200`.
const FIREBALL_RISE_SPREAD_UNITS: u32 = 200;

/// Deterministic stand-in for the original `random()`.
///
/// A headless gate has to be byte-identical across runs, so every emitter owns
/// a 32-bit LCG seeded from its own cooked source index. Emitters stay
/// independent and staggered exactly as the authored maps intend, but the
/// sequence is reproducible.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TrapRandom(u32);

impl TrapRandom {
    #[optimize(size)]
    pub const fn new(source_index: u16) -> Self {
        Self(0x1357_9bdf ^ (source_index as u32).wrapping_mul(2_654_435_761))
    }

    #[optimize(size)]
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 16
    }

    #[optimize(size)]
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }
}

/// Damage one spike from this shooter deals.
#[optimize(size)]
pub const fn spikeshooter_damage(spawn_flags: u16) -> i16 {
    if spawn_flags & SPAWNFLAG_SUPERSPIKE != 0 {
        SUPERSPIKE_DAMAGE
    } else {
        SPIKE_DAMAGE
    }
}

/// Per-tick Q20.12 displacement for a spike travelling along a Q12 unit
/// direction at the original's 500 units per second.
#[optimize(size)]
pub fn spike_step(direction: Vec3I32) -> Vec3I32 {
    let scale = SPIKE_SPEED_UNITS * Q12_ONE / TICKS_PER_SECOND;
    Vec3I32 {
        x: mul_q12_i32(direction.x, scale),
        y: mul_q12_i32(direction.y, scale),
        z: mul_q12_i32(direction.z, scale),
    }
}

/// `misc_fireball`'s first `nextthink`.
#[optimize(size)]
pub fn fireball_first_delay_ticks(random: &mut TrapRandom) -> u16 {
    random.below(FIREBALL_FIRST_DELAY_SPREAD_TICKS) as u16
}

/// `fire_fly`'s next `nextthink`.
#[optimize(size)]
pub fn fireball_next_delay_ticks(random: &mut TrapRandom) -> u16 {
    (FIREBALL_MIN_INTERVAL_TICKS + random.below(FIREBALL_INTERVAL_SPREAD_TICKS)) as u16
}

/// One lava ball's launch velocity as a per-tick Q20.12 displacement.
#[optimize(size)]
pub fn fireball_velocity(random: &mut TrapRandom, speed: i16) -> Vec3I32 {
    let per_tick = |units: i32| units.saturating_mul(Q12_ONE) / TICKS_PER_SECOND;
    let lateral = |random: &mut TrapRandom| {
        random.below(FIREBALL_LATERAL_SPREAD_UNITS) as i32
            - (FIREBALL_LATERAL_SPREAD_UNITS as i32 / 2)
    };
    let speed = if speed == 0 {
        FIREBALL_DEFAULT_SPEED
    } else {
        speed
    };
    Vec3I32 {
        x: per_tick(lateral(random)),
        y: per_tick(lateral(random)),
        z: per_tick(i32::from(speed) + random.below(FIREBALL_RISE_SPREAD_UNITS) as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn superspike_shooters_do_double_damage() {
        assert_eq!(spikeshooter_damage(0), SPIKE_DAMAGE);
        assert_eq!(spikeshooter_damage(SPAWNFLAG_SUPERSPIKE), SUPERSPIKE_DAMAGE);
        // E1M2 authors 1024 (not-hard) shooters with no superspike bit.
        assert_eq!(spikeshooter_damage(1024), SPIKE_DAMAGE);
        assert_eq!(spikeshooter_damage(769), SUPERSPIKE_DAMAGE);
    }

    #[optimize(size)]
    #[test]
    fn a_spike_covers_500_units_per_second() {
        let step = spike_step(Vec3I32 {
            x: 0,
            y: 0,
            z: -Q12_ONE,
        });
        assert_eq!(step.x, 0);
        assert_eq!(step.z, -(SPIKE_SPEED_UNITS * Q12_ONE / TICKS_PER_SECOND));
        // Six seconds of flight at 500 units per second.
        assert_eq!(SPIKE_LIFETIME_TICKS, 6 * 60);
    }

    #[optimize(size)]
    #[test]
    fn each_emitter_gets_its_own_reproducible_stagger() {
        let mut first = TrapRandom::new(11);
        let mut second = TrapRandom::new(12);
        let mut replay = TrapRandom::new(11);
        let a = fireball_first_delay_ticks(&mut first);
        let b = fireball_first_delay_ticks(&mut second);
        assert_ne!(a, b);
        assert_eq!(a, fireball_first_delay_ticks(&mut replay));
        for _ in 0..64 {
            let interval = fireball_next_delay_ticks(&mut first);
            assert!((180..480).contains(&interval), "interval {interval}");
        }
    }

    #[optimize(size)]
    #[test]
    fn every_lava_ball_rises_and_drifts_inside_the_authored_envelope() {
        let mut random = TrapRandom::new(3);
        for _ in 0..256 {
            let velocity = fireball_velocity(&mut random, 0);
            // 1000 to 1200 units per second up.
            assert!(velocity.z >= 1_000 * Q12_ONE / 60);
            assert!(velocity.z <= 1_200 * Q12_ONE / 60);
            // 50 units per second of lateral drift either way.
            assert!(velocity.x.abs() <= 50 * Q12_ONE / 60);
            assert!(velocity.y.abs() <= 50 * Q12_ONE / 60);
        }
        let mut authored = TrapRandom::new(3);
        assert!(fireball_velocity(&mut authored, 200).z >= 200 * Q12_ONE / 60);
    }
}
