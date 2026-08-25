//! `view.c` feel: weapon punch, damage kick, strafe roll, and the walk bob.
//!
//! Everything here is a pure offset layered on the player's own view angles
//! and eye height at render time, exactly like `V_CalcRefdef` layering
//! `cl.punchangle`, `V_CalcViewRoll` and `V_CalcBob` on top of `cl.viewangles`.
//! Angles are Q12 turns (4096 = 360 degrees), distances are Q20.12 Quake
//! units, and time is the 60 Hz vblank tick.

use psx_math::int32::{isqrt_i32, mul_q12_i32};
use psx_math::sin_q12;
use quake_formats::Vec3I32;

const TICKS_PER_SECOND: i32 = 60;
/// Sub-unit angle scale used for the punch so a 10 deg/s decay stays exact
/// across whole ticks: one Q12 angle unit is 256 sub-units.
const PUNCH_SUB: i32 = 256;
/// One degree in punch sub-units: 4096 * 256 / 360.
const DEGREE_SUB: i32 = 2913;
/// `DropPunchAngle`: `len -= 10 * host_frametime`.
const PUNCH_DECAY_PER_TICK: i32 = 10 * DEGREE_SUB / TICKS_PER_SECOND;
/// `v_kicktime` 0.5 s.
const KICK_TICKS: i32 = 30;
/// `cl_bobcycle` 0.6 s.
const BOB_CYCLE_TICKS: i32 = 36;
/// `cl_rollspeed` 200 units/s and `cl_rollangle` 2 degrees (23 Q12 units).
const ROLL_SPEED: i32 = 200;
const ROLL_ANGLE: i32 = 23;
/// `V_CalcViewRoll`: `r_refdef.viewangles[ROLL] = 80` once dead.
const DEAD_ROLL: i16 = 910;
const BOB_MIN_Q12: i32 = -7 << 12;
const BOB_MAX_Q12: i32 = 4 << 12;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewFeel {
    punch_sub: i32,
    dmg_roll: i32,
    dmg_pitch: i32,
    dmg_ticks: i32,
    bob_tick: i32,
    pitch_delta: i16,
    roll_delta: i16,
    bob_z_q12: i32,
}

impl ViewFeel {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            punch_sub: 0,
            dmg_roll: 0,
            dmg_pitch: 0,
            dmg_ticks: 0,
            bob_tick: 0,
            pitch_delta: 0,
            roll_delta: 0,
            bob_z_q12: 0,
        }
    }

    /// `self.punchangle_x = degrees` from the QuakeC fire functions; the
    /// value replaces whatever was still decaying.
    #[optimize(size)]
    pub fn punch(&mut self, degrees: i32) {
        self.punch_sub = degrees.saturating_mul(DEGREE_SUB);
    }

    /// `V_ParseDamage` angular part. `count` is the damage total (already
    /// clamped to at least 10 by the caller or here), `side` and `front` are
    /// the Q12 dot products of the normalized attacker direction with the
    /// view right and forward vectors.
    #[optimize(size)]
    pub fn damage(&mut self, count: i32, side_q12: i32, front_q12: i32) {
        let count = count.max(10);
        // count * side * v_kickroll(0.6) degrees, in Q12 angle units:
        // count * side_q12 * 0.6 * 4096 / 360 / 4096 = count * side_q12 / 600.
        self.dmg_roll = count.saturating_mul(side_q12) / 600;
        self.dmg_pitch = count.saturating_mul(front_q12) / 600;
        self.dmg_ticks = KICK_TICKS;
    }

    /// Advance the decays and recompute the offsets for this frame.
    #[optimize(size)]
    pub fn tick(&mut self, velocity: Vec3I32, right: Vec3I32, dead: bool, elapsed_ticks: u16) {
        let ticks = i32::from(elapsed_ticks);
        // DropPunchAngle.
        let decay = PUNCH_DECAY_PER_TICK.saturating_mul(ticks);
        self.punch_sub = if self.punch_sub < 0 {
            (self.punch_sub + decay).min(0)
        } else {
            (self.punch_sub - decay).max(0)
        };
        let mut pitch = self.punch_sub / PUNCH_SUB;

        // V_CalcRoll: dot(velocity, right) in units/s scaled into the roll
        // angle up to cl_rollspeed.
        let side = mul_q12_i32(velocity.x, right.x)
            .saturating_add(mul_q12_i32(velocity.y, right.y))
            .saturating_add(mul_q12_i32(velocity.z, right.z))
            >> 12;
        let mut roll = side.clamp(-ROLL_SPEED, ROLL_SPEED) * ROLL_ANGLE / ROLL_SPEED;

        // Damage kick, fading linearly over v_kicktime.
        if self.dmg_ticks > 0 {
            roll += self.dmg_roll.saturating_mul(self.dmg_ticks) / KICK_TICKS;
            pitch += self.dmg_pitch.saturating_mul(self.dmg_ticks) / KICK_TICKS;
            self.dmg_ticks = (self.dmg_ticks - ticks).max(0);
        }

        // V_CalcBob: with cl_bobup at 0.5 both branches reduce to one full
        // sine turn per cl_bobcycle.
        self.bob_tick = (self.bob_tick + ticks) % BOB_CYCLE_TICKS;
        let vx = velocity.x >> 12;
        let vy = velocity.y >> 12;
        let speed = isqrt_i32(vx.saturating_mul(vx).saturating_add(vy.saturating_mul(vy)));
        // speed * cl_bob(0.02) in Q12.
        let bob = speed.saturating_mul(82);
        let cycle = ((self.bob_tick << 12) / BOB_CYCLE_TICKS) as u16;
        let bob = mul_q12_i32(bob, 1229)
            .saturating_add(mul_q12_i32(mul_q12_i32(bob, 2867), sin_q12(cycle)));

        if dead {
            self.roll_delta = DEAD_ROLL;
            self.bob_z_q12 = 0;
        } else {
            self.roll_delta = roll.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            self.bob_z_q12 = bob.clamp(BOB_MIN_Q12, BOB_MAX_Q12);
        }
        self.pitch_delta = pitch.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }

    /// `(pitch_delta, roll_delta, bob_z_q12)` to add to the view angles and
    /// the eye height for the frame `tick` last computed.
    #[optimize(size)]
    pub const fn offsets(&self) -> (i16, i16, i32) {
        (self.pitch_delta, self.roll_delta, self.bob_z_q12)
    }
}

/// `V_ParseDamage`'s `side`/`front`: normalize the attacker-to-eye
/// direction and project it on the view basis. A zero direction (world
/// damage, `from == origin` in the original) yields no kick.
#[optimize(size)]
pub fn damage_components(from: Vec3I32, forward: Vec3I32, right: Vec3I32) -> (i32, i32) {
    let fx = from.x >> 12;
    let fy = from.y >> 12;
    let fz = from.z >> 12;
    let length = isqrt_i32(
        fx.saturating_mul(fx)
            .saturating_add(fy.saturating_mul(fy))
            .saturating_add(fz.saturating_mul(fz)),
    );
    if length == 0 {
        return (0, 0);
    }
    let dot = |basis: Vec3I32| {
        (fx.saturating_mul(basis.x)
            .saturating_add(fy.saturating_mul(basis.y))
            .saturating_add(fz.saturating_mul(basis.z)))
            / length
    };
    (dot(right), dot(forward))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STILL: Vec3I32 = Vec3I32 { x: 0, y: 0, z: 0 };
    const RIGHT: Vec3I32 = Vec3I32 {
        x: 0,
        y: -4096,
        z: 0,
    };

    #[optimize(size)]
    #[test]
    fn punch_decays_to_zero_at_ten_degrees_per_second() {
        let mut view = ViewFeel::new();
        view.punch(-2);
        view.tick(STILL, RIGHT, false, 1);
        let (pitch, roll, bob) = view.offsets();
        assert!((-23..=-20).contains(&pitch), "pitch {pitch}");
        assert_eq!((roll, bob), (0, 0));
        // 2 degrees at 10 deg/s is 0.2 s: 12 ticks.
        for _ in 0..10 {
            view.tick(STILL, RIGHT, false, 1);
        }
        assert!(view.offsets().0 < 0);
        view.tick(STILL, RIGHT, false, 1);
        assert_eq!(view.offsets().0, 0);
    }

    #[optimize(size)]
    #[test]
    fn damage_tilt_fades_in_half_a_second() {
        let mut view = ViewFeel::new();
        // 20 damage from the right and front.
        view.damage(20, 4096, 4096);
        view.tick(STILL, RIGHT, false, 1);
        let (pitch, roll, _) = view.offsets();
        // 20 * 0.6 = 12 degrees = 136 units.
        assert_eq!((pitch, roll), (136, 136));
        for _ in 0..15 {
            view.tick(STILL, RIGHT, false, 1);
        }
        let (pitch, roll, _) = view.offsets();
        assert!(pitch > 0 && pitch < 136 && roll == pitch);
        for _ in 0..15 {
            view.tick(STILL, RIGHT, false, 1);
        }
        assert_eq!(view.offsets(), (0, 0, 0));
        // World damage carries no direction.
        assert_eq!(damage_components(STILL, RIGHT, RIGHT), (0, 0));
    }

    #[optimize(size)]
    #[test]
    fn bob_clamps_and_rests_when_still() {
        let mut view = ViewFeel::new();
        for _ in 0..40 {
            view.tick(STILL, RIGHT, false, 1);
            assert_eq!(view.offsets().2, 0);
        }
        let running = Vec3I32 {
            x: 1000 << 12,
            y: 0,
            z: 0,
        };
        let (mut low, mut high) = (i32::MAX, i32::MIN);
        for _ in 0..36 {
            view.tick(running, RIGHT, false, 1);
            let bob = view.offsets().2;
            low = low.min(bob);
            high = high.max(bob);
        }
        assert_eq!(high, BOB_MAX_Q12);
        assert_eq!(low, BOB_MIN_Q12);
        // Strafing right rolls the view right, capped at cl_rollangle.
        let strafing = Vec3I32 {
            x: 0,
            y: -400 << 12,
            z: 0,
        };
        view.tick(strafing, RIGHT, false, 1);
        assert_eq!(i32::from(view.offsets().1), ROLL_ANGLE);
        view.tick(strafing, RIGHT, true, 1);
        assert_eq!(view.offsets(), (0, DEAD_ROLL, 0));
    }
}
