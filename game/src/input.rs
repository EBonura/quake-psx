use psx_io::periph::ControllerPort;
use psx_pad::{aim_curve, button, Controllers, Deadzone, PadState, Port};

pub use crate::input_policy::InputFrame;
use crate::input_policy::{quake_horizontal_axis, quake_look_pitch_axis, EdgeTracker};

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

/// Port 1, read the one way every PSoXide game reads its pads: the first poll of a pad
/// runs the analog handshake, a garbled packet returns the last clean state.
pub struct Input {
    edges: EdgeTracker,
    pads: Controllers,
    port: ControllerPort,
}

impl Input {
    /// Take over the pads the boot code already polled, so a pad is handshaken once.
    #[optimize(size)]
    pub fn new(pads: Controllers, port: ControllerPort) -> Self {
        Self {
            edges: EdgeTracker::new(),
            pads,
            port,
        }
    }

    /// The motor request sent with every poll from now on.
    #[inline(always)]
    pub fn set_rumble(&mut self, #[allow(unused)] rumble: crate::rumble::Request) {
        #[cfg(feature = "rumble")]
        self.pads.set_rumble(&mut self.port, Port::One, rumble);
    }

    /// Produce one gameplay sample for this frame. Analog filtering
    /// deliberately calls the SDK's scaled-radial deadzone and Half-Life aim
    /// curve instead of carrying another game-specific implementation.
    #[optimize(size)]
    pub fn poll(&mut self, deadzone: i16) -> InputFrame {
        let pad: PadState = *self.pads.poll(&mut self.port, Port::One);
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
