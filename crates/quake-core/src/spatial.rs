//! Integer helpers for positional sound.

use psx_math::int32::div_u64_by_u32;

/// The listener's right vector in the floor plane, in Q12: `AngleVectors`'
/// `right` for a level view, `(sin yaw, -cos yaw)`. Quake's y axis points to the
/// listener's left when facing +x, so this is the vector the pan has to be
/// measured against; its negation is the left ear.
pub fn listener_right_q12(yaw: u16) -> (i32, i32) {
    let yaw = yaw & 0x0fff;
    (psx_math::sin_q12(yaw), -psx_math::cos_q12(yaw))
}

/// `S_Spatialize`'s pan: `(dot / distance)` clamped to `[-one, one]`, where
/// `dot` is the 64-bit sum of two products and `distance` a positive 32-bit
/// length. Identical to `(dot / i64::from(distance)).clamp(-one, one)` for every
/// input, with no 64-bit divide helper: the quotient is taken by the SDK's
/// 64 by 32 divide when it fits 32 bits and saturates otherwise (a quotient
/// that wide is far past the clamp).
pub fn pan_q12(dot: i64, distance: u32, one: i32) -> i32 {
    debug_assert!(distance != 0 && one >= 0);
    let magnitude = dot.unsigned_abs();
    let high = (magnitude >> 32) as u32;
    let quotient = if high < distance {
        div_u64_by_u32(high, magnitude as u32, distance)
    } else {
        u32::MAX
    };
    let clamped = quotient.min(one as u32) as i32;
    if dot < 0 {
        -clamped
    } else {
        clamped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source on the player's right must be louder in the right ear. The
    /// right vector is the one the view and the weapons use.
    #[test]
    fn the_listener_right_vector_is_the_views_right() {
        for yaw in (0..4096u16).step_by(37) {
            let (x, y) = listener_right_q12(yaw);
            let (_, right, _) = crate::combat::view_basis([0, yaw as i16, 0]);
            assert!((x - right.x).abs() <= 2 && (y - right.y).abs() <= 2, "yaw {yaw}");
        }
        // Facing +x, the right-hand side is -y.
        assert_eq!(listener_right_q12(0).1, -4096);
    }

    #[test]
    fn matches_the_wide_division_on_edges_and_a_sweep() {
        let one = 4096;
        let reference = |dot: i64, distance: u32| {
            (dot / i64::from(distance)).clamp(i64::from(-one), i64::from(one)) as i32
        };
        let edges: [i64; 9] = [
            0,
            1,
            -1,
            4095,
            -4096,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
            i64::MAX,
            i64::MIN + 1,
        ];
        for &dot in &edges {
            for distance in [1u32, 2, 3, 1000, 4096, 65_535, 1 << 20, u32::MAX] {
                assert_eq!(pan_q12(dot, distance, one), reference(dot, distance), "{dot} / {distance}");
            }
        }
        let mut state = 0x9e37_79b9u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        for _ in 0..200_000 {
            let dot = ((u64::from(next()) << 32 | u64::from(next())) as i64) >> (next() % 40);
            let distance = (next() >> (next() % 32)).max(1);
            assert_eq!(pan_q12(dot, distance, one), reference(dot, distance), "{dot} / {distance}");
        }
    }
}
