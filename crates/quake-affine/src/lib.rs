// SPDX-License-Identifier: GPL-2.0-or-later
//! Quake PSX's classic-affine packet writer.
//!
//! The vertex records Quake's world pass fills, their materialisation, the
//! alias-model writer and the renderer census. World faces and the liquid and
//! sky fans are written by PSoXide's `psx_engine::affine_surface`.

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
    submit_classic_alias_model, submit_classic_alias_view_model, ClassicAffineBatchSurface,
    ClassicAffineIndexedCorner, ClassicAffinePosition, ClassicAffineProfile,
    ClassicAffineSubdivisionRequest, ClassicAffineSubmit, ClassicAffineTopologyCensus,
    ClassicAffineVertex, ClassicAliasFace, ClassicAliasProjectedVertex, ClassicAliasVertex,
};
