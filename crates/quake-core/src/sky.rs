//! Quake's view-direction sky projection.
//!
//! Sky brush polygons define only the aperture through which the sky is seen.
//! Their authored surface UVs must not make the sky look attached to the
//! nearby brush. The original renderer instead derives UVs from the ray from
//! the camera to each polygon vertex, with a three-times-flattened vertical
//! component.

// `directional_texel` and `screen_view_ray` are the SDK's (`psx_engine::sky`): the renderer calls
// them there, and `directional_uv` below uses the first.
use psx_engine::sky::directional_texel;

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
pub use psx_engine::sky::packet_quad_uv;

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
    use super::{directional_texel, directional_uv, packet_quad_uv};
    use psx_engine::sky::screen_view_ray;

    /// The renderer's whole lattice: every screen position of the 320x240 frame about its centre,
    /// projection 160, over rotations at the Q12 extremes and two general ones.
    fn lattice_rays() -> impl Iterator<Item = [i32; 3]> {
        let rotations = [
            [[4096i16, 0, 0], [0, 4096, 0], [0, 0, 4096]],
            [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]],
            [[2896, -2896, 0], [1448, 1448, -3547], [2048, 2048, 2896]],
            [[-1234, 3999, -707], [3000, 1000, 2500], [-2000, -1500, 3200]],
        ];
        rotations.into_iter().flat_map(|rotation| {
            (0i16..240).flat_map(move |y| {
                (0i16..320).map(move |x| screen_view_ray([x, y], [160, 120], 160, rotation))
            })
        })
    }

    #[test]
    fn sdk_view_ray_matches_i64_on_the_renderers_lattice() {
        // The SDK clamps the screen offsets to +-2048 and the projection to 1..=2048; the
        // renderer's offsets are within +-160, so the clamp never acts.
        for ((y, x), rotation_index) in (0..4).flat_map(|r| {
            (0i16..240).flat_map(move |y| (0i16..320).map(move |x| ((y, x), r)))
        }) {
            let rotation = [
                [[4096i16, 0, 0], [0, 4096, 0], [0, 0, 4096]],
                [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]],
                [[2896, -2896, 0], [1448, 1448, -3547], [2048, 2048, 2896]],
                [[-1234, 3999, -707], [3000, 1000, 2500], [-2000, -1500, 3200]],
            ][rotation_index];
            let camera = [i64::from(x) - 160, i64::from(y) - 120, 160];
            let mut expected = [0i32; 3];
            for axis in 0..3 {
                let value = camera[0] * i64::from(rotation[0][axis])
                    + camera[1] * i64::from(rotation[1][axis])
                    + camera[2] * i64::from(rotation[2][axis]);
                expected[axis] = (value >> 12) as i32;
            }
            assert_eq!(screen_view_ray([x, y], [160, 120], 160, rotation), expected);
        }
    }

    #[test]
    fn the_renderers_rays_stay_under_the_halving_threshold() {
        // Quake's own copy halved a ray above 16,000 and the SDK's halves from 16,384, so the two
        // differ only for a ray whose largest component (vertical one tripled) lies between them.
        // The lattice stays far below both.
        for ray in lattice_rays() {
            let largest = ray[0].abs().max(ray[1].abs()).max(ray[2].abs() * 3);
            assert!(largest < 2_000, "{ray:?}");
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
