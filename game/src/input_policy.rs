//! Pure controller policy shared by the MIPS guest and host regressions.

/// Active-high PlayStation button bits used by the shipping layout.
///
/// `input.rs` has compile-time assertions tying these protocol constants to
/// `psx-pad`; keeping the pure policy here makes its edge behavior host-testable.
pub mod button {
    pub const SELECT: u16 = 1 << 0;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

const DPAD: u16 = button::UP | button::RIGHT | button::DOWN | button::LEFT;

pub const MENU_STICK_THRESHOLD: i16 = 48;

/// Poll cadence for reasserting locked DualShock analog mode.
pub const ANALOG_RETRY_POLLS: u8 = 15;
/// Analog-mode exchanges attempted on one connected pad before giving up on
/// it. A pad that stays digital through eight requests (two seconds) is
/// either digital or ignores the request; hammering it four times a second
/// forever only keeps interrupting its ordinary polls. A disconnect and
/// reconnect starts the budget over.
pub const ANALOG_RETRY_ATTEMPTS: u8 = 8;

/// Bounded reconnect policy shared with the host input regression.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AnalogRetry {
    remaining: u8,
    attempts: u8,
}

impl AnalogRetry {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            remaining: 0,
            attempts: 0,
        }
    }

    /// Return `true` when the caller should run the analog-mode exchange.
    /// `connected` false (nothing on the port) resets the attempt budget.
    #[optimize(size)]
    pub fn poll(&mut self, is_analog: bool, connected: bool) -> bool {
        if is_analog {
            self.remaining = 0;
            self.attempts = 0;
            return false;
        }
        if !connected {
            self.remaining = 0;
            self.attempts = 0;
            return false;
        }
        if self.attempts >= ANALOG_RETRY_ATTEMPTS {
            return false;
        }
        if self.remaining == 0 {
            self.remaining = ANALOG_RETRY_POLLS - 1;
            self.attempts += 1;
            return true;
        }
        self.remaining -= 1;
        false
    }
}

/// Consecutive polls a connected pad may answer with an unrecognised ID
/// before its buttons are trusted anyway. One such frame is a garbled
/// exchange (a held button would read as a fresh press); a pad that answers
/// this way every frame is a controller the driver does not classify, and
/// its digital buttons are still the player's input.
pub const UNKNOWN_MODE_TRUST_POLLS: u8 = 3;

/// Convert the DualShock's right-positive horizontal convention into Quake's
/// left-positive yaw/strafe convention.
///
/// Menu navigation intentionally keeps the raw pad convention; this adapter is
/// only for gameplay movement and view commands.
#[optimize(size)]
pub const fn quake_horizontal_axis(value: i16) -> i16 {
    let inverted = value.saturating_neg();
    if inverted < -127 {
        -127
    } else if inverted > 127 {
        127
    } else {
        inverted
    }
}

/// Preserve the DualShock's up-negative vertical convention for Quake pitch.
///
/// Quake also uses negative pitch for looking up. HL-PSX negates this axis
/// because its camera pitch convention is opposite; copying that sign into
/// Quake made physical up look down.
#[optimize(size)]
pub const fn quake_look_pitch_axis(value: i16) -> i16 {
    if value < -127 {
        -127
    } else if value > 127 {
        127
    } else {
        value
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct InputFrame {
    /// Forward/back and strafe intent in `-127..=127`.
    pub movement: [i16; 2],
    /// Yaw/pitch intent in `-127..=127` after the shared fine-aim curve.
    pub look: [i16; 2],
    pub held: u16,
    pub pressed: u16,
    /// Physical D-pad edges plus left-stick virtual D-pad edges for menus.
    pub menu_pressed: u16,
}

impl InputFrame {
    #[optimize(size)]
    pub const fn fire_held(self) -> bool {
        self.held & button::R2 != 0
    }

    #[optimize(size)]
    pub const fn fire_pressed(self) -> bool {
        self.pressed & button::R2 != 0
    }

    #[optimize(size)]
    pub const fn jump_held(self) -> bool {
        self.held & button::CROSS != 0
    }

    #[optimize(size)]
    pub const fn jump_pressed(self) -> bool {
        self.pressed & button::CROSS != 0
    }

    #[optimize(size)]
    pub const fn use_pressed(self) -> bool {
        self.pressed & button::SQUARE != 0
    }

    #[optimize(size)]
    pub const fn menu_pressed(self) -> bool {
        self.pressed & (button::START | button::SELECT) != 0
    }

    #[optimize(size)]
    pub const fn next_weapon_pressed(self) -> bool {
        self.pressed & button::R1 != 0
    }

    #[optimize(size)]
    pub const fn previous_weapon_pressed(self) -> bool {
        self.pressed & button::L1 != 0
    }

    /// Quake impulses 1 through 8 on a Triangle-held D-pad wheel.
    ///
    /// Starting at Up and walking clockwise selects Axe, Shotgun, Super
    /// Shotgun, Nailgun, Super Nailgun, Grenade Launcher, Rocket Launcher and
    /// Lightning. The edge may be either Triangle (direction already held) or
    /// a direction (Triangle already held), so the chord is order-independent
    /// and held input never repeats. Opposing or three-way directions are
    /// ignored instead of guessed.
    #[optimize(size)]
    pub const fn direct_weapon_impulse(self) -> Option<u8> {
        if self.held & button::TRIANGLE == 0
            || self.held & DPAD == 0
            || self.pressed & (button::TRIANGLE | DPAD) == 0
        {
            return None;
        }
        match self.held & DPAD {
            button::UP => Some(1),
            bits if bits == button::UP | button::RIGHT => Some(2),
            button::RIGHT => Some(3),
            bits if bits == button::RIGHT | button::DOWN => Some(4),
            button::DOWN => Some(5),
            bits if bits == button::DOWN | button::LEFT => Some(6),
            button::LEFT => Some(7),
            bits if bits == button::LEFT | button::UP => Some(8),
            _ => None,
        }
    }

    /// Digital movement yields to the Triangle weapon chord. Analog movement
    /// remains live, and menu navigation reads `menu_pressed` independently.
    #[optimize(size)]
    pub const fn dpad_moves(self) -> bool {
        self.held & button::TRIANGLE == 0
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ButtonEdges {
    pub pressed: u16,
    pub menu_pressed: u16,
}

/// Tracks gameplay and menu edges independently, matching the C port's
/// left-stick-as-D-pad menu behavior without quantizing analog gameplay.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct EdgeTracker {
    previous_buttons: u16,
    previous_menu_buttons: u16,
}

impl EdgeTracker {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            previous_buttons: 0,
            previous_menu_buttons: 0,
        }
    }

    #[optimize(size)]
    pub fn update(&mut self, held: u16, left_x: i16, left_y: i16) -> ButtonEdges {
        let pressed = held & !self.previous_buttons;
        self.previous_buttons = held;

        let mut menu_buttons = held;
        if left_y < -MENU_STICK_THRESHOLD {
            menu_buttons |= button::UP;
        }
        if left_y > MENU_STICK_THRESHOLD {
            menu_buttons |= button::DOWN;
        }
        if left_x < -MENU_STICK_THRESHOLD {
            menu_buttons |= button::LEFT;
        }
        if left_x > MENU_STICK_THRESHOLD {
            menu_buttons |= button::RIGHT;
        }
        let menu_pressed = menu_buttons & !self.previous_menu_buttons;
        self.previous_menu_buttons = menu_buttons;

        ButtonEdges {
            pressed,
            menu_pressed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    fn frame(pressed: u16) -> InputFrame {
        InputFrame {
            pressed,
            ..InputFrame::default()
        }
    }

    #[optimize(size)]
    #[test]
    fn shipping_actions_match_half_life_for_equivalent_actions() {
        assert!(frame(button::R1).next_weapon_pressed());
        assert!(frame(button::L1).previous_weapon_pressed());
        assert!(!frame(button::TRIANGLE).next_weapon_pressed());
        assert!(!frame(button::CIRCLE).previous_weapon_pressed());
        assert!(frame(button::START).menu_pressed());
        assert!(frame(button::SELECT).menu_pressed());
        assert!(frame(button::SQUARE).use_pressed());
        assert!(frame(button::R2).fire_pressed());
        assert!(frame(button::CROSS).jump_pressed());
    }

    #[optimize(size)]
    #[test]
    fn triangle_dpad_maps_all_quake_impulses_clockwise() {
        let directions = [
            button::UP,
            button::UP | button::RIGHT,
            button::RIGHT,
            button::RIGHT | button::DOWN,
            button::DOWN,
            button::DOWN | button::LEFT,
            button::LEFT,
            button::LEFT | button::UP,
        ];
        for (index, direction) in directions.into_iter().enumerate() {
            let chord = InputFrame {
                held: button::TRIANGLE | direction,
                pressed: direction,
                ..InputFrame::default()
            };
            assert_eq!(chord.direct_weapon_impulse(), Some(index as u8 + 1));
            assert!(!chord.dpad_moves());

            let triangle_last = InputFrame {
                pressed: button::TRIANGLE,
                ..chord
            };
            assert_eq!(triangle_last.direct_weapon_impulse(), Some(index as u8 + 1));
        }
    }

    #[optimize(size)]
    #[test]
    fn direct_weapon_chord_is_edged_and_rejects_ambiguous_directions() {
        let held = InputFrame {
            held: button::TRIANGLE | button::UP,
            ..InputFrame::default()
        };
        assert_eq!(held.direct_weapon_impulse(), None);
        assert!(!held.dpad_moves());

        let contradictory = InputFrame {
            held: button::TRIANGLE | button::UP | button::DOWN,
            pressed: button::DOWN,
            ..InputFrame::default()
        };
        assert_eq!(contradictory.direct_weapon_impulse(), None);

        let ordinary_move = InputFrame {
            held: button::UP,
            pressed: button::UP,
            ..InputFrame::default()
        };
        assert_eq!(ordinary_move.direct_weapon_impulse(), None);
        assert!(ordinary_move.dpad_moves());
    }

    #[optimize(size)]
    #[test]
    fn both_gameplay_sticks_compensate_for_quake_handedness() {
        // A physical push to the right is positive in the DualShock report,
        // but negative is right in Quake's yaw and strafe conventions.
        assert_eq!(quake_horizontal_axis(127), -127);
        assert_eq!(quake_horizontal_axis(48), -48);
        assert_eq!(quake_horizontal_axis(-48), 48);
        assert_eq!(quake_horizontal_axis(-128), 127);
        assert_eq!(quake_horizontal_axis(0), 0);
    }

    #[optimize(size)]
    #[test]
    fn right_stick_vertical_matches_quake_pitch_handedness() {
        // DualShock up is negative and Quake's look-up pitch is negative.
        assert_eq!(quake_look_pitch_axis(-127), -127);
        assert_eq!(quake_look_pitch_axis(-48), -48);
        assert_eq!(quake_look_pitch_axis(48), 48);
        assert_eq!(quake_look_pitch_axis(127), 127);
        assert_eq!(quake_look_pitch_axis(-128), -127);
        assert_eq!(quake_look_pitch_axis(0), 0);
    }

    #[optimize(size)]
    #[test]
    fn analog_menu_directions_are_edges_and_rearm_after_recentering() {
        let mut tracker = EdgeTracker::new();
        assert_eq!(tracker.update(0, 0, -49).menu_pressed, button::UP);
        assert_eq!(tracker.update(0, 0, -127).menu_pressed, 0);
        assert_eq!(tracker.update(0, 0, -48).menu_pressed, 0);
        assert_eq!(tracker.update(0, 0, -49).menu_pressed, button::UP);
    }

    #[optimize(size)]
    #[test]
    fn physical_and_virtual_menu_edges_coexist_without_gameplay_edges() {
        let mut tracker = EdgeTracker::new();
        let first = tracker.update(button::CROSS, 49, 0);
        assert_eq!(first.pressed, button::CROSS);
        assert_eq!(first.menu_pressed, button::CROSS | button::RIGHT);

        let held = tracker.update(button::CROSS, 80, 0);
        assert_eq!(held, ButtonEdges::default());

        let left = tracker.update(0, -49, 0);
        assert_eq!(left.pressed, 0);
        assert_eq!(left.menu_pressed, button::LEFT);
    }

    #[optimize(size)]
    #[test]
    fn non_analog_pad_retries_immediately_then_every_fifteen_polls() {
        let mut retry = AnalogRetry::new();
        assert!(retry.poll(false, true));
        for _ in 1..ANALOG_RETRY_POLLS {
            assert!(!retry.poll(false, true));
        }
        assert!(retry.poll(false, true));

        assert!(!retry.poll(true, true));
        assert!(retry.poll(false, true));
    }
}
