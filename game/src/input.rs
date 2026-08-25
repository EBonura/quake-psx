//! Modern twin-stick input built on PSoXide's shared pad policy.

use psx_pad::{aim_curve, button, enable_analog_port1, poll_port1, Deadzone, PadMode, PadState};

pub use crate::input_policy::InputFrame;
use crate::input_policy::{
    quake_horizontal_axis, quake_look_pitch_axis, AnalogRetry, EdgeTracker,
    UNKNOWN_MODE_TRUST_POLLS,
};

const _: () = {
    assert!(crate::input_policy::button::SELECT == button::SELECT);
    assert!(crate::input_policy::button::START == button::START);
    assert!(crate::input_policy::button::UP == button::UP);
    assert!(crate::input_policy::button::RIGHT == button::RIGHT);
    assert!(crate::input_policy::button::DOWN == button::DOWN);
    assert!(crate::input_policy::button::LEFT == button::LEFT);
    assert!(crate::input_policy::button::R2 == button::R2);
    assert!(crate::input_policy::button::L1 == button::L1);
    assert!(crate::input_policy::button::R1 == button::R1);
    assert!(crate::input_policy::button::TRIANGLE == button::TRIANGLE);
    assert!(crate::input_policy::button::CIRCLE == button::CIRCLE);
    assert!(crate::input_policy::button::CROSS == button::CROSS);
    assert!(crate::input_policy::button::SQUARE == button::SQUARE);
};

pub struct Input {
    edges: EdgeTracker,
    last_pad: PadState,
    analog_retry: AnalogRetry,
    /// Consecutive connected polls that answered with an unrecognised ID.
    unknown_polls: u8,
}

impl Input {
    #[optimize(size)]
    pub fn new() -> Self {
        let mut input = Self {
            edges: EdgeTracker::new(),
            last_pad: PadState::NONE,
            analog_retry: AnalogRetry::new(),
            unknown_polls: 0,
        };
        input.last_pad = input.poll_clean_pad();
        input
    }

    /// Return one valid controller sample and complete a Digital-to-Analog
    /// transition before exposing its axes to gameplay.
    ///
    /// This follows HL-PSX's live-input policy: retain the last clean sample
    /// across a garbled ID response, request locked analog mode whenever a
    /// DualShock reports Digital/Config, then use the verified follow-up poll.
    /// Processing the pre-handshake frame made the guest's first analog frame
    /// depend on transient controller state in emulators and on real pads.
    #[optimize(size)]
    fn poll_clean_pad(&mut self) -> PadState {
        let sampled = poll_port1();
        let mut pad = if sampled.mode == PadMode::Unknown {
            if sampled.is_connected() {
                self.unknown_polls = self.unknown_polls.saturating_add(1);
            } else {
                self.unknown_polls = 0;
            }
            if self.unknown_polls >= UNKNOWN_MODE_TRUST_POLLS {
                // Not a garbled frame: this pad answers like this every
                // poll. Use its buttons rather than ignoring the player.
                self.last_pad = sampled;
                sampled
            } else {
                self.last_pad
            }
        } else {
            self.unknown_polls = 0;
            self.last_pad = sampled;
            sampled
        };

        if self
            .analog_retry
            .poll(pad.is_analog(), sampled.is_connected())
        {
            // VoXide retries any non-analog state. In particular, an
            // interrupted configuration transaction may report Unknown rather
            // than Digital/Config; restricting this retry to those two modes
            // left Quake permanently digital on real hardware.
            if enable_analog_port1() {
                let verified = poll_port1();
                if verified.mode != PadMode::Unknown {
                    self.last_pad = verified;
                    pad = verified;
                }
            }
        }
        pad
    }

    /// Produce one gameplay sample for this frame. Analog filtering
    /// deliberately calls the SDK's scaled-radial deadzone and Half-Life aim
    /// curve instead of carrying another game-specific implementation.
    #[optimize(size)]
    pub fn poll(&mut self, deadzone: i16) -> InputFrame {
        let pad = self.poll_clean_pad();
        let held = pad.buttons.bits();

        let (mut strafe, mut forward, mut yaw, mut pitch) = (0, 0, 0, 0);
        let (mut left_x, mut left_y) = (0, 0);
        if pad.is_analog() {
            let (lx, ly) = pad.sticks.left_centered();
            left_x = lx;
            left_y = ly;
            let (rx, ry) = pad.sticks.right_centered();
            if let Some((x, y)) = Deadzone::new(deadzone).scaled(lx, ly) {
                strafe = quake_horizontal_axis(x);
                forward = -y;
            }
            if let Some((x, y)) = Deadzone::new(deadzone).scaled(rx, ry) {
                yaw = aim_curve(quake_horizontal_axis(x));
                // Quake and the DualShock both encode look-up as negative.
                pitch = aim_curve(quake_look_pitch_axis(y));
            }
        }

        // Digital movement remains available on every pad. Shoulders retain
        // the original port's weapon-cycle mapping; look is right-stick only.
        let dpad_moves = InputFrame {
            held,
            ..InputFrame::default()
        }
        .dpad_moves();
        if dpad_moves && pad.buttons.is_held(button::UP) {
            forward = 127;
        }
        if dpad_moves && pad.buttons.is_held(button::DOWN) {
            forward = -127;
        }
        if dpad_moves && pad.buttons.is_held(button::RIGHT) {
            strafe = quake_horizontal_axis(127);
        }
        if dpad_moves && pad.buttons.is_held(button::LEFT) {
            strafe = quake_horizontal_axis(-127);
        }
        let edges = self.edges.update(held, left_x, left_y);

        InputFrame {
            movement: [forward, strafe],
            look: [yaw, pitch],
            held,
            pressed: edges.pressed,
            menu_pressed: edges.menu_pressed,
        }
    }
}
