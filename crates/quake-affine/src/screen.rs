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

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::projection::{triangle_outside_common_plane, ScreenClipBounds};

    #[test]
    fn outcodes_match_the_common_plane_test_at_every_corner_combination() {
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
                        triangle_outside_common_plane(
                            [a, b, c].map(|p| [p[0] as i32, p[1] as i32]),
                            ScreenClipBounds::new(0, 319, 0, 239)
                        ),
                        (x & y & z) != 0
                    );
                }
            }
        }
    }
}
