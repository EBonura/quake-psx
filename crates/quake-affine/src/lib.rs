// SPDX-License-Identifier: GPL-2.0-or-later
//! Quake PSX's classic-affine packet writer.
//!
//! The compact world and alias-model submission path Quake renders through:
//! camera-space midpoints are reprojected, near or visibly warped triangles
//! use a fixed two-level lattice, compatible leaves pair into GP0(3Ch) quads,
//! and optional root-edge underdraw seals rasterisation cracks. The packet
//! topology is Quake's, so the writer lives here rather than in PSoXide's
//! shared engine.

#![no_std]
#![cfg_attr(
    target_arch = "mips",
    feature(asm_experimental_arch, optimize_attribute)
)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
#![allow(clippy::too_many_arguments)]

mod classic_affine;
mod screen;

pub use classic_affine::{
    census_classic_affine_projected_batch_topology,
    collect_classic_affine_projected_subdivision_requests,
    materialize_classic_affine_indexed_baked_vertices, materialize_classic_affine_indexed_vertices,
    submit_classic_affine_batch, submit_classic_affine_scoped_windowed_fan,
    submit_classic_alias_model, submit_classic_alias_view_model, ClassicAffineBatchSurface,
    ClassicAffineIndexedCorner, ClassicAffinePosition, ClassicAffineProfile, ClassicAffineSubmit,
    ClassicAffineSubdivisionRequest, ClassicAffineTopologyCensus, ClassicAffineVertex,
    ClassicAliasFace, ClassicAliasProjectedVertex, ClassicAliasVertex,
    WORST_PACKET_WORDS_PER_TRIANGLE,
};
pub use classic_affine::{quake_error_bounded_profile, QUAKE_COARSE_ERROR_BUDGET_Q3};
#[cfg(feature = "classic-affine-quake-specialized-kernel")]
pub use classic_affine::{
    submit_quake_classic_affine_batch, submit_quake_classic_affine_batch_budget,
};
