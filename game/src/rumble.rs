//! DualShock motors for damage and gunfire.
//!
//! Built only with the `rumble` cargo feature, which needs the SDK's motor
//! API (`psx_pad::Rumble`, `poll_rumble_on`, `enable_rumble_on`, on the pad
//! integration branch). Without the feature every method here is an empty
//! inline function, so the game loop calls them unconditionally and a normal
//! image carries no motor code and no per-frame cost.
//!
//! The large motor answers damage: a level that grows with the health and
//! armour lost, held briefly and then decayed. The small motor is a short
//! kick when a weapon fires. Both are held off while a menu is up and while the
//! player is dead.

#[cfg(feature = "rumble")]
pub use enabled::{Request, Rumbler};

#[cfg(not(feature = "rumble"))]
pub use disabled::{Request, Rumbler};

#[cfg(not(feature = "rumble"))]
mod disabled {
    /// No motors: nothing to ask for.
    pub type Request = ();

    #[derive(Copy, Clone)]
    pub struct Rumbler;

    impl Rumbler {
        #[inline(always)]
        pub const fn new() -> Self {
            Self
        }
        #[inline(always)]
        pub fn hurt(&mut self, _lost: u16) {}
        #[inline(always)]
        pub fn fired(&mut self) {}
        #[inline(always)]
        pub fn quiet(&mut self) {}
        #[inline(always)]
        pub fn tick(&mut self, _ticks: u16) {}
        #[inline(always)]
        pub fn request(&self) -> Request {}
    }
}

#[cfg(feature = "rumble")]
mod enabled {
    use psx_pad::Rumble;

    /// The motor request one poll carries.
    pub type Request = Rumble;

    /// Large-motor floor: below about 0x40 the motor does not turn.
    const LARGE_MIN: u8 = 0x50;
    /// Ticks (60 Hz) the large motor holds full strength before it decays.
    const HURT_HOLD_TICKS: u16 = 6;
    /// Large-motor level lost per tick once the hold has run out.
    const HURT_DECAY: u8 = 12;
    /// Ticks the small motor stays on after a shot.
    const FIRE_TICKS: u16 = 4;
    /// Health plus armour lost at which the large motor reaches full strength.
    const FULL_DAMAGE: u16 = 40;

    #[derive(Copy, Clone)]
    pub struct Rumbler {
        large: u8,
        hold: u16,
        small: u16,
    }

    impl Rumbler {
        pub const fn new() -> Self {
            Self {
                large: 0,
                hold: 0,
                small: 0,
            }
        }

        /// The player lost `lost` points of health and armour this frame.
        pub fn hurt(&mut self, lost: u16) {
            if lost == 0 {
                return;
            }
            let span = 255 - u16::from(LARGE_MIN);
            let level = u16::from(LARGE_MIN) + span * lost.min(FULL_DAMAGE) / FULL_DAMAGE;
            self.large = self.large.max(level as u8);
            self.hold = HURT_HOLD_TICKS;
        }

        /// A weapon fired this frame.
        pub fn fired(&mut self) {
            self.small = FIRE_TICKS;
        }

        /// Stop both motors at once (menu, death, intermission).
        pub fn quiet(&mut self) {
            *self = Self::new();
        }

        /// Age the pulses by `ticks` 60 Hz game ticks.
        pub fn tick(&mut self, ticks: u16) {
            self.small = self.small.saturating_sub(ticks);
            let held = self.hold.min(ticks);
            self.hold -= held;
            let decayed =
                u16::from(self.large).saturating_sub(u16::from(HURT_DECAY) * (ticks - held));
            self.large = if decayed < u16::from(LARGE_MIN) {
                0
            } else {
                decayed as u8
            };
        }

        /// What the motors are asked for on the next poll.
        pub fn request(&self) -> Rumble {
            Rumble::new(self.small != 0, self.large)
        }
    }
}
