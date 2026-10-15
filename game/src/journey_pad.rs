//! Pad round trip for the journey authoring images.
//!
//! The journey controllers think in `InputFrame`s. To make their output
//! replayable on the shipping disc as a controller tape, every frame is turned
//! into the pad sample (buttons and four stick bytes) that produces it, run
//! back through the same stick filtering `Input::poll` applies, and logged to
//! the emulator debug log as `PADTAPE`. The game then plays the filtered
//! frame, so what it simulated is exactly what a tape of the logged bytes
//! reproduces. The shipping input path is untouched.

use psx_pad::{aim_curve, Deadzone};

use crate::input::InputFrame;
use crate::input_policy::{
    quake_horizontal_axis, quake_look_pitch_axis, EdgeTracker, MENU_STICK_THRESHOLD,
};

/// `aim_curve`'s preimage: the stick value whose curve output is nearest `want`.
fn invert_aim(want: i16) -> i16 {
    let mut best = 0i16;
    let mut error = i32::MAX;
    for candidate in -127i16..=127 {
        let e = (i32::from(aim_curve(candidate)) - i32::from(want)).abs();
        if e < error || (e == error && candidate.abs() < best.abs()) {
            best = candidate;
            error = e;
        }
    }
    best
}

/// Stick bytes (x, y) whose scaled-radial output is nearest `(tx, ty)`.
fn stick_for(deadzone: Deadzone, radius: i16, tx: i16, ty: i16) -> (u8, u8) {
    if tx == 0 && ty == 0 {
        return (128, 128);
    }
    let (fx, fy) = (i32::from(tx), i32::from(ty));
    let magnitude = psx_math::int32::isqrt_i32(fx * fx + fy * fy).max(1);
    let mut best = (0i16, 0i16);
    let mut error = i32::MAX;
    for reach in (i32::from(radius) + 1)..=181 {
        let x = ((fx * reach) / magnitude).clamp(-128, 127) as i16;
        let y = ((fy * reach) / magnitude).clamp(-128, 127) as i16;
        let (sx, sy) = deadzone.scaled(x, y).unwrap_or((0, 0));
        let e = (i32::from(sx) - fx).pow(2) + (i32::from(sy) - fy).pow(2);
        if e < error {
            best = (x, y);
            error = e;
        }
    }
    ((128 + i32::from(best.0)) as u8, (128 + i32::from(best.1)) as u8)
}

pub struct JourneyPad {
    edges: EdgeTracker,
    frame: u32,
}

impl JourneyPad {
    pub const fn new() -> Self {
        Self { edges: EdgeTracker::new(), frame: 0 }
    }

    /// `want` is what the controller asked for; the result is what a
    /// controller carrying the logged sample would have produced.
    pub fn through_pad(&mut self, want: InputFrame, deadzone_radius: i16, menu: bool) -> InputFrame {
        let deadzone = Deadzone::new(deadzone_radius);
        let held = want.held | want.pressed;
        let (lx_b, ly_b) = stick_for(
            deadzone,
            deadzone_radius,
            -want.movement[1],
            -want.movement[0],
        );
        let (rx_b, ry_b) = stick_for(
            deadzone,
            deadzone_radius,
            -invert_aim(want.look[0]),
            invert_aim(want.look[1]),
        );
        let (lx, ly) = (i16::from(lx_b) - 128, i16::from(ly_b) - 128);
        let (rx, ry) = (i16::from(rx_b) - 128, i16::from(ry_b) - 128);
        let (mut strafe, mut forward, mut yaw, mut pitch) = (0, 0, 0, 0);
        if let Some((x, y)) = deadzone.scaled(lx, ly) {
            strafe = quake_horizontal_axis(x);
            forward = -y;
        }
        if let Some((x, y)) = deadzone.scaled(rx, ry) {
            yaw = aim_curve(quake_horizontal_axis(x));
            pitch = aim_curve(quake_look_pitch_axis(y));
        }
        // The dpad moves unless Triangle is held, exactly as `Input::poll`.
        if held & crate::input_policy::button::TRIANGLE == 0 {
            use crate::input_policy::button as b;
            if held & b::UP != 0 {
                forward = 127;
            }
            if held & b::DOWN != 0 {
                forward = -127;
            }
            if held & b::RIGHT != 0 {
                strafe = quake_horizontal_axis(127);
            }
            if held & b::LEFT != 0 {
                strafe = quake_horizontal_axis(-127);
            }
        }
        let edges = self.edges.update(held, lx, ly);
        let _ = MENU_STICK_THRESHOLD;
        // The heap is a bump allocator that never frees, so format on the stack.
        struct Line {
            bytes: [u8; 64],
            len: usize,
        }
        impl core::fmt::Write for Line {
            fn write_str(&mut self, text: &str) -> core::fmt::Result {
                let room = self.bytes.len() - self.len;
                let take = text.len().min(room);
                self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
                self.len += take;
                Ok(())
            }
        }
        let mut line = Line { bytes: [0; 64], len: 0 };
        let _ = core::fmt::write(
            &mut line,
            format_args!(
                "PADTAPE {} {} {:04x} {:02x}{:02x}{:02x}{:02x}",
                self.frame,
                u8::from(menu),
                held,
                rx_b,
                ry_b,
                lx_b,
                ly_b
            ),
        );
        if let Ok(text) = core::str::from_utf8(&line.bytes[..line.len]) {
            psx_telemetry::emit::debug_log(text);
        }
        self.frame = self.frame.wrapping_add(1);
        InputFrame {
            movement: [forward, strafe],
            look: [yaw, pitch],
            held,
            pressed: edges.pressed,
            menu_pressed: edges.menu_pressed,
        }
    }
}
