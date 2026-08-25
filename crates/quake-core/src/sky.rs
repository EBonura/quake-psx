//! Quake's view-direction sky projection.
//!
//! Sky brush polygons define only the aperture through which the sky is seen.
//! Their authored surface UVs must not make the sky look attached to the
//! nearby brush. The original renderer instead derives UVs from the ray from
//! the camera to each polygon vertex, with a three-times-flattened vertical
//! component.

use psx_math::int32::isqrt_i32;

/// Return signed material-relative texel coordinates for a Quake sky ray.
///
/// Keeping this signed until a small raster cell is emitted is important.
/// Casting the full dome to `u8` at a brush corner can cross the byte seam and
/// makes the PS1 interpolate through most of the texture between adjacent
/// vertices.
pub fn directional_texel(mut direction: [i32; 3], layer_width: u8) -> [i32; 2] {
    direction[2] = direction[2].saturating_mul(3);

    // Keep the squared length inside i32 without changing the direction.
    while direction[0]
        .unsigned_abs()
        .max(direction[1].unsigned_abs())
        .max(direction[2].unsigned_abs())
        > 16_000
    {
        direction[0] >>= 1;
        direction[1] >>= 1;
        direction[2] >>= 1;
    }

    let length_squared = direction[0]
        .saturating_mul(direction[0])
        .saturating_add(direction[1].saturating_mul(direction[1]))
        .saturating_add(direction[2].saturating_mul(direction[2]));
    let length = isqrt_i32(length_squared).max(1);
    // Every component is within +-16,000 here and `layer_width` is a byte, so
    // the numerator stays below 1.55e9 and the denominator below 3.6e6: the
    // whole projection fits i32 (one hardware `div` instead of the 64-bit
    // software routine; `i32_projection_matches_i64` pins the equivalence).
    let denominator = length * 128;
    let project = |component: i32| {
        // Original Quake uses `6 * 63 / length` against a 128-texel layer.
        // Preserve that projection while scaling it to the selected sky mip.
        let numerator = component * 378 * i32::from(layer_width);
        numerator / denominator
    };

    [project(direction[0]), project(direction[1])]
}

/// Recover a world-space viewing ray from one screen coordinate.
///
/// `world_to_view_q12` is the rotation already loaded for the frame. Its
/// transpose is sufficient here because Quake's coordinate conversion has a
/// uniform scale, which disappears during sky normalisation.
pub fn screen_view_ray(
    screen: [i16; 2],
    center: [i16; 2],
    projection: i16,
    world_to_view_q12: [[i16; 3]; 3],
) -> [i32; 3] {
    let camera = [
        i64::from(screen[0] - center[0]),
        i64::from(screen[1] - center[1]),
        i64::from(projection),
    ];
    let mut world = [0i32; 3];
    for axis in 0..3 {
        let value = camera[0] * i64::from(world_to_view_q12[0][axis])
            + camera[1] * i64::from(world_to_view_q12[1][axis])
            + camera[2] * i64::from(world_to_view_q12[2][axis]);
        world[axis] = (value >> 12) as i32;
    }
    world
}

/// Rebase four signed sky samples into packet UV bytes without changing the
/// local projection gradient.
///
/// The texture window repeats every `period` texels, but that does not make
/// the shortest wrapped delta the correct affine gradient. Near the bottom of
/// Quake's flattened dome, adjacent 32-pixel samples can legitimately differ
/// by slightly more than half a sky tile. Folding that delta into
/// `-period / 2..period / 2` reverses the interpolation direction and creates
/// a radial streak. Keep the original signed deltas and translate the whole
/// packet by complete periods until all four coordinates fit in GP0's byte
/// UVs.
pub fn packet_quad_uv(
    samples: [[i32; 2]; 4],
    atlas: [u8; 2],
    period: [u8; 2],
    scroll: [u8; 2],
) -> [[u8; 2]; 4] {
    let mut output = [[0u8; 2]; 4];
    for axis in 0..2 {
        let period = i32::from(period[axis]).max(1);
        let scroll = i32::from(scroll[axis]);
        let sample_anchor = samples[0][axis];
        let anchor = sample_anchor + i32::from(atlas[axis]) + scroll;
        let mut values = [0i32; 4];
        values[0] = anchor;
        for index in 1..4 {
            values[index] = anchor + samples[index][axis] - sample_anchor;
        }

        let mut minimum = values[0];
        let mut maximum = values[0];
        for value in &values[1..] {
            minimum = minimum.min(*value);
            maximum = maximum.max(*value);
        }
        while minimum < 0 {
            for value in &mut values {
                *value += period;
            }
            minimum += period;
            maximum += period;
        }
        while maximum > 255 {
            for value in &mut values {
                *value -= period;
            }
            minimum -= period;
            maximum -= period;
        }
        debug_assert!(minimum >= 0 && maximum <= 255);
        for index in 0..4 {
            output[index][axis] = values[index] as u8;
        }
    }
    output
}

/// Return material-relative UV bytes for a layered Quake sky vertex.
///
/// `camera_origin_q12` uses the simulation's Q12 world coordinates while the
/// retained BSP vertex is stored in whole world units. `layer_width` is the
/// width of one cooked sky half; the original direction scale targets a
/// 128-texel layer and is reduced proportionally for cooked mip levels.
pub fn directional_uv(
    vertex_units: [i16; 3],
    camera_origin_q12: [i32; 3],
    layer_width: u8,
) -> [u8; 2] {
    let direction = [
        i32::from(vertex_units[0]).saturating_sub(camera_origin_q12[0] >> 12),
        i32::from(vertex_units[1]).saturating_sub(camera_origin_q12[1] >> 12),
        i32::from(vertex_units[2]).saturating_sub(camera_origin_q12[2] >> 12),
    ];
    let projected = directional_texel(direction, layer_width);
    [projected[0] as u8, projected[1] as u8]
}

#[cfg(test)]
mod tests {
    use super::{directional_texel, directional_uv, packet_quad_uv, screen_view_ray};

    #[test]
    fn i32_projection_matches_i64() {
        // The pre-halving reference form, over the full input domain: any i32
        // direction (the halving loop and saturating multiply run first in
        // both), every layer width the cooker can produce.
        fn reference(mut direction: [i32; 3], layer_width: u8) -> [i32; 2] {
            direction[2] = direction[2].saturating_mul(3);
            while direction[0]
                .unsigned_abs()
                .max(direction[1].unsigned_abs())
                .max(direction[2].unsigned_abs())
                > 16_000
            {
                direction[0] >>= 1;
                direction[1] >>= 1;
                direction[2] >>= 1;
            }
            let length_squared = direction[0]
                .saturating_mul(direction[0])
                .saturating_add(direction[1].saturating_mul(direction[1]))
                .saturating_add(direction[2].saturating_mul(direction[2]));
            let length = super::isqrt_i32(length_squared).max(1);
            let denominator = i64::from(length) * 128;
            let project = |component: i32| {
                let numerator = i64::from(component) * 378 * i64::from(layer_width);
                (numerator / denominator) as i32
            };
            [project(direction[0]), project(direction[1])]
        }
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            let r = next();
            let shift = (r >> 58) as u32 % 32;
            let direction = [
                (r as i32) >> shift,
                ((r >> 20) as i32) >> shift,
                ((r >> 40) as i32) >> shift,
            ];
            for layer_width in [8u8, 16, 32, 64, 128, 255] {
                assert_eq!(
                    directional_texel(direction, layer_width),
                    reference(direction, layer_width),
                    "{direction:?} {layer_width}"
                );
            }
        }
        for direction in [
            [i32::MAX, i32::MAX, i32::MAX],
            [i32::MIN, i32::MIN, i32::MIN],
            [0, 0, 0],
            [16_000, -16_000, 5_333],
            [16_001, 0, 0],
            [-1, 1, -1],
        ] {
            for layer_width in [1u8, 128, 255] {
                assert_eq!(directional_texel(direction, layer_width), reference(direction, layer_width));
            }
        }
    }

    #[test]
    fn distance_does_not_create_sky_parallax() {
        assert_eq!(
            directional_uv([100, 0, 0], [0, 0, 0], 64),
            directional_uv([200, 0, 0], [0, 0, 0], 64)
        );
    }

    #[test]
    fn translating_camera_and_aperture_together_keeps_the_sky_fixed() {
        let original = directional_uv([100, -40, 25], [0, 0, 0], 64);
        let translated =
            directional_uv([1_100, -540, 225], [1_000 << 12, -500 << 12, 200 << 12], 64);
        assert_eq!(translated, original);
    }

    #[test]
    fn vertical_flattening_projects_onto_the_distant_dome() {
        assert_eq!(directional_uv([100, 0, 0], [0, 0, 0], 64), [189, 0]);
        let elevated = directional_uv([100, 0, 100], [0, 0, 0], 64);
        assert!(elevated[0] < 64);
        assert_eq!(elevated[1], 0);
    }

    #[test]
    fn selected_mip_scales_directional_coordinates() {
        let full = directional_uv([100, 0, 0], [0, 0, 0], 128);
        let half = directional_uv([100, 0, 0], [0, 0, 0], 64);
        assert_eq!(full[0], half[0].wrapping_mul(2));
    }

    #[test]
    fn quake_basis_center_ray_points_forward() {
        let basis = [[0, -0x3000, 0], [0, 0, -0x3000], [0x3000, 0, 0]];
        assert_eq!(
            screen_view_ray([160, 120], [160, 120], 160, basis),
            [480, 0, 0]
        );
        assert_eq!(directional_texel([480, 0, 0], 64), [189, 0]);
    }

    #[test]
    fn packet_uv_crosses_a_tile_seam_locally() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [0, 0],
        );
        assert_eq!(uv, [[61, 36], [67, 36], [61, 42], [67, 42]]);
        assert_eq!(uv[1][0] - uv[0][0], 6);
    }

    #[test]
    fn scrolling_cannot_reintroduce_a_packet_seam() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [98, 151],
        );
        for axis in 0..2 {
            let minimum = uv.iter().map(|sample| sample[axis]).min().unwrap();
            let maximum = uv.iter().map(|sample| sample[axis]).max().unwrap();
            assert!(maximum - minimum <= 6);
        }
    }

    #[test]
    fn more_than_half_period_gradient_keeps_its_direction() {
        let uv = packet_quad_uv(
            [[372, 0], [366, -66], [376, 0], [371, -63]],
            [0, 0],
            [128, 128],
            [0, 0],
        );

        assert_eq!(uv, [[244, 128], [238, 62], [248, 128], [243, 65]]);
        assert_eq!(i16::from(uv[1][1]) - i16::from(uv[0][1]), -66);
    }
}
