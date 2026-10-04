//! Classic-affine screen rejection for the zero-origin viewport.

/// Branch-minimal outcode for the common zero-origin viewport.
///
/// This retains the exact arithmetic previously embedded in the classic
/// affine packet writer, including its fast all-inside return.
#[inline(always)]
pub(crate) fn zero_origin_screen_outcode(position: [i16; 2], right: i32, bottom: i32) -> u8 {
    let x = position[0] as i32;
    let y = position[1] as i32;
    if (x as u32) <= right as u32 && (y as u32) <= bottom as u32 {
        return 0;
    }
    ((x as u32 >> 31) as u8)
        | ((((right - x) as u32 >> 31) as u8) << 1)
        | (((y as u32 >> 31) as u8) << 2)
        | ((((bottom - y) as u32 >> 31) as u8) << 3)
}

/// Classic-affine triangle screen rejection.
///
/// Preserve the established pairwise rule byte-for-byte rather than silently
/// replacing it with a common-bit test; its packet topology is part of the
/// visual-equivalence contract.
#[inline(always)]
pub(crate) fn classic_triangle_screen_rejected(
    points: [[i16; 2]; 3],
    right: i32,
    bottom: i32,
) -> bool {
    let c0 = zero_origin_screen_outcode(points[0], right, bottom);
    if c0 == 0 {
        return false;
    }
    let c1 = zero_origin_screen_outcode(points[1], right, bottom);
    if (c0 & c1) == 0 {
        return false;
    }
    let c2 = zero_origin_screen_outcode(points[2], right, bottom);
    (c1 & c2) != 0 && (c2 & c0) != 0
}

/// Classic-affine quad screen rejection with the established six pair tests.
#[inline(always)]
pub(crate) fn classic_quad_screen_rejected(points: [[i16; 2]; 4], right: i32, bottom: i32) -> bool {
    let c0 = zero_origin_screen_outcode(points[0], right, bottom);
    if c0 == 0 {
        return false;
    }
    let c1 = zero_origin_screen_outcode(points[1], right, bottom);
    if (c0 & c1) == 0 {
        return false;
    }
    let c2 = zero_origin_screen_outcode(points[2], right, bottom);
    if (c0 & c2) == 0 || (c1 & c2) == 0 {
        return false;
    }
    let c3 = zero_origin_screen_outcode(points[3], right, bottom);
    (c2 & c3) != 0 && (c3 & c0) != 0 && (c1 & c3) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::projection::{triangle_outside_common_plane, ScreenClipBounds};

    #[test]
    fn early_rejection_matches_all_historical_corner_combinations() {
        // Every side/corner code, inclusive edges, and the inside region.
        let positions = [
            [-1, -1],
            [0, -1],
            [320, -1],
            [-1, 0],
            [0, 0],
            [320, 0],
            [-1, 240],
            [0, 240],
            [320, 240],
            [319, 239],
        ];
        for a in positions {
            for b in positions {
                for c in positions {
                    let codes = [a, b, c].map(|p| zero_origin_screen_outcode(p, 319, 239));
                    let [x, y, z] = codes;
                    assert_eq!(
                        classic_triangle_screen_rejected([a, b, c], 319, 239),
                        (x & y) != 0 && (y & z) != 0 && (z & x) != 0
                    );
                    assert_eq!(
                        triangle_outside_common_plane(
                            [a, b, c].map(|p| [p[0] as i32, p[1] as i32]),
                            ScreenClipBounds::new(0, 319, 0, 239)
                        ),
                        (x & y & z) != 0
                    );
                    for d in positions {
                        let w = zero_origin_screen_outcode(d, 319, 239);
                        assert_eq!(
                            classic_quad_screen_rejected([a, b, c, d], 319, 239),
                            (x & y) != 0
                                && (y & z) != 0
                                && (z & w) != 0
                                && (w & x) != 0
                                && (x & z) != 0
                                && (y & w) != 0
                        );
                    }
                }
            }
        }
    }
}
