//! Fixed-point underwater view-warp policy.
//!
//! The original software renderer remaps the completed 3D view through a
//! pair of sine tables before drawing the status bar. A full-frame copy is a
//! poor fit for the PlayStation's two-buffer VRAM layout, so the console port
//! drives the GTE projection and render camera with the same slow, crossed
//! sine motion. The HUD stays sharp and the effect costs no packets or VRAM.

use psx_math::sin_q12;

/// One frame of the PS1-native underwater projection warp.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WaterWarpSample {
    /// Post-divide horizontal screen shift, in pixels.
    pub offset_x: i16,
    /// Post-divide vertical screen shift, in pixels.
    pub offset_y: i16,
    /// GTE projection-plane distance. This stays above the dry value of 160
    /// so the moving view always has enough overscan to cover the display.
    pub projection_plane: u16,
    /// Render-only camera roll in Quake's 4096-unit turn.
    pub roll: i16,
}

/// Cross four deliberately incommensurate waves at the 60 Hz render tick.
/// Every operation stays 32-bit on the guest.
#[optimize(size)]
pub fn sample(tick_60hz: u32) -> WaterWarpSample {
    let tick = tick_60hz as u16;
    let x = sin_q12(tick.wrapping_mul(37));
    let y = sin_q12(tick.wrapping_mul(29).wrapping_add(0x0400));
    let lens = sin_q12(tick.wrapping_mul(23).wrapping_add(0x0800));
    let roll = sin_q12(tick.wrapping_mul(17).wrapping_add(0x0c00));
    WaterWarpSample {
        offset_x: (x.saturating_mul(3) >> 12) as i16,
        offset_y: (y.saturating_mul(2) >> 12) as i16,
        projection_plane: (165 + (lens.saturating_mul(2) >> 12)) as u16,
        roll: (roll.saturating_mul(10) >> 12) as i16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn warp_is_bounded_and_always_keeps_overscan() {
        for tick in 0..4096 {
            let warp = sample(tick);
            assert!((-3..=3).contains(&warp.offset_x));
            assert!((-2..=2).contains(&warp.offset_y));
            assert!((163..=167).contains(&warp.projection_plane));
            assert!((-10..=10).contains(&warp.roll));
        }
    }

    #[optimize(size)]
    #[test]
    fn crossed_phases_start_in_motion_without_touching_gameplay_state() {
        assert_eq!(
            sample(0),
            WaterWarpSample {
                offset_x: 0,
                offset_y: 2,
                projection_plane: 165,
                roll: -10,
            }
        );
        assert_ne!(sample(1), sample(0));
    }
}
