//! Quake's view-direction sky projection.
//!
//! Sky brush polygons define only the aperture through which the sky is seen.
//! Their authored surface UVs must not make the sky look attached to the
//! nearby brush. The original renderer instead derives UVs from the ray from
//! the camera to each polygon vertex, with a three-times-flattened vertical
//! component.

// The projection, the view-ray recovery and the packet UV rebasing are the SDK's
// (`psx_engine::sky`); Quake keeps no copy and the tests below hold its behaviour.
pub use psx_engine::sky::{directional_texel, directional_uv, packet_quad_uv, screen_view_ray};

#[cfg(test)]
mod tests {
    use super::{directional_texel, directional_uv, packet_quad_uv, screen_view_ray};

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
