//! Exact coordinate boundary between Quake game space and PSoXide brush space.
//!
//! Quake remains Z-up. PSoXide-authored PXBSP worlds are Y-up. The boundary
//! retains X and exchanges Y/Z without floating-point conversion or scaling.

use psx_bsp::collision::{BrushTransform, Q12_ONE};
use psx_bsp::{Vec3I16 as PsoxideVec3I16, Vec3I32 as PsoxideVec3I32};
use quake_formats::{Vec3I16 as QuakeVec3I16, Vec3I32 as QuakeVec3I32};

/// One Q3.12 3x3 rotation, stored by rows.
pub type RotationQ12 = [[i16; 3]; 3];

pub const IDENTITY_ROTATION_Q12: RotationQ12 = [
    [Q12_ONE as i16, 0, 0],
    [0, Q12_ONE as i16, 0],
    [0, 0, Q12_ONE as i16],
];

/// Re-express one Q20.12 Quake Z-up point as PSoXide Y-up.
#[optimize(size)]
pub const fn quake_point_to_psoxide(point: QuakeVec3I32) -> PsoxideVec3I32 {
    PsoxideVec3I32 {
        x: point.x,
        y: point.z,
        z: point.y,
    }
}

/// Re-express one Q20.12 PSoXide Y-up point as Quake Z-up.
#[optimize(size)]
pub const fn psoxide_point_to_quake(point: PsoxideVec3I32) -> QuakeVec3I32 {
    QuakeVec3I32 {
        x: point.x,
        y: point.z,
        z: point.y,
    }
}

/// Re-express one Q3.12 Quake Z-up normal as PSoXide Y-up.
#[optimize(size)]
pub const fn quake_normal_to_psoxide(normal: QuakeVec3I16) -> PsoxideVec3I16 {
    PsoxideVec3I16 {
        x: normal.x,
        y: normal.z,
        z: normal.y,
    }
}

/// Re-express one Q3.12 PSoXide Y-up normal as Quake Z-up.
#[optimize(size)]
pub const fn psoxide_normal_to_quake(normal: PsoxideVec3I16) -> QuakeVec3I16 {
    QuakeVec3I16 {
        x: normal.x,
        y: normal.z,
        z: normal.y,
    }
}

/// Convert a rotation whose input and output are both Quake Z-up into the
/// equivalent PSoXide Y-up rotation (`A * rotation * A^-1`).
///
/// The axis matrix `A` only exchanges Y/Z and is its own inverse, so this is
/// an exact cell permutation with no Q12 multiplication or rounding.
#[optimize(size)]
pub const fn quake_rotation_to_psoxide(rotation: RotationQ12) -> RotationQ12 {
    [
        [rotation[0][0], rotation[0][2], rotation[0][1]],
        [rotation[2][0], rotation[2][2], rotation[2][1]],
        [rotation[1][0], rotation[1][2], rotation[1][1]],
    ]
}

/// Convert a PSoXide Y-up rotation back to Quake Z-up.
#[optimize(size)]
pub const fn psoxide_rotation_to_quake(rotation: RotationQ12) -> RotationQ12 {
    quake_rotation_to_psoxide(rotation)
}

/// Build the shared transform for a hull whose plane records remain encoded
/// in Quake-local axes while its query/output boundary is PSoXide Y-up.
///
/// Unlike [`quake_rotation_to_psoxide`], the input side remains the raw Quake
/// hull basis. Consequently the matrix is `A * rotation`, not conjugation.
#[optimize(size)]
pub fn quake_raw_hull_transform_to_psoxide(
    origin: QuakeVec3I32,
    rotation: RotationQ12,
) -> BrushTransform {
    let mut transform = BrushTransform::IDENTITY;
    transform.origin = quake_point_to_psoxide(origin);
    transform.rotation.m = [rotation[0], rotation[2], rotation[1]];
    transform
}
