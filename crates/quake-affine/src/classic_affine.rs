//! Quake's classic-affine vertex records, their materialisation from indexed
//! corners, the alias-model writer and the renderer census (a diagnostic of
//! the historical depth-band topology). World faces and the liquid and sky
//! fans are written by `psx_engine::affine_surface`.

use core::{mem::size_of, ptr};

use psx_gpu::prim::ClassicTriTextured;
use psx_gte::{
    math::Vec3I16,
    scene::{self, project_triangle_scheduled, project_vertex_scheduled},
};
use psx_pack::WordFnv32;

use crate::screen::zero_origin_screen_outcode;

/// Mutable vertex layout shared with retained renderers.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineVertex {
    /// Camera-space position consumed by the GTE.
    pub position: [i16; 3],
    /// Packet-space UV bytes.
    pub uv: [u8; 2],
    /// RGB in the low 24 bits.
    pub color: u32,
    /// Projected screen coordinate.
    pub screen: [i16; 2],
    /// Cached GTE SZ value.
    pub depth: i32,
}

/// Indexed retained-world corner with material attributes separated from its
/// shared position.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineIndexedCorner {
    /// Index into the caller-owned shared-position array.
    pub position_index: u16,
    /// Material-relative or baked atlas UV.
    pub uv: [u8; 2],
    /// Two light contributions in the low bytes or baked RGB.
    pub light: u32,
}

const _: [(); 8] = [(); size_of::<ClassicAffineIndexedCorner>()];

/// Expand indexed corners and shared positions into projection scratch.
///
/// # Safety
/// `corners` and `destination` must contain `vertex_count` records, every
/// corner index must address `positions`, and all ranges must be aligned and
/// non-overlapping.
pub unsafe fn materialize_classic_affine_indexed_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
    baked_uv: bool,
    baked_light: bool,
) {
    let mut index = 0usize;
    while index < vertex_count {
        let corner = unsafe { ptr::read(corners.add(index)) };
        let position_index = corner.position_index as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let uv = if baked_uv {
            corner.uv
        } else {
            [
                corner.uv[0].wrapping_add(uv_offset[0]),
                corner.uv[1].wrapping_add(uv_offset[1]),
            ]
        };
        let color = if baked_light {
            corner.light
        } else {
            light_color(
                [corner.light as u8, (corner.light >> 8) as u8],
                light_weights,
            )
        };
        // Projection overwrites screen/depth before either field is read. As
        // with the retained word-source path, initialize only the first three
        // words and avoid eight bytes of dead stores per visible corner.
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv = u32::from(position.position[2] as u16)
            | (u32::from(uv[0]) << 16)
            | (u32::from(uv[1]) << 24);
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Expand indexed corners whose UV and RGB attributes were both baked by the
/// cooker.
///
/// This is the dominant static-world contract in Quake-derived maps. Selecting
/// it once per face removes the two per-corner format branches, the light-style
/// arithmetic, and the dead UV-offset inputs from the hot materialisation loop.
/// The corner's two aligned words can also be copied directly around the shared
/// position lookup.
///
/// # Safety
/// `corners` and `destination` must contain `vertex_count` records, every
/// corner index must address `positions`, and all ranges must be aligned and
/// non-overlapping.
pub unsafe fn materialize_classic_affine_indexed_baked_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
) {
    let mut index = 0usize;
    while index < vertex_count {
        let corner_words = unsafe { corners.add(index).cast::<u32>() };
        let position_and_uv = unsafe { ptr::read(corner_words) };
        let position_index = position_and_uv as u16 as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv =
            u32::from(position.position[2] as u16) | (position_and_uv & 0xffff_0000);
        let color = unsafe { ptr::read(corner_words.add(1)) };
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Deduplicated signed-integer position consumed by the retained indexed
/// world projection batch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffinePosition {
    /// Position in the 3D space expected by the currently loaded GTE matrix.
    pub position: [i16; 3],
}

/// One fan inside a contiguous classic-affine vertex batch: the SDK's world
/// surface descriptor.
pub type ClassicAffineBatchSurface = psx_engine::affine_surface::WorldSurface;

/// Indexed source attributes for one fan in a fused materialize/project batch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineIndexedBatchSource {
    /// First corner in the retained indexed-corner array.
    pub first_corner: u32,
    /// Texture-atlas offset used when UVs were not baked by the cooker.
    pub uv_offset: [u8; 2],
    /// Bit zero marks baked UVs; bit one marks baked RGB.
    pub format: u16,
    /// Current Q8 contributions of the face's two light styles.
    pub light_weights: [u16; 2],
}

const _: [(); 12] = [(); size_of::<ClassicAffineIndexedBatchSource>()];

/// Fixed topology and packet bounds for classic affine submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineProfile {
    /// Viewport width.
    pub screen_width: i16,
    /// Viewport height.
    pub screen_height: i16,
    /// Number of ordering-table slots.
    pub ot_depth: u16,
    /// OTZ below which one subdivision level is used.
    pub subdivide_once_at: u16,
    /// OTZ below which two subdivision levels are used.
    pub subdivide_twice_at: u16,
    /// Predicted affine error in texels above which one level is used.
    ///
    /// Zero disables the error trigger and retains the historical depth-only
    /// schedule.
    pub subdivide_once_error_texels: u8,
    /// Predicted affine error in texels above which two levels are used.
    ///
    /// Zero disables the error trigger. This must be no smaller than
    /// [`Self::subdivide_once_error_texels`] when both are enabled.
    pub subdivide_twice_error_texels: u8,
    /// Screen-space affine error budget in 1/8 pixel. Non-zero selects the
    /// error-bounded policy (see [`psx_engine::tess`]): a face whose screen extent
    /// and depth range cannot exceed the budget keeps the historical
    /// unsplit fan; any other face is split as a fan of quads (plus at most
    /// one triangle), each quad as a lattice with a level per axis from its
    /// own edges. The depth bands then only gate crack-sealing underdraw
    /// (and should be zero or the historical fan splits too): a split
    /// boundary edge is sealed when a corner's depth reaches
    /// [`Self::subdivide_once_at`] (level one) or
    /// [`Self::subdivide_twice_at`] (level two). Zero keeps the historical
    /// schedule.
    pub subdivide_error_px_q3: u8,
    /// Band schedule only ([`Self::subdivide_error_px_q3`] zero): split
    /// four-corner faces as a quad lattice (GT4 cells) instead of two
    /// triangle lattices. The face takes the deeper of its two roots' band
    /// levels, on each axis whose edges change depth.
    pub quad_lattice: bool,
    /// Error-bounded policy only: faces whose nearest vertex is at least this
    /// deep (GTE SZ units) take level zero without the screen-extent pass.
    /// Sound when it is at least `F * sqrt(H / (2 * budget_px))` for the
    /// largest face extent `F` the cooker emits (camera-space units) and
    /// projection distance `H`: such a face's affine displacement cannot
    /// reach the budget. Zero tests every face.
    pub error_gate_depth: u16,
    /// Minimum OT slot bias of crack-sealing underdraw behind its parent's
    /// average (see [`underlay_otz`]).
    pub underdraw_slot_bias: u16,
}

impl ClassicAffineProfile {
    /// Historical 320x240 Quake-style profile with 2,048 OT slots.
    pub const QUAKE_REFERENCE: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        subdivide_once_at: 136,
        subdivide_twice_at: 60,
        subdivide_once_error_texels: 0,
        subdivide_twice_error_texels: 0,
        subdivide_error_px_q3: 0,
        quad_lattice: false,
        error_gate_depth: 0,
        underdraw_slot_bias: 8,
    };

    /// Experimental bounded-lattice affine-error profile.
    ///
    /// The depth bands preserve the historical close-surface workload. The
    /// additional error trigger uses the measured p90 affine-error bound and
    /// spends one split above four predicted texels, then the existing second
    /// split above eight. A bisection can reduce the worst near-side edge
    /// error by as little as half, so the eight-texel threshold keeps each
    /// resulting edge near the four-texel budget without introducing a third
    /// lattice level or changing packet-capacity bounds. This profile is not
    /// selected by a shipping world renderer until that renderer also owns a
    /// hard per-frame extra-packet budget. A fixed-camera Quake measurement
    /// showed that selecting it globally could increase modeled GPU cost by
    /// 82 percent and reach the emulator's 4,096-draw census envelope.
    pub const RUNTIME_ADAPTIVE: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        subdivide_once_at: 136,
        subdivide_twice_at: 60,
        subdivide_once_error_texels: 4,
        subdivide_twice_error_texels: 8,
        subdivide_error_px_q3: 0,
        quad_lattice: false,
        error_gate_depth: 0,
        underdraw_slot_bias: 8,
    };

    /// PSoXide brush-world profile: the same topology as
    /// [`Self::QUAKE_REFERENCE`] but each subdivision band reaches 2.5 times
    /// as far. Quake's first-person camera looks at walls square-on; a
    /// third-person camera looks down at the floor from a few dozen units
    /// up, and 128-unit patches split only within ~80 units left ~100 px
    /// affine triangles underfoot that swim whenever the view pitches.
    /// 340/170 (from 272/136) measured on the Cortex 0.4 whole-level tape:
    /// textured pixels over one texel of warp 47.6% -> 36.8%, over two
    /// 30.4% -> 18.8%, -3.5% gameplay fps; the bands are compile-time
    /// constants, so the writer's stack and RAM are unchanged.
    pub const PXBSP_THIRD_PERSON: Self = Self {
        subdivide_once_at: 340,
        subdivide_twice_at: 170,
        ..Self::QUAKE_REFERENCE
    };

    /// quake-psx's world profile with feature `classic-affine-lattice`:
    /// [`Self::QUAKE_REFERENCE`]'s viewport and OT with the error-bounded
    /// fan policy at a 16-pixel budget, every split edge sealed. The gate
    /// (930 SZ) is `F * sqrt(H / (2 * 16))` for qbsp's 240-unit face cuts
    /// (`F` = 416, a 240-unit cube's diagonal) at `H` = 160; zoomed views
    /// (larger `H`) can leave faces just past it a little under-split.
    pub const QUAKE_ERROR_BOUNDED: Self = Self {
        subdivide_once_at: 0,
        subdivide_twice_at: 0,
        subdivide_error_px_q3: 128,
        error_gate_depth: 930,
        ..Self::QUAKE_REFERENCE
    };
}

/// Result of one fan submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineSubmit {
    /// First packet word after the emitted stream.
    pub next_packet: *mut u32,
    /// Number of compact GPU packets emitted.
    pub packets: u32,
    /// Number of hardware triangles represented by those packets.
    pub hardware_triangles: u32,
}

/// Diagnostic description of the camera-dependent topology selected for a
/// sequence of already projected classic-affine fans.
///
/// The packet and byte counts are the exact pre-screen-rejection topology.
/// Comparing them with the submitter's actual output isolates packets removed
/// by polygon-level screen rejection without adding counters to the shipping
/// packet writer.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineTopologyCensus {
    /// Convex fans examined.
    pub surfaces: u32,
    /// Source fan triangles, including roots rejected before subdivision.
    pub root_triangles: u32,
    /// Fans rejected by the shared four-way screen clip code.
    pub surface_clip_rejects: u32,
    /// Root triangles rejected by the OTZ range.
    pub depth_rejects: u32,
    /// Root triangles emitted without subdivision.
    pub level0_root_triangles: u32,
    /// Root triangles expanded through the one-level lattice.
    pub level1_root_triangles: u32,
    /// Root triangles expanded through the two-level lattice.
    pub level2_root_triangles: u32,
    /// Level-zero GT4 packets formed by pairing adjacent fan triangles.
    pub paired_level0_packets: u32,
    /// One-level roots that require crack-sealing underdraw.
    pub level1_underdraw_roots: u32,
    /// Two-level roots that require crack-sealing underdraw.
    pub level2_underdraw_roots: u32,
    /// Packets selected before individual polygon screen rejection.
    pub theoretical_packets: u32,
    /// Hardware triangles represented by the theoretical packet stream.
    pub theoretical_hardware_triangles: u32,
    /// Bytes occupied by the theoretical compact GT3/GT4 packet stream.
    pub theoretical_packet_bytes: u32,
    /// First deterministic topology fingerprint lane.
    pub topology_hash_a: u32,
    /// Second deterministic topology fingerprint lane.
    pub topology_hash_b: u32,
}

/// One camera-selected adaptive-subdivision root requested by an already
/// projected compact batch.
///
/// This deliberately identifies the root only within its batch surface. The
/// caller owns the stable source-face identity needed for a persistent cache.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineSubdivisionRequest {
    /// Index of the owning surface descriptor in the submitted batch.
    pub batch_surface: u8,
    /// Fan-root index (`0` is corners 0/1/2, `1` is corners 0/2/3).
    pub root: u8,
    /// Selected subdivision lattice level: one or two.
    pub level: u8,
    /// Non-zero when the root also requires crack-sealing underdraw packets.
    pub underdraw: u8,
    /// Ordering-table depth selected for the source root.
    pub otz: u16,
    /// Complete compact packet footprint for this root.
    pub packet_bytes: u16,
    /// Bytes whose UV, colour, CLUT, TPAGE and command fields are invariant.
    pub invariant_bytes: u16,
    /// Explicit padding kept deterministic across targets.
    pub _padding: u16,
    /// Packed CLUT/TPAGE identity from the source surface.
    pub material: u32,
}

/// Indexed face used by classic alias-model batches.
///
/// Each word keeps its packet UV in the low half and projected-cache byte
/// offset in the high half, matching the retained Quake model cooker.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasFace {
    /// Three packed UV16|projected-offset16 corner words.
    pub corners: [u32; 3],
}

/// Compact alias-model position in the original Quake byte representation.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasVertex {
    /// Original unsigned X, Y, and Z coordinate bytes.
    pub position: [u8; 3],
}

/// Projected scratch record used by classic alias-model batches.
#[repr(C, align(4))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasProjectedVertex {
    /// Packed screen coordinate.
    pub screen: [i16; 2],
    /// Cached unsigned GTE depth. The aligned record retains an eight-byte
    /// stride, with two trailing padding bytes left unused.
    pub depth: u16,
}

trait ClassicAffineSample {
    fn affine_uv(&self) -> [u8; 2];
    fn affine_depth(&self) -> i32;
}

impl ClassicAffineSample for ClassicAffineVertex {
    #[inline(always)]
    fn affine_uv(&self) -> [u8; 2] {
        self.uv
    }

    #[inline(always)]
    fn affine_depth(&self) -> i32 {
        self.depth
    }
}

/// Select the existing zero-, one-, or two-level lattice from depth and a
/// calibrated affine texture-error bound.
///
/// For an edge spanning `du` texels between positive depths `za` and `zb`,
/// its exact midpoint error is `du * |zb-za| / (2 * (za+zb))`. The measured
/// p90 worst-polygon multiplier is 2.4, so the bound exceeds `target` exactly
/// when `6 * du * |zb-za| > 5 * target * (za+zb)`. This comparison avoids a
/// guest division and stays within `u32` for PS1 UV and GTE depth ranges.
#[inline(always)]
fn classic_affine_subdivision_level<T: ClassicAffineSample>(
    vertices: [&T; 3],
    otz: u16,
    profile: ClassicAffineProfile,
) -> u8 {
    let mut level = if otz < profile.subdivide_twice_at {
        2
    } else if otz < profile.subdivide_once_at {
        1
    } else {
        0
    };
    if level == 2 || profile.subdivide_once_error_texels == 0 {
        return level;
    }

    debug_assert!(
        profile.subdivide_twice_error_texels == 0
            || profile.subdivide_twice_error_texels >= profile.subdivide_once_error_texels
    );
    for (a, b) in [(0usize, 1usize), (1, 2), (2, 0)] {
        let za = vertices[a].affine_depth();
        let zb = vertices[b].affine_depth();
        if za <= 0 || zb <= 0 {
            continue;
        }
        // Projected GTE depths are u16. Clamp caller-supplied projected
        // records as well so the public preprojected path cannot overflow the
        // fixed-width comparison even if its safety contract is violated.
        let za = (za as u32).min(u16::MAX as u32);
        let zb = (zb as u32).min(u16::MAX as u32);
        let uv_a = vertices[a].affine_uv();
        let uv_b = vertices[b].affine_uv();
        let du = u32::from(uv_a[0].abs_diff(uv_b[0]).max(uv_a[1].abs_diff(uv_b[1])));
        let scaled_error = du * za.abs_diff(zb) * 6;
        let scaled_depth = (za + zb) * 5;
        if profile.subdivide_twice_error_texels != 0
            && scaled_error > scaled_depth * u32::from(profile.subdivide_twice_error_texels)
        {
            return 2;
        }
        if scaled_error > scaled_depth * u32::from(profile.subdivide_once_error_texels) {
            level = 1;
        }
    }
    level
}

#[inline(always)]
fn topology_census_mix(census: &mut ClassicAffineTopologyCensus, value: u32) {
    if census.topology_hash_a == 0 && census.topology_hash_b == 0 {
        census.topology_hash_a = WordFnv32::OFFSET_BASIS;
        census.topology_hash_b = 0x9e37_79b9;
    }
    let mut word_hash = WordFnv32::from_state(census.topology_hash_a);
    word_hash.push(value);
    census.topology_hash_a = word_hash.finish();
    census.topology_hash_b = census
        .topology_hash_b
        .rotate_left(7)
        .wrapping_add(value.wrapping_mul(0x85eb_ca6b));
}

#[inline(always)]
fn topology_census_packets(
    census: &mut ClassicAffineTopologyCensus,
    packets: u32,
    hardware_triangles: u32,
    bytes: u32,
) {
    census.theoretical_packets = census.theoretical_packets.wrapping_add(packets);
    census.theoretical_hardware_triangles = census
        .theoretical_hardware_triangles
        .wrapping_add(hardware_triangles);
    census.theoretical_packet_bytes = census.theoretical_packet_bytes.wrapping_add(bytes);
}

/// Accumulate the exact adaptive topology decisions for already projected
/// compact fans without emitting another packet stream.
///
/// This is a diagnostic counterpart to [`submit_classic_affine_batch`]. It
/// shares the submitter's subdivision selector, fan pairing, OTZ rejection,
/// whole-surface clipping, and underdraw predicates. Individual polygon screen
/// rejection is deliberately left to the real submit result, so the difference
/// between theoretical and actual bytes measures that final camera-dependent
/// stage.
///
/// # Safety
///
/// `vertices` and `surfaces` must obey the source-range contract of
/// [`submit_classic_affine_projected_batch`], and every source vertex must
/// already contain the current camera's projected screen coordinate and depth.
pub unsafe fn census_classic_affine_projected_batch_topology(
    vertices: *const ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    profile: ClassicAffineProfile,
    census: &mut ClassicAffineTopologyCensus,
) {
    if vertices.is_null() || surfaces.is_null() || vertex_count == 0 || surface_count == 0 {
        return;
    }

    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        census.surfaces = census.surfaces.wrapping_add(1);
        census.root_triangles = census
            .root_triangles
            .wrapping_add(surface.vertex_count.saturating_sub(2) as u32);
        topology_census_mix(census, 0x1000_0000 | u32::from(surface.vertex_count));

        let fan = unsafe { vertices.add(first_vertex) };
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < surface_vertices && surface_clip != 0 {
            surface_clip &= classic_clip_code(unsafe { (*fan.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip != 0 {
            census.surface_clip_rejects = census.surface_clip_rejects.wrapping_add(1);
            topology_census_mix(census, 0xf000_0000);
            surface_ptr = unsafe { surface_ptr.add(1) };
            continue;
        }

        let root = unsafe { &*fan };
        let root_depth = root.depth as u16 as u32;
        let end = unsafe { fan.add(surface_vertices) };
        let mut previous = unsafe { fan.add(1) };
        let mut current = unsafe { fan.add(2) };
        while current != end {
            let previous_ref = unsafe { &*previous };
            let current_ref = unsafe { &*current };
            let otz = scene::classic_ordering_depth3_from_sum(
                root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
            );
            if otz == 0 || otz >= profile.ot_depth {
                census.depth_rejects = census.depth_rejects.wrapping_add(1);
                topology_census_mix(census, 0xe000_0000);
                previous = current;
                current = unsafe { current.add(1) };
                continue;
            }

            let level =
                classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile);
            let next = unsafe { current.add(1) };
            if level == 0 && next != end {
                let next_ref = unsafe { &*next };
                let next_otz = scene::classic_ordering_depth3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                );
                if next_otz == otz
                    && classic_affine_subdivision_level(
                        [root, current_ref, next_ref],
                        next_otz,
                        profile,
                    ) == 0
                {
                    census.level0_root_triangles = census.level0_root_triangles.wrapping_add(2);
                    census.paired_level0_packets = census.paired_level0_packets.wrapping_add(1);
                    topology_census_packets(census, 1, 2, 52);
                    topology_census_mix(census, 0x4000_0000);
                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }

            if level == 2 {
                census.level2_root_triangles = census.level2_root_triangles.wrapping_add(1);
                topology_census_packets(census, 10, 16, 472);
                let underdraw_at = i32::from(profile.subdivide_twice_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                if underdraw {
                    census.level2_underdraw_roots = census.level2_underdraw_roots.wrapping_add(1);
                    topology_census_packets(census, 6, 9, 276);
                }
                topology_census_mix(census, 0x3000_0000 | (u32::from(underdraw) << 27));
            } else if level == 1 {
                census.level1_root_triangles = census.level1_root_triangles.wrapping_add(1);
                topology_census_packets(census, 3, 4, 132);
                let underdraw_at = i32::from(profile.subdivide_once_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                if underdraw {
                    census.level1_underdraw_roots = census.level1_underdraw_roots.wrapping_add(1);
                    topology_census_packets(census, 3, 3, 120);
                }
                topology_census_mix(census, 0x2000_0000 | (u32::from(underdraw) << 27));
            } else {
                census.level0_root_triangles = census.level0_root_triangles.wrapping_add(1);
                topology_census_packets(census, 1, 1, 40);
                topology_census_mix(census, 0);
            }
            previous = current;
            current = unsafe { current.add(1) };
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }
}

/// Collect adaptive-subdivision roots from already projected compact fans.
///
/// The returned count is the number of requests found, which may exceed
/// `output_capacity`; only the prefix that fits is written. Level-zero roots,
/// whole-surface screen rejects and invalid OT depths do not request cache
/// residency. The packet and invariant-byte shapes exactly match the compact
/// GT3/GT4 lattices used by the authoritative submitter.
///
/// # Safety
///
/// `vertices` and `surfaces` must obey the source-range contract of
/// [`submit_classic_affine_projected_batch`]. When `output_capacity` is not
/// zero, `output` must point to that many writable request records.
pub unsafe fn collect_classic_affine_projected_subdivision_requests(
    vertices: *const ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    profile: ClassicAffineProfile,
    output: *mut ClassicAffineSubdivisionRequest,
    output_capacity: usize,
) -> usize {
    if vertices.is_null()
        || surfaces.is_null()
        || vertex_count == 0
        || surface_count == 0
        || (output.is_null() && output_capacity != 0)
    {
        return 0;
    }

    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    let mut surface_index = 0usize;
    let mut request_count = 0usize;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);

        let fan = unsafe { vertices.add(first_vertex) };
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < surface_vertices && surface_clip != 0 {
            surface_clip &= classic_clip_code(unsafe { (*fan.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip == 0 {
            let root = unsafe { &*fan };
            let root_depth = root.depth as u16 as u32;
            let end = unsafe { fan.add(surface_vertices) };
            let mut previous = unsafe { fan.add(1) };
            let mut current = unsafe { fan.add(2) };
            let mut root_index = 0usize;
            while current != end {
                let previous_ref = unsafe { &*previous };
                let current_ref = unsafe { &*current };
                let otz = scene::classic_ordering_depth3_from_sum(
                    root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
                );
                if otz != 0 && otz < profile.ot_depth {
                    let level = classic_affine_subdivision_level(
                        [root, previous_ref, current_ref],
                        otz,
                        profile,
                    );
                    if level != 0 {
                        let underdraw_at = i32::from(if level == 2 {
                            profile.subdivide_twice_at
                        } else {
                            profile.subdivide_once_at
                        });
                        let underdraw = root.depth >= underdraw_at
                            || previous_ref.depth >= underdraw_at
                            || current_ref.depth >= underdraw_at;
                        let (base_bytes, underdraw_bytes, invariant_base, invariant_underdraw) =
                            if level == 2 {
                                (472u16, 276u16, 288u16, 168u16)
                            } else {
                                (132u16, 120u16, 80u16, 72u16)
                            };
                        if request_count < output_capacity {
                            unsafe {
                                ptr::write(
                                    output.add(request_count),
                                    ClassicAffineSubdivisionRequest {
                                        batch_surface: surface_index as u8,
                                        root: root_index as u8,
                                        level,
                                        underdraw: u8::from(underdraw),
                                        otz,
                                        packet_bytes: base_bytes
                                            + if underdraw { underdraw_bytes } else { 0 },
                                        invariant_bytes: invariant_base
                                            + if underdraw { invariant_underdraw } else { 0 },
                                        _padding: 0,
                                        material: u32::from(surface.tpage)
                                            | (u32::from(surface.clut) << 16),
                                    },
                                );
                            }
                        }
                        request_count += 1;
                    }
                }
                previous = current;
                current = unsafe { current.add(1) };
                root_index += 1;
            }
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
        surface_index += 1;
    }
    request_count
}

#[inline(always)]
fn light_color(light: [u8; 2], weights: [u16; 2]) -> u32 {
    let mut lit = light[0] as u32 * weights[0] as u32;
    if weights[1] != 0 {
        lit = lit.wrapping_add(light[1] as u32 * weights[1] as u32);
    }
    lit >>= 8;
    lit | (lit << 8) | (lit << 16)
}

/// Project and submit an indexed alias model through compact flat textured
/// packets.
///
/// This is the retained-model counterpart to [`submit_classic_affine_fan`]:
/// all shared vertices are projected once, then the face loop performs the
/// scheduled GTE area/depth tests and stages packets for the tagged-stream OT
/// linker.
///
/// # Safety
/// `vertices` must contain `vertex_count` records. `faces` must be four-byte
/// aligned and contain `face_count` valid faces whose projected byte offsets
/// are eight-byte aligned and below `vertex_count * 8`, `projected` must
/// contain `vertex_count` writable records, and `output` must have room for
/// one [`ClassicTriTextured`] per face.
unsafe fn submit_classic_alias_model_inner<const SCREEN_SPACE: bool>(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || faces.is_null() || projected.is_null() || output.is_null() {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        let out = project_triangle_scheduled(
            unsafe { alias_vec3(vertices, vertex) },
            unsafe { alias_vec3(vertices, vertex + 1) },
            unsafe { alias_vec3(vertices, vertex + 2) },
        );
        let projected_a = ClassicAliasProjectedVertex {
            screen: [out[0].sx, out[0].sy],
            depth: out[0].sz,
        };
        let projected_b = ClassicAliasProjectedVertex {
            screen: [out[1].sx, out[1].sy],
            depth: out[1].sz,
        };
        let projected_c = ClassicAliasProjectedVertex {
            screen: [out[2].sx, out[2].sy],
            depth: out[2].sz,
        };
        unsafe {
            ptr::write(projected.add(vertex), projected_a);
            ptr::write(projected.add(vertex + 1), projected_b);
            ptr::write(projected.add(vertex + 2), projected_c);
        }
        vertex += 3;
    }
    while vertex < vertex_count {
        let out = project_vertex_scheduled(unsafe { alias_vec3(vertices, vertex) });
        let projected_vertex = ClassicAliasProjectedVertex {
            screen: [out.sx, out.sy],
            depth: out.sz,
        };
        unsafe { ptr::write(projected.add(vertex), projected_vertex) };
        vertex += 1;
    }

    let mut next = output;
    let mut face_index = 0usize;
    while face_index < face_count {
        let corners = unsafe { ptr::read(faces.add(face_index)) }.corners;
        let projected_bytes = projected.cast::<u8>();
        let a = unsafe {
            &*projected_bytes
                .add((corners[0] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let b = unsafe {
            &*projected_bytes
                .add((corners[1] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let c = unsafe {
            &*projected_bytes
                .add((corners[2] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let screens = [a.screen, b.screen, c.screen];
        let screen_points = [
            (screens[0][0], screens[0][1]),
            (screens[1][0], screens[1][1]),
            (screens[2][0], screens[2][1]),
        ];
        let (area, cached_otz) = if SCREEN_SPACE {
            (scene::screen_area(screen_points), u16::MAX)
        } else {
            scene::screen_area_and_classic_ordering_depth3_scheduled(
                screen_points,
                [a.depth, b.depth, c.depth],
            )
        };
        if area >= 0 {
            let otz = if SCREEN_SPACE {
                Some(cached_otz)
            } else {
                let depth = cached_otz;
                (depth > 0 && depth < profile.ot_depth).then_some(depth)
            };
            if let Some(otz) = otz {
                let packet = ClassicTriTextured::with_staged_slot(
                    [
                        (screens[0][0], screens[0][1]),
                        (screens[1][0], screens[1][1]),
                        (screens[2][0], screens[2][1]),
                    ],
                    [corners[0] as u16, corners[1] as u16, corners[2] as u16],
                    tint,
                    clut,
                    tpage,
                    otz,
                );
                unsafe { ptr::write(next.cast::<ClassicTriTextured>(), packet) };
                next = unsafe { next.add(size_of::<ClassicTriTextured>() / size_of::<u32>()) };
            }
        }
        face_index += 1;
    }

    let packets = unsafe { next.offset_from(output) as u32 }
        / (size_of::<ClassicTriTextured>() / size_of::<u32>()) as u32;
    ClassicAffineSubmit {
        next_packet: next,
        packets,
        hardware_triangles: packets,
    }
}

#[inline(always)]
unsafe fn alias_vec3(vertices: *const ClassicAliasVertex, index: usize) -> Vec3I16 {
    let vertex = unsafe { (*vertices.add(index)).position };
    Vec3I16::new(vertex[0] as i16, vertex[1] as i16, vertex[2] as i16)
}

/// Project and submit an indexed alias model into staged ordering-table
/// packets.
///
/// # Safety
/// The pointer and capacity contract is the same as
/// [`submit_classic_affine_fan`]; every projected face offset must be aligned
/// to eight bytes and below `vertex_count * 8`.
pub unsafe fn submit_classic_alias_model(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_alias_model_inner::<false>(
            vertices,
            vertex_count,
            faces,
            face_count,
            projected,
            output,
            tpage,
            clut,
            tint,
            profile,
        )
    }
}

/// Project and submit an indexed first-person alias model into a
/// contiguous screen-space packet stream.
///
/// The returned packets carry the `0xffff` tagged-stream sentinel and must be
/// registered with the caller's screen/HUD submission list.
///
/// # Safety
/// The pointer and capacity contract is the same as
/// [`submit_classic_alias_model`].
pub unsafe fn submit_classic_alias_view_model(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_alias_model_inner::<true>(
            vertices,
            vertex_count,
            faces,
            face_count,
            projected,
            output,
            tpage,
            clut,
            tint,
            profile,
        )
    }
}

#[inline(always)]
fn classic_clip_code(screen: [i16; 2], profile: ClassicAffineProfile) -> u8 {
    zero_origin_screen_outcode(
        screen,
        profile.screen_width as i32 - 1,
        profile.screen_height as i32 - 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_gte::math::{Mat3I16, Vec3I32};

    fn census_vertices(count: usize) -> [ClassicAffineVertex; 4] {
        let mut vertices = [ClassicAffineVertex::default(); 4];
        for (index, vertex) in vertices[..count].iter_mut().enumerate() {
            vertex.screen = [40 + index as i16 * 8, 60 + index as i16 * 6];
            vertex.depth = 100;
            vertex.uv = [index as u8 * 8, index as u8 * 4];
            vertex.color = 0x0080_8080;
        }
        vertices
    }

    #[test]
    fn topology_census_recovers_level_zero_fan_pairing() {
        let vertices = census_vertices(4);
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 4,
            tpage: 0,
            clut: 0,
        };
        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut census = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                &mut census,
            );
        }
        assert_eq!(census.root_triangles, 2);
        assert_eq!(census.level0_root_triangles, 2);
        assert_eq!(census.paired_level0_packets, 1);
        assert_eq!(census.theoretical_packets, 1);
        assert_eq!(census.theoretical_hardware_triangles, 2);
        assert_eq!(census.theoretical_packet_bytes, 52);
    }

    #[test]
    fn topology_census_bounds_both_subdivision_lattices() {
        let vertices = census_vertices(3);
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 3,
            tpage: 0,
            clut: 0,
        };
        let mut once = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                3,
                &surface,
                1,
                ClassicAffineProfile {
                    subdivide_once_at: 1000,
                    subdivide_twice_at: 0,
                    ..ClassicAffineProfile::QUAKE_REFERENCE
                },
                &mut once,
            );
        }
        assert_eq!(once.level1_root_triangles, 1);
        assert_eq!(once.level1_underdraw_roots, 0);
        assert_eq!(once.theoretical_packets, 3);
        assert_eq!(once.theoretical_hardware_triangles, 4);
        assert_eq!(once.theoretical_packet_bytes, 132);

        let mut twice = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                3,
                &surface,
                1,
                ClassicAffineProfile {
                    subdivide_once_at: 2000,
                    subdivide_twice_at: 1000,
                    ..ClassicAffineProfile::QUAKE_REFERENCE
                },
                &mut twice,
            );
        }
        assert_eq!(twice.level2_root_triangles, 1);
        assert_eq!(twice.level2_underdraw_roots, 0);
        assert_eq!(twice.theoretical_packets, 10);
        assert_eq!(twice.theoretical_hardware_triangles, 16);
        assert_eq!(twice.theoretical_packet_bytes, 472);
    }

    #[test]
    fn subdivision_request_collector_reports_exact_cache_shapes() {
        let mut vertices = census_vertices(4);
        vertices[0].depth = 100;
        vertices[1].depth = 100;
        vertices[2].depth = 100;
        vertices[3].depth = 1200;
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 4,
            tpage: 0x1234,
            clut: 0xabcd,
        };
        let profile = ClassicAffineProfile {
            subdivide_once_at: 1000,
            subdivide_twice_at: 500,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut requests = [ClassicAffineSubdivisionRequest::default(); 2];
        let count = unsafe {
            collect_classic_affine_projected_subdivision_requests(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                requests.as_mut_ptr(),
                requests.len(),
            )
        };
        assert_eq!(count, 2);
        assert_eq!(requests[0].batch_surface, 0);
        assert_eq!(requests[0].root, 0);
        assert_eq!(requests[0].level, 2);
        assert_eq!(requests[0].underdraw, 0);
        assert_eq!(requests[0].packet_bytes, 472);
        assert_eq!(requests[0].invariant_bytes, 288);
        assert_eq!(requests[0].material, 0xabcd_1234);
        assert_eq!(requests[1].root, 1);
        assert_eq!(requests[1].level, 2);
        assert_eq!(requests[1].underdraw, 1);
        assert_eq!(requests[1].packet_bytes, 748);
        assert_eq!(requests[1].invariant_bytes, 456);

        let required = unsafe {
            collect_classic_affine_projected_subdivision_requests(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                requests.as_mut_ptr(),
                1,
            )
        };
        assert_eq!(required, 2);
    }

    fn affine_sample(uv: [u8; 2], depth: i32) -> ClassicAffineVertex {
        ClassicAffineVertex {
            uv,
            depth,
            ..ClassicAffineVertex::default()
        }
    }

    fn affine_refs(vertices: &[ClassicAffineVertex; 3]) -> [&ClassicAffineVertex; 3] {
        [&vertices[0], &vertices[1], &vertices[2]]
    }

    #[test]
    fn adaptive_profile_splits_far_oblique_texture_edges_by_predicted_error() {
        let moderate = [
            affine_sample([0, 0], 900),
            affine_sample([63, 0], 1100),
            affine_sample([0, 0], 900),
        ];
        let severe = [
            affine_sample([0, 0], 900),
            affine_sample([80, 0], 1100),
            affine_sample([0, 0], 900),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&moderate),
                500,
                ClassicAffineProfile::QUAKE_REFERENCE,
            ),
            0,
            "the historical profile must remain byte-compatible"
        );
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&moderate),
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            1,
            "7.56 predicted texels fit after one bounded bisection"
        );
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&severe),
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2,
            "9.6 predicted texels require the existing second lattice level"
        );
    }

    #[test]
    fn adaptive_error_rule_ignores_constant_or_invalid_depth_edges() {
        let constant = [
            affine_sample([0, 0], 1000),
            affine_sample([255, 255], 1000),
            affine_sample([0, 255], 1000),
        ];
        let behind = [
            affine_sample([0, 0], 0),
            affine_sample([255, 255], 1000),
            affine_sample([0, 255], -1),
        ];
        for vertices in [&constant, &behind] {
            assert_eq!(
                classic_affine_subdivision_level(
                    [&vertices[0], &vertices[1], &vertices[2]],
                    500,
                    ClassicAffineProfile::RUNTIME_ADAPTIVE,
                ),
                0
            );
        }
    }

    #[test]
    fn adaptive_error_rule_never_exceeds_the_existing_two_level_packet_bound() {
        let extreme = [
            affine_sample([0, 0], 1),
            affine_sample([255, 255], u16::MAX as i32),
            affine_sample([0, 255], 1),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                [&extreme[0], &extreme[1], &extreme[2]],
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2
        );

        let flat_near = [
            affine_sample([0, 0], 1000),
            affine_sample([0, 0], 1000),
            affine_sample([0, 0], 1000),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                [&flat_near[0], &flat_near[1], &flat_near[2]],
                50,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2,
            "the original close-surface depth schedule remains authoritative"
        );
    }

    #[test]
    fn alias_vertex_reader_preserves_compact_coordinates() {
        let compact = [
            ClassicAliasVertex {
                position: [1, 2, 3],
            },
            ClassicAliasVertex {
                position: [4, 5, 6],
            },
        ];
        assert_eq!(
            unsafe { alias_vec3(compact.as_ptr(), 1) },
            Vec3I16::new(4, 5, 6)
        );
    }

    #[test]
    fn fused_alias_transform_matches_separate_sdk_operations() {
        let view_rotation = Mat3I16::rotate_xyz(19, 37, 5);
        let view_translation = Vec3I32::new(120, -48, 320);
        let model_rotation = Mat3I16::rotate_z(71).mul(&Mat3I16::rotate_y(43));
        let model_offset = Vec3I16::new(-7, 13, 4);
        let world_origin = Vec3I32::new(96, -112, 28);
        let scale = Vec3I16::new(3072, 4096, 5120);

        scene::load_rotation(&model_rotation);
        scene::load_translation(Vec3I32::ZERO);
        let rotated_offset = scene::transform_vertex_scheduled(model_offset);
        let diagonal = Mat3I16 {
            m: [[scale.x, 0, 0], [0, scale.y, 0], [0, 0, scale.z]],
        };
        let scaled = scene::compose_rotation_scheduled(&model_rotation, &diagonal);
        let rotation = scene::compose_rotation_scheduled(&view_rotation, &scaled);
        scene::load_rotation(&view_rotation);
        scene::load_translation(Vec3I32::ZERO);
        let rotated_translation = scene::transform_vertex_scheduled(Vec3I16::new(
            rotated_offset.x.wrapping_add(world_origin.x) as i16,
            rotated_offset.y.wrapping_add(world_origin.y) as i16,
            rotated_offset.z.wrapping_add(world_origin.z) as i16,
        ));
        let translation = Vec3I32::new(
            rotated_translation.x.wrapping_add(view_translation.x),
            rotated_translation.y.wrapping_add(view_translation.y),
            rotated_translation.z.wrapping_add(view_translation.z),
        );

        assert_eq!(
            psx_engine::compose_model_view_transform(
                view_rotation,
                view_translation,
                model_rotation,
                model_offset,
                world_origin,
                scale,
            ),
            (rotation, translation)
        );
    }

    #[test]
    fn branchless_clip_code_keeps_inclusive_viewport_boundaries() {
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        assert_eq!(classic_clip_code([0, 0], profile), 0);
        assert_eq!(classic_clip_code([318, 238], profile), 0);
        assert_eq!(classic_clip_code([319, 239], profile), 0);
        assert_eq!(classic_clip_code([-1, 120], profile), 1);
        assert_eq!(classic_clip_code([320, 120], profile), 2);
        assert_eq!(classic_clip_code([160, -1], profile), 4);
        assert_eq!(classic_clip_code([160, 240], profile), 8);
        assert_eq!(classic_clip_code([-1024, -1024], profile), 5);
        assert_eq!(classic_clip_code([1023, 1023], profile), 10);
    }
}
