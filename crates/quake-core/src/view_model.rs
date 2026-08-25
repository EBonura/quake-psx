//! Original PlayStation view-model presentation policy.
//!
//! The retained C renderer magnifies the authored alias-model pose by eight,
//! advances a small horizontal bob from player speed, lights the weapon from
//! the camera leaf, and uses the first uploaded Quake palette row. Keeping
//! those inputs pure makes the packet and transform contract host-testable.

use psx_math::int32::{abs_i32, clamp_i16, mul_q12_i32};
use psx_math::sin_q12;

const VIDEO_TICKS_PER_SECOND: i32 = 60;
const MAGNIFY: i32 = 8;

/// The C renderer boots with gamma zero, the first of the six uploaded rows.
pub const CLUT_BASE_ROW: u16 = 240;
pub const CLUT: u16 = CLUT_BASE_ROW << 6;

/// Magnify one authored alias-model component exactly as `VMODEL_SCALE=3`.
#[optimize(size)]
pub fn magnify_component(value: i16) -> i16 {
    clamp_i16(i32::from(value).saturating_mul(MAGNIFY))
}

/// Magnify an authored X offset and add the original horizontal bob.
#[optimize(size)]
pub fn magnify_x_with_bob(value: i16, bob: i16) -> i16 {
    clamp_i16(
        i32::from(value)
            .saturating_mul(MAGNIFY)
            .saturating_add(i32::from(bob)),
    )
}

/// `V_CalcRefdef`'s `view->origin += forward * bob * 0.4`, in the magnified
/// model-space X (forward) units the offset lane uses. The vertical part of
/// the bob is already on the camera, which this weapon is locked to, so the
/// forward lean is the only relative motion left.
#[optimize(size)]
pub fn bob_forward(bob_q12: i32) -> i16 {
    clamp_i16(mul_q12_i32(bob_q12, 1638) * MAGNIFY >> 12)
}

/// Advance the C renderer's `bob_t` and return its model-space X offset.
///
/// Player velocity is Quake units/second in Q20.12. `elapsed_ticks` is the
/// 60 Hz vblank delta already bounded by the game loop.
#[optimize(size)]
pub fn advance_bob(phase: u16, velocity_x: i32, velocity_y: i32, elapsed_ticks: u16) -> (u16, i16) {
    let speed = abs_i32(velocity_x).max(abs_i32(velocity_y));
    let frame_time_q12 = (i32::from(elapsed_ticks) << 12) / VIDEO_TICKS_PER_SECOND;
    let step = mul_q12_i32(speed, frame_time_q12) >> 7;
    let phase = phase.wrapping_add(step as u16);
    (phase, (sin_q12(phase) >> 9) as i16)
}

/// Repeat one scalar Quake light sample into the flat-textured RGB command.
#[optimize(size)]
pub const fn packet_tint(light: u8) -> u32 {
    let light = light as u32;
    light | (light << 8) | (light << 16)
}

/// Preserve C `EF_MUZZLEFLASH`: boost the previous sample once, otherwise
/// replace it with the current camera-leaf sample.
#[optimize(size)]
pub const fn update_light(previous: u8, camera_leaf: u8, muzzle_flash: bool) -> u8 {
    if muzzle_flash {
        previous.saturating_add(0x80)
    } else {
        camera_leaf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn pose_components_match_vmodel_scale_three() {
        assert_eq!(magnify_component(12), 96);
        assert_eq!(magnify_component(-17), -136);
        assert_eq!(magnify_x_with_bob(12, 7), 103);
        assert_eq!(magnify_x_with_bob(-17, -8), -144);
        assert_eq!(magnify_component(i16::MAX), i16::MAX);
        assert_eq!(magnify_x_with_bob(i16::MIN, -8), i16::MIN);
    }

    #[optimize(size)]
    #[test]
    fn bob_matches_the_c_fixed_point_formula() {
        // 320 units/s for one 60 Hz tick: ((320 * 4096) * (4096 / 60)
        // >> 12) >> 7 = 170 phase units.
        let (phase, bob) = advance_bob(0, 320 << 12, 0, 1);
        assert_eq!(phase, 170);
        assert_eq!(bob, sin_q12(170) as i16 >> 9);

        let (phase, bob) = advance_bob(phase, -(320 << 12), 1, 2);
        assert_eq!(phase, 510);
        assert_eq!(bob, (sin_q12(510) >> 9) as i16);
        assert!((-8..=8).contains(&bob));
    }

    #[optimize(size)]
    #[test]
    fn bob_forward_leans_four_tenths_of_the_camera_bob() {
        assert_eq!(bob_forward(0), 0);
        // 4 units of bob: 1.6 units forward, magnified by eight.
        assert_eq!(bob_forward(4 << 12), 12);
        assert_eq!(bob_forward(-7 << 12), -23);
    }

    #[optimize(size)]
    #[test]
    fn packet_material_matches_the_pinned_c_camera() {
        assert_eq!(CLUT, 240 << 6);
        assert_eq!(packet_tint(120), 0x0078_7878);
        assert_eq!(update_light(0, 120, false), 120);
        assert_eq!(update_light(120, 12, true), 248);
        assert_eq!(packet_tint(248), 0x00f8_f8f8);
    }
}
