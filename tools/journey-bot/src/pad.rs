//! Raw pad samples for the journey driver and the stick maths that produce them.
//!
//! The guest reads the left stick through a scaled-radial dead zone and the right
//! stick through the same zone plus `psx_pad::aim_curve`; these helpers invert that
//! so a controller can ask for "this much forward, this much turn" and get the stick
//! bytes a person would have held.

use emulator_core::input_tape::PadSample;

/// The game's default stick dead zone (`DEADZONE_RADII[2]`).
pub const DEAD: i32 = 28;

pub mod button {
    pub const SELECT: u16 = 1 << 0;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

/// `psx_pad::aim_curve`: 45% linear plus 55% cubic.
pub fn aim_curve(value: i32) -> i32 {
    let value = value.clamp(-128, 128);
    let magnitude = value.abs();
    let cubic = magnitude * magnitude / 128 * magnitude / 128;
    value.signum() * ((magnitude * 45 + cubic * 55) / 100)
}

/// The stick deflection (-127..127) whose curve output is nearest `want`.
fn invert_aim(want: i32) -> i32 {
    let mut best: i32 = 0;
    let mut error = i32::MAX;
    for candidate in -127..=127 {
        let e = (aim_curve(candidate) - want).abs();
        if e < error || (e == error && candidate.abs() < best.abs()) {
            best = candidate;
            error = e;
        }
    }
    best
}

/// Radius in stick units that the dead zone maps to scaled magnitude `m` (0..127).
fn radius_for(scaled: i32) -> i32 {
    DEAD + (scaled * (127 - DEAD) + 126) / 127
}

fn byte(centred: i32) -> u8 {
    (128 + centred.clamp(-128, 127)) as u8
}

/// What a controller asks of one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Command {
    /// Forward (+) / back (-), -127..127.
    pub forward: i32,
    /// Strafe left (+) / right (-), -127..127 (the game's sign).
    pub strafe: i32,
    /// Yaw change per tick in Q12 turns, positive turns left (counter-clockwise).
    pub yaw: i32,
    /// Pitch change per tick in Q12 turns, positive looks down.
    pub pitch: i32,
    /// Buttons held.
    pub buttons: u16,
}

/// Turn a command into a pad sample.
pub fn sample(cmd: Command) -> PadSample {
    // Left stick: stick right (lx > 0) strafes right (negative strafe); stick up
    // (ly < 0) is forward.
    let (mut lx, mut ly) = (0, 0);
    let mag = isqrt(cmd.forward * cmd.forward + cmd.strafe * cmd.strafe);
    if mag > 0 {
        let scaled = mag.min(127);
        let r = radius_for(scaled);
        lx = -cmd.strafe * r / mag;
        ly = -cmd.forward * r / mag;
    }
    // Right stick: yaw = aim_curve(-(rx*scaled)/rmag) * 32 / 127 per tick.
    let want_yaw = (cmd.yaw * 127 / 32).clamp(-127, 127);
    let want_pitch = (cmd.pitch * 127 / 24).clamp(-127, 127);
    let yaw_axis = invert_aim(want_yaw);
    let pitch_axis = invert_aim(want_pitch);
    let (mut rx, mut ry) = (0, 0);
    let rmag = isqrt(yaw_axis * yaw_axis + pitch_axis * pitch_axis);
    if rmag > 0 {
        let r = radius_for(rmag.min(127));
        rx = -yaw_axis * r / rmag;
        ry = pitch_axis * r / rmag;
    }
    PadSample {
        buttons: cmd.buttons,
        right_x: byte(rx),
        right_y: byte(ry),
        left_x: byte(lx),
        left_y: byte(ly),
    }
}

pub fn isqrt(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut x = (n as f64).sqrt() as i32;
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guest's stick reading, copied from the routesim tape replay.
    fn guest_read(s: PadSample) -> (i32, i32, i32, i32) {
        let (lx, ly) = (i32::from(s.left_x) - 128, i32::from(s.left_y) - 128);
        let (mut strafe, mut forward) = (0, 0);
        let m = isqrt(lx * lx + ly * ly);
        if m > DEAD {
            let scaled = (((m - DEAD) * 127) / (127 - DEAD)).min(127);
            strafe = -((lx * scaled) / m).clamp(-127, 127);
            forward = -((ly * scaled) / m);
        }
        let (rx, ry) = (i32::from(s.right_x) - 128, i32::from(s.right_y) - 128);
        let (mut yaw, mut pitch) = (0, 0);
        let rm = isqrt(rx * rx + ry * ry);
        if rm > DEAD {
            let scaled = (((rm - DEAD) * 127) / (127 - DEAD)).min(127);
            yaw = aim_curve(-((rx * scaled) / rm).clamp(-127, 127)) * 32 / 127;
            pitch = aim_curve(((ry * scaled) / rm).clamp(-127, 127)) * 24 / 127;
        }
        (forward, strafe, yaw, pitch)
    }

    #[test]
    fn sticks_round_trip_within_two_units() {
        for forward in [-127, -60, 0, 30, 90, 127] {
            for strafe in [-127, -40, 0, 70, 127] {
                for yaw in [-32, -10, 0, 3, 20, 32] {
                    let c = Command { forward, strafe, yaw, pitch: yaw / 2, buttons: 0 };
                    let (f, s, y, p) = guest_read(sample(c));
                    let mag = isqrt(forward * forward + strafe * strafe).min(127);
                    let tol = 4 + mag / 20;
                    if mag > 0 {
                        // direction preserved, magnitude approximate
                        assert!((f - forward * mag / isqrt(forward * forward + strafe * strafe).max(1)).abs() <= tol, "f {forward} {strafe}: {f}");
                        assert!((s - strafe * mag / isqrt(forward * forward + strafe * strafe).max(1)).abs() <= tol, "s");
                    } else {
                        assert_eq!((f, s), (0, 0));
                    }
                    assert!((y - yaw).abs() <= 3, "yaw {yaw}: {y}");
                    assert!((p - yaw / 2).abs() <= 3, "pitch");
                }
            }
        }
    }
}
