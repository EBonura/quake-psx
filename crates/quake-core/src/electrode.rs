//! The map lightning event's two-electrode interlock and beam lifetime.
use crate::mover::QuakeMoverState;
use quake_formats::Vec3I32;

pub const BEAM_TICKS: u16 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeamAdvance {
    pub held_ticks: u16,
    pub expired: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LightningEvent {
    remaining: u16,
}
impl LightningEvent {
    pub const fn active(self) -> bool { self.remaining != 0 }
    /// Returns whether the beam ended and its electrodes must go down.
    pub fn advance(&mut self, ticks: u16) -> BeamAdvance {
        let before = self.remaining;
        self.remaining = self.remaining.saturating_sub(ticks);
        BeamAdvance { held_ticks: before.min(ticks), expired: before != 0 && self.remaining == 0 }
    }
    pub const fn just_started(self) -> bool { self.remaining == BEAM_TICKS }
    pub fn holds_tick(self, advance: BeamAdvance, was_settled: bool,
        current: QuakeMoverState, tick: u16) -> bool {
        let settled = matches!(current, QuakeMoverState::Top | QuakeMoverState::Bottom);
        if self.just_started() { was_settled && settled }
        else { tick < advance.held_ticks && was_settled && (advance.expired || settled) }
    }
    /// A stable matching pair emits a beam; only both Top permit a shock.
    /// The original end >= time + one second guard rejects another use in
    /// the same gameplay tick, not all uses during the beam's lifetime.
    pub fn begin(&mut self, pair: [QuakeMoverState; 2]) -> Option<bool> {
        if self.remaining == BEAM_TICKS || pair[0] != pair[1]
            || !matches!(pair[0], QuakeMoverState::Top | QuakeMoverState::Bottom)
        { return None; }
        self.remaining = BEAM_TICKS;
        Some(pair[0] == QuakeMoverState::Top)
    }
}

/// The visible discharge uses the electrodes' actual translated bounds.
pub fn beam_endpoints(bounds: [[Vec3I32; 2]; 2]) -> [Vec3I32; 2] {
    let centre = |b: [Vec3I32; 2]| Vec3I32 {
        x: b[0].x / 2 + b[1].x / 2,
        y: b[0].y / 2 + b[1].y / 2,
        z: b[0].z.saturating_sub(16 << 12),
    };
    let p1 = centre(bounds[0]);
    let mut p2 = centre(bounds[1]);
    let delta = [(p2.x - p1.x) >> 12, (p2.y - p1.y) >> 12, (p2.z - p1.z) >> 12];
    let length = psx_math::int32::isqrt_u64(delta.iter().map(|&v| (i64::from(v) * i64::from(v)) as u64).sum());
    if length != 0 {
        // `mul_div_i32` is `(v * 100 << 12 / length) as i32` through the wide
        // product, with no 64-bit divide helper in the image.
        // The scale goes through `black_box` so the call reuses the one
        // `mul_div_i32` already in the image instead of cloning a copy for the
        // constant (a 1.1 KB clone otherwise).
        let scale = core::hint::black_box(100 << 12);
        let shorten = |v| psx_math::int32::mul_div_i32(v, scale, length as i32);
        p2.x -= shorten(delta[0]); p2.y -= shorten(delta[1]); p2.z -= shorten(delta[2]);
    }
    [p1, p2]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_interlock_moving_and_mismatched_pairs_never_fire() {
        let mut event = LightningEvent::default();
        for pair in [[QuakeMoverState::Up; 2], [QuakeMoverState::Down; 2],
            [QuakeMoverState::Top, QuakeMoverState::Bottom],
            [QuakeMoverState::Top, QuakeMoverState::Up]] {
            assert_eq!(event.begin(pair), None); assert!(!event.active());
        }
    }
    #[test]
    fn both_bottom_make_a_beam_without_a_boss_shock() {
        let mut event = LightningEvent::default();
        assert_eq!(event.begin([QuakeMoverState::Bottom; 2]), Some(false));
        assert!(!event.advance(59).expired); assert!(event.advance(1).expired); assert!(!event.active());
        assert!(!event.advance(4).expired);
    }
    #[test]
    fn both_top_shock_once_per_tick_and_restart_the_original_beam_deadline() {
        let mut event = LightningEvent::default();
        assert_eq!(event.begin([QuakeMoverState::Top; 2]), Some(true));
        assert_eq!(event.begin([QuakeMoverState::Top; 2]), None);
        assert!(!event.advance(4).expired);
        assert_eq!(event.begin([QuakeMoverState::Top; 2]), Some(true));
        assert!(!event.advance(59).expired); assert!(event.advance(4).expired);
    }
    #[test]
    fn beam_uses_translated_bottoms_and_shortens_second_end() {
        let v = |x,y,z| Vec3I32 { x:x<<12,y:y<<12,z:z<<12 };
        assert_eq!(beam_endpoints([[v(329,249,129),v(391,311,447)],
            [v(329,-183,129),v(391,-121,447)]]),
            [v(360,280,113),v(360,-52,113)]);
    }
    #[test]
    fn beam_shortening_matches_the_wide_division_it_replaced() {
        let mut state = 0x2545_f491u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        for _ in 0..4000 {
            let delta = [
                (next() % 16_384) as i32 - 8_192,
                (next() % 16_384) as i32 - 8_192,
                (next() % 4_096) as i32 - 2_048,
            ];
            let length = psx_math::int32::isqrt_u64(
                delta.iter().map(|&v| (i64::from(v) * i64::from(v)) as u64).sum(),
            );
            if length == 0 {
                continue;
            }
            for v in delta {
                let wide = (i64::from(v) * (100 << 12) / i64::from(length)) as i32;
                assert_eq!(psx_math::int32::mul_div_i32(v, 100 << 12, length as i32), wide);
            }
        }
    }
}
