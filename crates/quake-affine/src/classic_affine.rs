//! Compact classic-affine world submission for retained C/Rust renderers.
//!
//! This path keeps the historical PS1 packet topology: camera-space
//! midpoints are reprojected, near or visibly warped triangles use a fixed
//! two-level lattice, compatible leaves are paired into GP0(3Ch) quads, and
//! optional root-edge underdraw seals rasterisation cracks. It deliberately
//! emits the compact no-texture-window packets from `psx-gpu`; callers set one
//! texture-window state for the pass and finish the staged tags with PSoXide's
//! tagged-stream OT linker.

use core::{mem::size_of, ptr};

use psx_pack::WordFnv32;
use psx_hw::gpu::packet;
use psx_gpu::prim::{
    ClassicQuadTexturedGouraud, ClassicTriTextured, ClassicTriTexturedGouraud,
};
use psx_gte::{
    math::Vec3I16,
    scene::{self, project_triangle_scheduled, project_vertex_scheduled, Projected},
};

use crate::screen::{
    classic_quad_screen_rejected, classic_triangle_screen_rejected, zero_origin_screen_outcode,
};

const EXTRA_VERTICES: usize = 12;

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

/// One fan inside a contiguous classic-affine vertex batch.
///
/// Quake's kernel reads a descriptor as two words (the descriptors live in
/// RAM, where every load stalls), so its feature aligns them to four bytes.
#[repr(C)]
#[cfg_attr(feature = "classic-affine-quake-specialized-kernel", repr(align(4)))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineBatchSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word used by this surface.
    pub tpage: u16,
    /// CLUT word used by this surface.
    pub clut: u16,
}

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

struct PacketWriter {
    next: *mut u32,
    packets: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    profile: ClassicAffineProfile,
}

impl PacketWriter {
    #[inline(always)]
    unsafe fn emit_tri_unclipped(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = ClassicTriTexturedGouraud::with_staged_slot_prepacked_colors(
            [
                (projected[0].screen[0], projected[0].screen[1]),
                (projected[1].screen[0], projected[1].screen[1]),
                (projected[2].screen[0], projected[2].screen[1]),
            ],
            [
                uv_word(attributes[0]),
                uv_word(attributes[1]),
                uv_word(attributes[2]),
            ],
            [
                attributes[0].color,
                attributes[1].color,
                attributes[2].color,
            ],
            self.clut_high_word,
            self.tpage_high_word,
            otz,
        );
        unsafe { ptr::write(self.next.cast::<ClassicTriTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        if classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        ) {
            return;
        }
        unsafe { self.emit_tri_unclipped(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad_unclipped(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = ClassicQuadTexturedGouraud::with_staged_slot_prepacked_colors(
            [
                (projected[0].screen[0], projected[0].screen[1]),
                (projected[1].screen[0], projected[1].screen[1]),
                (projected[2].screen[0], projected[2].screen[1]),
                (projected[3].screen[0], projected[3].screen[1]),
            ],
            [
                uv_word(attributes[0]),
                uv_word(attributes[1]),
                uv_word(attributes[2]),
                uv_word(attributes[3]),
            ],
            [
                attributes[0].color,
                attributes[1].color,
                attributes[2].color,
                attributes[3].color,
            ],
            self.clut_high_word,
            self.tpage_high_word,
            otz,
        );
        unsafe { ptr::write(self.next.cast::<ClassicQuadTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        if classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        ) {
            return;
        }
        unsafe { self.emit_quad_unclipped(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn finish(self, output: *mut u32) -> ClassicAffineSubmit {
        let words = unsafe { self.next.offset_from(output) as u32 };
        let tri_words = (size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>()) as u32;
        let quad_words = (size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>()) as u32;
        // Every packet is either one hardware triangle or one quad. Recover
        // the quad count from the final stream length so emitters only update
        // the packet count in the hot path.
        let quads =
            words.wrapping_sub(self.packets.wrapping_mul(tri_words)) / (quad_words - tri_words);
        ClassicAffineSubmit {
            next_packet: self.next,
            packets: self.packets,
            hardware_triangles: self.packets.wrapping_add(quads),
        }
    }
}

/// Packet sink shared by the compact and self-contained-window variants.
/// Generic subdivision code monomorphises this trait, so the normal compact
/// path keeps its existing packet shape and does not gain a per-polygon
/// material branch.
trait AffinePacketWriter {
    /// True when this writer deliberately delegates per-polygon lattice
    /// rejection to the GPU draw area after the whole surface was admitted.
    const LATTICE_USES_GPU_CLIP: bool = false;

    /// True when the caller's conservative 3D admission permits this writer
    /// to delegate the projected whole-surface reject to the GPU draw area.
    const SURFACE_USES_GPU_CLIP: bool = false;

    /// False when this writer's fans always subdivide by depth band, so the
    /// error-bounded and quad-lattice paths (feature
    /// `classic-affine-lattice`) need not be compiled into it. Its profiles
    /// must then leave `subdivide_error_px_q3` at 0 and `quad_lattice` off.
    #[cfg_attr(not(feature = "classic-affine-lattice"), allow(dead_code))]
    const USES_LATTICE: bool = true;

    fn profile(&self) -> ClassicAffineProfile;

    /// Where the next packet will be written, for the writers that keep their
    /// packets in memory; null for the others.
    #[inline(always)]
    fn packet_cursor(&self) -> *mut u32 {
        ptr::null_mut()
    }

    /// Repair the packets written since `start` for a face whose screen box
    /// is over the GPU's extent limit: see [`split_oversize_packets`]. Only
    /// the compact writer has packets to repair.
    #[cfg_attr(not(feature = "classic-affine-lattice"), allow(dead_code))]
    #[inline(always)]
    unsafe fn repair_oversize_packets(&mut self, _start: *mut u32, _budget_words: usize) {}

    #[inline(always)]
    fn topology_event(&mut self, _value: u8) {}

    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    );

    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    );

    #[inline(always)]
    unsafe fn emit_visible_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { self.emit_tri(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_visible_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { self.emit_quad(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_subdivide_twice(
        &mut self,
        root0: &ClassicAffineVertex,
        root1: &ClassicAffineVertex,
        root2: &ClassicAffineVertex,
        scratch: *mut ClassicAffineVertex,
        root_otz: u16,
        underdraw_edges: u8,
    ) where
        Self: Sized,
    {
        unsafe {
            subdivide_twice(
                self,
                root0,
                root1,
                root2,
                scratch,
                root_otz,
                underdraw_edges,
            )
        };
    }
}

impl AffinePacketWriter for PacketWriter {
    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    const LATTICE_USES_GPU_CLIP: bool = true;

    #[inline(always)]
    fn packet_cursor(&self) -> *mut u32 {
        self.next
    }

    #[cfg_attr(not(feature = "classic-affine-lattice"), allow(dead_code))]
    #[inline(always)]
    unsafe fn repair_oversize_packets(&mut self, start: *mut u32, budget_words: usize) {
        unsafe { split_oversize_packets(self, start, budget_words) };
    }

    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        unsafe {
            self.emit_tri_unclipped(projected, attributes, otz)
        };
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        unsafe {
            PacketWriter::emit_tri(self, projected, attributes, otz)
        };
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        unsafe {
            self.emit_quad_unclipped(projected, attributes, otz)
        };
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        unsafe {
            PacketWriter::emit_quad(self, projected, attributes, otz)
        };
    }

    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    #[inline(always)]
    unsafe fn emit_visible_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { self.emit_tri_unclipped(projected, attributes, otz) };
    }

    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    #[inline(always)]
    unsafe fn emit_visible_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { self.emit_quad_unclipped(projected, attributes, otz) };
    }
}

#[inline(always)]
const fn uv_word(vertex: &ClassicAffineVertex) -> u16 {
    vertex.uv[0] as u16 | ((vertex.uv[1] as u16) << 8)
}

#[inline(always)]
fn midpoint(a: &ClassicAffineVertex, b: &ClassicAffineVertex) -> ClassicAffineVertex {
    // Average every colour channel. The colour word carries baked RGB light
    // on PXBSP faces; averaging one byte and replicating it turned every
    // generated near-band vertex grey, so the floor and rails darkened in a
    // band that followed the camera.
    let color = ((a.color & 0x00ff_00ff) + (b.color & 0x00ff_00ff)) >> 1 & 0x00ff_00ff
        | ((a.color & 0x0000_ff00) + (b.color & 0x0000_ff00)) >> 1 & 0x0000_ff00;
    let a_uv = u16::from_le_bytes(a.uv);
    let b_uv = u16::from_le_bytes(b.uv);
    // Average both packed bytes independently. Clearing each byte's low bit
    // before the shift prevents a carry from U into V.
    let uv = (a_uv & b_uv).wrapping_add(((a_uv ^ b_uv) & 0xfefe) >> 1);
    ClassicAffineVertex {
        position: [
            ((a.position[0] as i32 + b.position[0] as i32) >> 1) as i16,
            ((a.position[1] as i32 + b.position[1] as i32) >> 1) as i16,
            ((a.position[2] as i32 + b.position[2] as i32) >> 1) as i16,
        ],
        uv: uv.to_le_bytes(),
        color,
        screen: [0; 2],
        depth: 0,
    }
}

#[inline(always)]
unsafe fn project_three_consecutive(vertices: *mut ClassicAffineVertex) {
    let a = unsafe { classic_vertex_position(vertices) };
    let b = unsafe { classic_vertex_position(vertices.add(1)) };
    let c = unsafe { classic_vertex_position(vertices.add(2)) };
    let out = project_triangle_scheduled(a, b, c);
    unsafe {
        store_classic_projection(vertices, out[0]);
        store_classic_projection(vertices.add(1), out[1]);
        store_classic_projection(vertices.add(2), out[2]);
    }
}

#[inline(always)]
unsafe fn project_one(vertex: *mut ClassicAffineVertex) {
    let source = unsafe { classic_vertex_position(vertex) };
    let out = project_vertex_scheduled(source);
    unsafe { store_classic_projection(vertex, out) };
}

#[inline(always)]
unsafe fn classic_vertex_position(vertex: *const ClassicAffineVertex) -> Vec3I16 {
    let position = unsafe { (*vertex).position };
    Vec3I16::new(position[0], position[1], position[2])
}

#[inline(always)]
unsafe fn store_classic_projection(vertex: *mut ClassicAffineVertex, out: Projected) {
    unsafe {
        ptr::write(ptr::addr_of_mut!((*vertex).screen), [out.sx, out.sy]);
        ptr::write(ptr::addr_of_mut!((*vertex).depth), out.sz as i32);
    }
}

/// OT key of crack-sealing underdraw drawn behind its own split pieces.
///
/// One sort rule for every classic-affine world surface (and GoldSrc's
/// world): each packet a face emits, split piece or whole face, keys at its
/// own vertex average, and only underlays that exist to paper pinholes
/// behind a face's children key at the face's farthest vertex, never nearer
/// than the historical average-plus-bias slot, so no child is covered.
#[inline(always)]
fn underlay_otz(average_otz: u16, far_depth: u16, profile: ClassicAffineProfile) -> u16 {
    average_otz
        .saturating_add(profile.underdraw_slot_bias)
        .max(far_depth >> 2)
        .min(profile.ot_depth - 1)
}

#[inline(always)]
fn average3(vertices: [&ClassicAffineVertex; 3]) -> u16 {
    average3_depths(
        vertices[0].depth as u16,
        vertices[1].depth as u16,
        vertices[2].depth as u16,
    )
}

/// Convert three cached GTE depths to the classic OT key.
///
/// The host form intentionally retains the arithmetic oracle so all existing
/// packet-parity tests stay independent of global emulated GTE state. On PS1,
/// the feature A/B reloads SZ1..SZ3 and executes AVSZ3 with the installed
/// ZSF3=0x155; this is mathematically identical to the software path.
#[inline(always)]
fn average3_depths(a: u16, b: u16, c: u16) -> u16 {
    {
        scene::classic_ordering_depth3_from_sum(u32::from(a) + u32::from(b) + u32::from(c))
    }
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

/// Words the caller reserves in the packet arena for each triangle of a face
/// before it submits the face (nineteen of the largest packets): the most a
/// face may write, and so the room [`split_oversize_packets`] works inside.
pub const WORST_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 13;

/// One vertex of a packet being cut: screen position, packet UV and RGB.
#[derive(Copy, Clone)]
struct CutVertex {
    xy: [i32; 2],
    uv: [i32; 2],
    rgb: [i32; 3],
}

impl CutVertex {
    /// Read vertex `index` of the packet at `at` (words `[rgb, xy, uv]` from
    /// word 1, three words each).
    #[inline(always)]
    unsafe fn read(at: *const u32, index: usize) -> Self {
        let [color, xy, uv] = unsafe {
            [
                at.add(1 + 3 * index),
                at.add(2 + 3 * index),
                at.add(3 + 3 * index),
            ]
        }
        .map(|word| unsafe { ptr::read(word) });
        Self {
            xy: [i32::from(xy as i16), i32::from((xy >> 16) as i16)],
            uv: [(uv & 0xff) as i32, ((uv >> 8) & 0xff) as i32],
            rgb: [
                (color & 0xff) as i32,
                ((color >> 8) & 0xff) as i32,
                ((color >> 16) & 0xff) as i32,
            ],
        }
    }

    fn middle(a: &Self, b: &Self) -> Self {
        Self {
            xy: [(a.xy[0] + b.xy[0]) >> 1, (a.xy[1] + b.xy[1]) >> 1],
            uv: [(a.uv[0] + b.uv[0]) >> 1, (a.uv[1] + b.uv[1]) >> 1],
            rgb: [
                (a.rgb[0] + b.rgb[0]) >> 1,
                (a.rgb[1] + b.rgb[1]) >> 1,
                (a.rgb[2] + b.rgb[2]) >> 1,
            ],
        }
    }
}

/// Whether the GPU would drop a triangle for its size: two vertices more
/// than 1023 pixels apart across or 511 down.
fn cut_exceeds_gpu_extent(t: &[CutVertex; 3]) -> bool {
    let span = |axis: usize| {
        let v = [t[0].xy[axis], t[1].xy[axis], t[2].xy[axis]];
        v[0].max(v[1]).max(v[2]) - v[0].min(v[1]).min(v[2])
    };
    span(0) > 1023 || span(1) > 511
}

/// Where cut triangles are appended: the writer's cursor and the end of the
/// room the face may use.
struct CutOutput {
    next: *mut u32,
    limit: *mut u32,
    packets: u32,
    otz: u32,
    clut_word: u32,
    tpage_word: u32,
}

/// Write `triangle` as GT3 packets that fit the GPU's extent: whole when it
/// does, else its four midpoint triangles, `levels` deep at most. A piece
/// that does not fit the room left is dropped, as the whole was.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn cut_triangle(out: &mut CutOutput, t: [CutVertex; 3], levels: u8) {
    if levels != 0 && cut_exceeds_gpu_extent(&t) {
        let m = [
            CutVertex::middle(&t[0], &t[1]),
            CutVertex::middle(&t[1], &t[2]),
            CutVertex::middle(&t[2], &t[0]),
        ];
        let next = levels - 1;
        for piece in [
            [t[0], m[0], m[2]],
            [m[0], t[1], m[1]],
            [m[2], m[1], t[2]],
            [m[0], m[1], m[2]],
        ] {
            unsafe { cut_triangle(out, piece, next) };
        }
        return;
    }
    let words = 1 + ClassicTriTexturedGouraud::WORDS as usize;
    if unsafe { out.limit.offset_from(out.next) } < words as isize {
        return;
    }
    let pack = |v: &CutVertex| (v.xy[0] as u16 as u32) | ((v.xy[1] as u16 as u32) << 16);
    let rgb =
        |v: &CutVertex| (v.rgb[0] as u32) | ((v.rgb[1] as u32) << 8) | ((v.rgb[2] as u32) << 16);
    let uv = |v: &CutVertex| (v.uv[0] as u32) | ((v.uv[1] as u32) << 8);
    let packet = [
        ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | out.otz,
        packet::SHADED_TEXTURED_TRIANGLE | rgb(&t[0]),
        pack(&t[0]),
        uv(&t[0]) | out.clut_word,
        rgb(&t[1]),
        pack(&t[1]),
        uv(&t[1]) | out.tpage_word,
        rgb(&t[2]),
        pack(&t[2]),
        uv(&t[2]),
    ];
    for (index, word) in packet.into_iter().enumerate() {
        unsafe { ptr::write(out.next.add(index), word) };
    }
    out.next = unsafe { out.next.add(words) };
    out.packets += 1;
}

/// The GPU drops a primitive over 1023 pixels across or 511 down. Near the
/// eye the cells of a split face can still be that big, and then the sky or
/// the wall behind shows through where they should be. For a face whose
/// screen box is over the limit, walk the packets it wrote from `start`: one
/// that is over the limit is taken out of the ordering table (its slot becomes
/// the skip value) and replaced by smaller triangles cut at the edge midpoints
/// of its screen vertices, which the GPU's affine mapping draws as it would
/// have drawn the whole, at the packet's own ordering slot. The pieces are
/// appended inside the room the caller reserved for the face, as many as fit.
/// Rare, small and out of line: the split paths themselves are untouched.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn split_oversize_packets(writer: &mut PacketWriter, start: *mut u32, budget_words: usize) {
    let end = writer.next;
    let mut out = CutOutput {
        next: end,
        limit: unsafe { start.add(budget_words) },
        packets: 0,
        otz: 0,
        clut_word: 0,
        tpage_word: 0,
    };
    let mut at = start;
    while at < end {
        let tag = unsafe { ptr::read(at) };
        let body = (tag >> 24) as usize;
        // The box of the packet's vertices: if it fits, every triangle in it
        // does, so only a packet over the limit goes to `cut_packet`.
        let corners = if body == ClassicQuadTexturedGouraud::WORDS as usize {
            4
        } else {
            3
        };
        let (mut x0, mut x1, mut y0, mut y1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        let mut corner = 0;
        while corner < corners {
            let xy = unsafe { ptr::read(at.add(2 + 3 * corner)) };
            let (x, y) = (i32::from(xy as i16), i32::from((xy >> 16) as i16));
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            corner += 1;
        }
        if (x1 - x0 > 1023 || y1 - y0 > 511) && tag & 0xffff != 0xffff {
            unsafe { cut_packet(&mut out, at, tag, corners == 4) };
        }
        at = unsafe { at.add(1 + body) };
    }
    writer.next = out.next;
    writer.packets += out.packets;
}

/// [`split_oversize_packets`] for one packet whose box is over the limit: if
/// one of the triangles the GPU draws from it is over the limit it leaves the
/// ordering table and its triangles are cut instead.
#[cold]
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn cut_packet(out: &mut CutOutput, at: *mut u32, tag: u32, quad: bool) {
    let v = unsafe {
        [
            CutVertex::read(at, 0),
            CutVertex::read(at, 1),
            CutVertex::read(at, 2),
            CutVertex::read(at, if quad { 3 } else { 2 }),
        ]
    };
    let halves = [[v[0], v[1], v[2]], [v[1], v[2], v[3]]];
    if cut_exceeds_gpu_extent(&halves[0]) || (quad && cut_exceeds_gpu_extent(&halves[1])) {
        out.otz = tag & 0xffff;
        out.clut_word = unsafe { ptr::read(at.add(3)) } & 0xffff_0000;
        out.tpage_word = unsafe { ptr::read(at.add(6)) } & 0xffff_0000;
        unsafe {
            ptr::write(at, (tag & 0xffff_0000) | 0xffff);
            cut_triangle(out, halves[0], 3);
            if quad {
                cut_triangle(out, halves[1], 3);
            }
        }
    }
}

#[inline(always)]
unsafe fn sorted_tri<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
    lattice_visible: bool,
) {
    let otz = average3(projected);
    if otz > 0 {
        if lattice_visible {
            unsafe { writer.emit_visible_tri(projected, attributes, otz) };
        } else {
            unsafe { writer.emit_tri(projected, attributes, otz) };
        }
    }
}

#[inline(always)]
unsafe fn sorted_quad<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
    lattice_visible: bool,
) {
    let depth_sum = projected[0].depth as u16 as u32
        + projected[1].depth as u16 as u32
        + projected[2].depth as u16 as u32
        + projected[3].depth as u16 as u32;
    let otz = (depth_sum >> 4) as u16;
    if otz > 0 {
        if lattice_visible {
            unsafe { writer.emit_visible_quad(projected, attributes, otz) };
        } else {
            unsafe { writer.emit_quad(projected, attributes, otz) };
        }
    }
}

#[inline(always)]
unsafe fn projected_lattice_inside<W: AffinePacketWriter>(
    _writer: &W,
    _roots: [&ClassicAffineVertex; 3],
    _generated: *const ClassicAffineVertex,
    _generated_count: usize,
) -> bool {
    W::LATTICE_USES_GPU_CLIP
}

#[inline(always)]
unsafe fn emit_classified_tri<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
    otz: u16,
    lattice_visible: bool,
) {
    if lattice_visible {
        unsafe { writer.emit_visible_tri(projected, attributes, otz) };
    } else {
        unsafe { writer.emit_tri(projected, attributes, otz) };
    }
}

#[inline(always)]
unsafe fn emit_classified_quad<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
    otz: u16,
    lattice_visible: bool,
) {
    if lattice_visible {
        unsafe { writer.emit_visible_quad(projected, attributes, otz) };
    } else {
        unsafe { writer.emit_quad(projected, attributes, otz) };
    }
}

// Quake's split-fan function calls this once per root: inlined there, a
// face pays one register save rather than one per root.
#[cfg_attr(feature = "classic-affine-quake-specialized-kernel", inline(always))]
unsafe fn subdivide_once<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    underdraw_edges: u8,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root2, root0));
        project_three_consecutive(scratch);
    }
    let h01 = unsafe { &*scratch };
    let h12 = unsafe { &*scratch.add(1) };
    let h20 = unsafe { &*scratch.add(2) };
    let lattice_visible =
        unsafe { projected_lattice_inside(writer, [root0, root1, root2], scratch, 3) };
    unsafe {
        sorted_quad(
            writer,
            [root0, h01, h20, h12],
            [root0, h01, h20, h12],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [h01, root1, h12],
            [h01, root1, h12],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [h12, root2, h20],
            [h12, root2, h20],
            lattice_visible,
        );
    }
    let profile = writer.profile();
    let underdraw_at = i32::from(profile.subdivide_once_at);
    if root0.depth >= underdraw_at || root1.depth >= underdraw_at || root2.depth >= underdraw_at {
        let far = (root0.depth as u16)
            .max(root1.depth as u16)
            .max(root2.depth as u16);
        let underdraw = underlay_otz(root_otz, far, profile);
        unsafe {
            if underdraw_edges & 1 != 0 {
                emit_classified_tri(
                    writer,
                    [root0, root1, h01],
                    [root0, root1, h01],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 2 != 0 {
                emit_classified_tri(
                    writer,
                    [root1, root2, h12],
                    [root0, root1, h12],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 4 != 0 {
                emit_classified_tri(
                    writer,
                    [root2, root0, h20],
                    [root2, root0, h20],
                    underdraw,
                    lattice_visible,
                );
            }
        }
    }
}

// Quake's split-fan function calls this once per root: inlined there, a
// face pays one register save rather than one per root.
#[cfg_attr(feature = "classic-affine-quake-specialized-kernel", inline(always))]
unsafe fn subdivide_twice<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    underdraw_edges: u8,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root0, root2));
        ptr::write(scratch.add(3), midpoint(root0, &*scratch));
        ptr::write(scratch.add(4), midpoint(root1, &*scratch));
        ptr::write(scratch.add(5), midpoint(root1, &*scratch.add(1)));
        ptr::write(scratch.add(6), midpoint(&*scratch.add(1), root2));
        ptr::write(scratch.add(7), midpoint(&*scratch.add(2), root2));
        ptr::write(scratch.add(8), midpoint(&*scratch.add(2), root0));
        ptr::write(scratch.add(9), midpoint(&*scratch.add(2), &*scratch));
        ptr::write(
            scratch.add(10),
            midpoint(&*scratch.add(9), &*scratch.add(5)),
        );
        ptr::write(
            scratch.add(11),
            midpoint(&*scratch.add(2), &*scratch.add(1)),
        );
        project_three_consecutive(scratch);
        project_three_consecutive(scratch.add(3));
        project_three_consecutive(scratch.add(6));
        project_three_consecutive(scratch.add(9));
    }
    let v = unsafe { core::slice::from_raw_parts(scratch, EXTRA_VERTICES) };
    let lattice_visible =
        unsafe { projected_lattice_inside(writer, [root0, root1, root2], scratch, EXTRA_VERTICES) };
    unsafe {
        sorted_tri(
            writer,
            [root0, &v[3], &v[8]],
            [root0, &v[3], &v[8]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[8], &v[9], &v[2]],
            [&v[8], &v[9], &v[2]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[2], &v[11], &v[7]],
            [&v[2], &v[11], &v[7]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[7], &v[6], root2],
            [&v[7], &v[6], root2],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[3], &v[0], &v[8], &v[9]],
            [&v[3], &v[0], &v[8], &v[9]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[0], &v[4], &v[9], &v[10]],
            [&v[0], &v[4], &v[9], &v[10]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[4], root1, &v[10], &v[5]],
            [&v[4], root1, &v[10], &v[5]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[9], &v[10], &v[2], &v[11]],
            [&v[9], &v[10], &v[2], &v[11]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[10], &v[5], &v[11], &v[1]],
            [&v[10], &v[5], &v[11], &v[1]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[11], &v[1], &v[7], &v[6]],
            [&v[11], &v[1], &v[7], &v[6]],
            lattice_visible,
        );
    }
    let profile = writer.profile();
    let underdraw_at = i32::from(profile.subdivide_twice_at);
    if root0.depth >= underdraw_at || root1.depth >= underdraw_at || root2.depth >= underdraw_at {
        let far = (root0.depth as u16)
            .max(root1.depth as u16)
            .max(root2.depth as u16);
        let underdraw = underlay_otz(root_otz, far, profile);
        unsafe {
            // GP0(3Ch) splits [a,b,c,d] into [b,d,c] then [a,b,c].
            // These orders are cyclic rotations of the two old triangles in
            // their exact OT draw order, so the crack-sealing raster stays
            // bit-identical while each edge pair needs one packet setup.
            if underdraw_edges & 1 != 0 {
                emit_classified_quad(
                    writer,
                    [root1, &v[0], root0, &v[3]],
                    [root1, &v[0], root0, &v[3]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[0], root1, &v[4]],
                    [&v[0], root1, &v[4]],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 2 != 0 {
                emit_classified_quad(
                    writer,
                    [root2, &v[1], root1, &v[5]],
                    [root2, &v[1], root1, &v[5]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[1], root2, &v[6]],
                    [&v[1], root2, &v[6]],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 4 != 0 {
                emit_classified_quad(
                    writer,
                    [root2, &v[2], root0, &v[8]],
                    [root2, &v[2], root0, &v[8]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[2], root2, &v[7]],
                    [&v[2], root2, &v[7]],
                    underdraw,
                    lattice_visible,
                );
            }
        }
    }
}

/// Sentinel for "no face-wide level": the historical per-root schedule.
const PER_ROOT_LEVEL: u8 = u8::MAX;

/// The error-bounded and quad-lattice fan paths (feature
/// `classic-affine-lattice`). Everything here is inlined into the fan
/// submitter on purpose: lending the writer to an out-of-line call takes its
/// address, and LLVM then keeps it in memory across the historical fan loop
/// as well (measured +15% to +42% Quake frame work). The feature is off
/// where code size or stack depth binds (Cortex's PXBSP writer runs on a
/// scratchpad stack with 8 bytes to spare).
#[cfg(feature = "classic-affine-lattice")]
mod lattice {
    use super::*;

    /// Largest span, in pixels, of a primitive the GPU draws: it drops any
    /// triangle two of whose vertices lie more than 1023 pixels apart across
    /// or 511 down (psx-spx "GPU Render Polygon Commands"; PSoXide's
    /// rasteriser gates on the same rule in `triangle_exceeds_hw_extent`).
    const GPU_MAX_SPAN_X: i32 = 1023;
    const GPU_MAX_SPAN_Y: i32 = 511;

    /// Level a face needs so that no primitive it emits breaks the GPU's
    /// extent limit: 2 when its screen box is wider or taller than one
    /// primitive may be, else 0. The warp bound alone leaves a face that
    /// faces the camera unsplit however large it is on screen, and the GPU
    /// would drop it whole (the wall strips beside a wall the player stands
    /// at). Two levels match the depth bands' finest split.
    #[inline(always)]
    fn gpu_extent_level(dx: i32, dy: i32) -> u8 {
        if dx > GPU_MAX_SPAN_X || dy > GPU_MAX_SPAN_Y {
            2
        } else {
            0
        }
    }

    /// Error-bounded lattice level shared by every root of a face.
    ///
    /// An edge's affine displacement is `L |zb - za| / (2 (za + zb))`. Of all
    /// the face's edges and diagonals, the pair of extreme depths has the
    /// largest depth ratio, and the screen bounding box's diagonal is at
    /// least as long as any of them, so one test bounds every root at once,
    /// and the whole face sharing one level leaves no T-junction on its
    /// internal diagonals. Faces beyond `gate_depth` skip the screen pass
    /// (see [`ClassicAffineProfile::error_gate_depth`]); in front of it the
    /// level is at least [`gpu_extent_level`] of the screen box.
    #[inline(always)]
    pub(super) unsafe fn error_bounded_face_level_flagged(
        vertices: *const ClassicAffineVertex,
        vertex_count: usize,
        budget_q3: u32,
        gate_depth: u16,
    ) -> u8 {
        let end = unsafe { vertices.add(vertex_count) };
        let first = unsafe { &*vertices };
        let (mut z0, mut z1) = (first.depth, first.depth);
        let mut vertex = unsafe { vertices.add(1) };
        while vertex != end {
            let z = unsafe { (*vertex).depth };
            if z < z0 {
                z0 = z;
            } else if z > z1 {
                z1 = z;
            }
            vertex = unsafe { vertex.add(1) };
        }
        if z0 <= 0 || (gate_depth != 0 && z0 >= i32::from(gate_depth)) {
            return 0;
        }
        // i32 accumulators: the loads sign-extend once and the compares need
        // no re-extension.
        let (mut x0, mut y0) = (i32::from(first.screen[0]), i32::from(first.screen[1]));
        let (mut x1, mut y1) = (x0, y0);
        let mut vertex = unsafe { vertices.add(1) };
        while vertex != end {
            let v = unsafe { &*vertex };
            let (x, y) = (i32::from(v.screen[0]), i32::from(v.screen[1]));
            if x < x0 {
                x0 = x;
            } else if x > x1 {
                x1 = x;
            }
            if y < y0 {
                y0 = y;
            } else if y > y1 {
                y1 = y;
            }
            vertex = unsafe { vertex.add(1) };
        }
        let extent = gpu_extent_level(x1 - x0, y1 - y0);
        // No edge inside the guard band is longer than 2813 px.
        if psx_engine::tess::error_level(2813, z0, z1, budget_q3) == 0 {
            return extent | extent_flag(extent);
        }
        let (dx, dy) = ((x1 - x0).min(4095) as u32, (y1 - y0).min(4095) as u32);
        let span = if dx > dy {
            dx + ((dy * 3) >> 3)
        } else {
            dy + ((dx * 3) >> 3)
        };
        psx_engine::tess::error_level(span, z0, z1, budget_q3).max(extent) | extent_flag(extent)
    }

    /// Set in the level [`error_bounded_face_level_flagged`] returns when the
    /// face's screen box is over the GPU's extent limit.
    pub(super) const EXTENT_BIG: u8 = 0x80;

    #[inline(always)]
    fn extent_flag(extent: u8) -> u8 {
        if extent != 0 {
            EXTENT_BIG
        } else {
            0
        }
    }

    #[inline(always)]
    fn edge_error_level(a: &ClassicAffineVertex, b: &ClassicAffineVertex, budget_q3: u32) -> u8 {
        psx_engine::tess::error_level(
            psx_engine::tess::screen_span(a.screen, b.screen),
            a.depth,
            b.depth,
            budget_q3,
        )
    }

    /// Point `j / n` of the way from `a` to `b` (`n` is 2 or 4, `0 < j < n`)
    /// by the same recursive midpoints the triangle lattice takes, so both
    /// faces sharing an edge that is split as often generate identical
    /// vertices.
    #[inline(always)]
    fn lattice_edge_point(
        a: &ClassicAffineVertex,
        b: &ClassicAffineVertex,
        j: usize,
        n: usize,
    ) -> ClassicAffineVertex {
        let half = midpoint(a, b);
        if n == 2 || j == 2 {
            half
        } else if j == 1 {
            midpoint(a, &half)
        } else {
            midpoint(&half, b)
        }
    }

    /// Fill `row[0..=n]` with the points from `a` to `b` and project the ones
    /// that are new (`ends_projected`: `a` and `b` already carry
    /// projections).
    #[inline(always)]
    unsafe fn fill_lattice_row(
        row: *mut ClassicAffineVertex,
        a: ClassicAffineVertex,
        b: ClassicAffineVertex,
        n: usize,
        ends_projected: bool,
    ) {
        unsafe {
            ptr::write(row, a);
            ptr::write(row.add(n), b);
            if n == 2 {
                ptr::write(row.add(1), midpoint(&a, &b));
            } else if n == 4 {
                let half = midpoint(&a, &b);
                ptr::write(row.add(1), midpoint(&a, &half));
                ptr::write(row.add(2), half);
                ptr::write(row.add(3), midpoint(&half, &b));
            }
            if ends_projected {
                if n == 2 {
                    project_one(row.add(1));
                } else if n == 4 {
                    project_three_consecutive(row.add(1));
                }
            } else if n == 1 {
                project_one(row);
                project_one(row.add(1));
            } else if n == 2 {
                project_three_consecutive(row);
            } else {
                project_three_consecutive(row);
                project_three_consecutive(row.add(2));
            }
        }
    }

    /// Seal the crack between a split boundary edge `p[0..=n]` and a
    /// neighbour that drew it as one chord: the slivers between the chord
    /// and the split polyline, the same shapes the triangle lattice's
    /// underdraw takes.
    #[inline(always)]
    unsafe fn seal_split_edge<W: AffinePacketWriter>(
        writer: &mut W,
        p: [&ClassicAffineVertex; 5],
        n: usize,
        otz: u16,
        visible: bool,
    ) {
        unsafe {
            if n == 2 {
                emit_classified_tri(writer, [p[0], p[2], p[1]], [p[0], p[2], p[1]], otz, visible);
            } else if n == 4 {
                emit_classified_quad(
                    writer,
                    [p[4], p[2], p[0], p[1]],
                    [p[4], p[2], p[0], p[1]],
                    otz,
                    visible,
                );
                emit_classified_tri(writer, [p[2], p[4], p[3]], [p[2], p[4], p[3]], otz, visible);
            }
        }
    }

    /// Submission of a four-corner face (fan order `v0 v1 v2 v3`) as a quad
    /// lattice: `2^la` cells along `v0 -> v1` and `2^lb` along `v0 -> v3`,
    /// each axis split only as far as it needs, so a wall receding sideways
    /// takes a 4x1 strip where the triangle lattice would spend twenty
    /// packets on its two roots. Cells are GT4 in the GPU's Z order
    /// (`[p(i+1,j), p(i+1,j+1), p(i,j), p(i,j+1)]`), which puts the GPU's
    /// split on the same `v0 v2` diagonal the fan pairing uses. Rows are
    /// generated two at a time in `scratch` (at most ten records, inside the
    /// fan's twelve). Split boundary edges are sealed when a corner reaches
    /// the band depth ([`ClassicAffineProfile::subdivide_error_px_q3`]).
    ///
    /// Error-bounded (`subdivide_error_px_q3` set): each axis from its own
    /// two edges, the `v0 v2` diagonal counted on the axis with the larger
    /// depth change. Band mode: the face level on each axis whose edges
    /// change depth (a constant-depth edge maps affinely without error).
    #[inline(always)]
    pub(super) unsafe fn submit_quad_lattice<W: AffinePacketWriter>(
        vertices: *mut ClassicAffineVertex,
        scratch: *mut ClassicAffineVertex,
        writer: &mut W,
        face_level: u8,
    ) {
        let profile = writer.profile();
        let c = unsafe {
            [
                &*vertices,
                &*vertices.add(1),
                &*vertices.add(2),
                &*vertices.add(3),
            ]
        };
        let depth_sum = c.iter().map(|v| v.depth as u16 as u32).sum::<u32>();
        let face_otz = (depth_sum >> 4) as u16;
        if face_otz == 0 || face_otz >= profile.ot_depth {
            writer.topology_event(0);
            return;
        }
        let dz = |a: &ClassicAffineVertex, b: &ClassicAffineVertex| a.depth.abs_diff(b.depth);
        let dz_a = dz(c[0], c[1]) + dz(c[3], c[2]);
        let dz_b = dz(c[1], c[2]) + dz(c[0], c[3]);
        let (mut la, mut lb) = (0u8, 0u8);
        if face_level != 0 && profile.subdivide_error_px_q3 == 0 {
            if dz_a * 16 > depth_sum {
                la = face_level;
            }
            if dz_b * 16 > depth_sum {
                lb = face_level;
            }
        } else if face_level != 0 {
            let budget = u32::from(profile.subdivide_error_px_q3);
            la = edge_error_level(c[0], c[1], budget).max(edge_error_level(c[3], c[2], budget));
            lb = edge_error_level(c[1], c[2], budget).max(edge_error_level(c[0], c[3], budget));
            let diagonal = edge_error_level(c[0], c[2], budget);
            if diagonal > la.max(lb) {
                if dz_a >= dz_b {
                    la = diagonal;
                } else {
                    lb = diagonal;
                }
            }
            // The edges' warp can be zero on a face the GPU would still drop
            // for its size: split both axes as far as the face needs.
            let span = |axis: usize| {
                let values = c.map(|v| i32::from(v.screen[axis]));
                values.iter().max().unwrap_or(&0) - values.iter().min().unwrap_or(&0)
            };
            let extent = gpu_extent_level(span(0), span(1));
            la = la.max(extent);
            lb = lb.max(extent);
        }
        if la == 0 && lb == 0 {
            writer.topology_event(2);
            let quad = [c[1], c[2], c[0], c[3]];
            unsafe { writer.emit_quad(quad, quad, face_otz) };
            return;
        }
        writer.topology_event(if la.max(lb) == 2 { 5 } else { 3 });

        let na = 1usize << la;
        let nb = 1usize << lb;
        let visible = unsafe { projected_lattice_inside(writer, [c[0], c[1], c[2]], scratch, 10) };
        let seal_at = i32::from(if la.max(lb) == 2 {
            profile.subdivide_twice_at
        } else {
            profile.subdivide_once_at
        });
        let seal = c.iter().any(|v| v.depth >= seal_at);
        let far = c.iter().map(|v| v.depth as u16).max().unwrap_or(0);
        let seal_otz = underlay_otz(face_otz, far, profile);
        let ends = |r: &[ClassicAffineVertex]| -> [*const ClassicAffineVertex; 5] {
            let at = |k: usize| &r[k.min(na)] as *const ClassicAffineVertex;
            [at(0), at(1), at(2), at(3), at(na)]
        };

        let rows = [scratch, unsafe { scratch.add(5) }];
        unsafe { fill_lattice_row(rows[0], *c[0], *c[1], na, true) };
        if seal && na > 1 {
            let r = unsafe { core::slice::from_raw_parts(rows[0], na + 1) };
            let e = ends(r);
            unsafe { seal_split_edge(writer, e.map(|v| &*v), na, seal_otz, visible) };
        }
        let mut j = 1usize;
        while j <= nb {
            let prev = rows[(j - 1) & 1];
            let cur = rows[j & 1];
            if j == nb {
                unsafe { fill_lattice_row(cur, *c[3], *c[2], na, true) };
            } else {
                let left = lattice_edge_point(c[0], c[3], j, nb);
                let right = lattice_edge_point(c[1], c[2], j, nb);
                unsafe { fill_lattice_row(cur, left, right, na, false) };
            }
            let p = unsafe { core::slice::from_raw_parts(prev, na + 1) };
            let q = unsafe { core::slice::from_raw_parts(cur, na + 1) };
            let mut i = 0usize;
            while i < na {
                let cell = [&p[i + 1], &q[i + 1], &p[i], &q[i]];
                unsafe { sorted_quad(writer, cell, cell, visible) };
                i += 1;
            }
            if seal && nb > 1 {
                // Column edges v0-v3 (row starts) and v1-v2 (row ends): their
                // slivers need rows 1 and 2 (the quad) or rows 2 and 3 (the
                // triangle) of a four-row lattice, or row 1 of a two-row one.
                unsafe {
                    if nb == 2 && j == 1 {
                        let (s0, s1) = ([c[0], c[3], &q[0]], [c[1], c[2], &q[na]]);
                        emit_classified_tri(writer, s0, s0, seal_otz, visible);
                        emit_classified_tri(writer, s1, s1, seal_otz, visible);
                    } else if nb == 4 && j == 2 {
                        let s0 = [c[3], &q[0], c[0], &p[0]];
                        let s1 = [c[2], &q[na], c[1], &p[na]];
                        emit_classified_quad(writer, s0, s0, seal_otz, visible);
                        emit_classified_quad(writer, s1, s1, seal_otz, visible);
                    } else if nb == 4 && j == 3 {
                        let (s0, s1) = ([&p[0], c[3], &q[0]], [&p[na], c[2], &q[na]]);
                        emit_classified_tri(writer, s0, s0, seal_otz, visible);
                        emit_classified_tri(writer, s1, s1, seal_otz, visible);
                    }
                }
            }
            if j == nb && seal && na > 1 {
                let e = ends(q);
                unsafe { seal_split_edge(writer, e.map(|v| &*v), na, seal_otz, visible) };
            }
            j += 1;
        }
    }

    /// Band-mode face level of a four-corner face: the deeper of its two
    /// roots' band levels (zero when a root falls outside the OT, so the
    /// historical fan handles it).
    #[inline(always)]
    pub(super) unsafe fn band_quad_level(
        vertices: *const ClassicAffineVertex,
        profile: ClassicAffineProfile,
    ) -> u8 {
        let z = unsafe { [0, 1, 2, 3].map(|k| (*vertices.add(k)).depth as u16) };
        let band = |otz: u16| {
            if otz == 0 || otz >= profile.ot_depth {
                PER_ROOT_LEVEL
            } else if otz < profile.subdivide_twice_at {
                2
            } else if otz < profile.subdivide_once_at {
                1
            } else {
                0
            }
        };
        let (first, second) = (
            band(average3_depths(z[0], z[1], z[2])),
            band(average3_depths(z[0], z[2], z[3])),
        );
        if first == PER_ROOT_LEVEL || second == PER_ROOT_LEVEL {
            0
        } else {
            first.max(second)
        }
    }
}

#[inline(always)]
unsafe fn submit_classic_affine_projected_fan_into_writer<W: AffinePacketWriter>(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut W,
) {
    let profile = writer.profile();
    if !W::SURFACE_USES_GPU_CLIP {
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < vertex_count && surface_clip != 0 {
            surface_clip &=
                classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip != 0 {
            writer.topology_event(15);
            return;
        }
    }
    writer.topology_event(14);

    #[cfg(feature = "classic-affine-lattice")]
    debug_assert!(
        W::USES_LATTICE || (profile.subdivide_error_px_q3 == 0 && !profile.quad_lattice),
        "this writer is built without the lattice paths"
    );
    #[allow(unused_mut)]
    let mut extent_repair = false;
    let repair_start = writer.packet_cursor();
    #[cfg(feature = "classic-affine-lattice")]
    let face_level = if !W::USES_LATTICE {
        PER_ROOT_LEVEL
    } else if profile.subdivide_error_px_q3 != 0 {
        let flagged = unsafe {
            lattice::error_bounded_face_level_flagged(
                vertices,
                vertex_count,
                u32::from(profile.subdivide_error_px_q3),
                profile.error_gate_depth,
            )
        };
        let level = flagged & !lattice::EXTENT_BIG;
        extent_repair = flagged & lattice::EXTENT_BIG != 0;
        if vertex_count == 4 {
            unsafe { lattice::submit_quad_lattice(vertices, generated, writer, level) };
            if extent_repair {
                unsafe {
                    writer.repair_oversize_packets(
                        repair_start,
                        (vertex_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE,
                    )
                };
            }
            return;
        }
        level
    } else {
        if profile.quad_lattice && vertex_count == 4 {
            let level = unsafe { lattice::band_quad_level(vertices, profile) };
            if level != 0 {
                unsafe { lattice::submit_quad_lattice(vertices, generated, writer, level) };
                return;
            }
        }
        PER_ROOT_LEVEL
    };
    #[cfg(not(feature = "classic-affine-lattice"))]
    let face_level = PER_ROOT_LEVEL;

    let root = unsafe { &*vertices };
    let root_depth = root.depth as u16;
    let end = unsafe { vertices.add(vertex_count) };
    let first_previous = unsafe { vertices.add(1) };
    let mut previous = first_previous;
    let mut current = unsafe { vertices.add(2) };

    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_depth,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        let key_otz = otz;
        if otz > 0 && otz < profile.ot_depth {
            let subdivision_level = if face_level != PER_ROOT_LEVEL {
                face_level
            } else {
                classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile)
            };
            let next = unsafe { current.add(1) };
            if subdivision_level == 0 && next != end {
                let next_ref = unsafe { &*next };
                let next_otz =
                    average3_depths(root_depth, current_ref.depth as u16, next_ref.depth as u16);
                let next_level = if face_level != PER_ROOT_LEVEL {
                    face_level
                } else {
                    classic_affine_subdivision_level(
                        [root, current_ref, next_ref],
                        next_otz,
                        profile,
                    )
                };
                let compatible_depth = next_otz == otz;
                if next_otz > 0
                    && next_otz < profile.ot_depth
                    && compatible_depth
                    && next_level == 0
                {
                    // GP0 quads split on q1-q2. Reorder two adjacent fan
                    // triangles so that edge lands on the fan's shared 0-2
                    // diagonal and its two internal triangles match the
                    // staged OT stream's reverse link order. This keeps the
                    // affine interpolation anchors bit-exact at the seam.
                    writer.topology_event(2);
                    let quad_refs = unsafe { [&*previous, &*current, root, &*next] };

                    unsafe { writer.emit_quad(quad_refs, quad_refs, otz) };

                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }

            // A face-wide level splits every internal diagonal the same way
            // on both sides, so only the face's own edges can meet a
            // neighbour split differently.
            let underdraw_edges = if face_level == PER_ROOT_LEVEL {
                7
            } else {
                2 | u8::from(previous == first_previous) | (u8::from(next == end) << 2)
            };
            if subdivision_level == 2 {
                let underdraw_at = i32::from(profile.subdivide_twice_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                writer.topology_event(if underdraw { 6 } else { 5 });
                unsafe {
                    writer.emit_subdivide_twice(
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        key_otz,
                        underdraw_edges,
                    )
                };
            } else if subdivision_level == 1 {
                let underdraw_at = i32::from(profile.subdivide_once_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                writer.topology_event(if underdraw { 4 } else { 3 });
                unsafe {
                    subdivide_once(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        key_otz,
                        underdraw_edges,
                    )
                };
            } else {
                writer.topology_event(1);
                let root_refs = [root, previous_ref, current_ref];
                unsafe { writer.emit_tri(root_refs, root_refs, key_otz) };
            }
        } else {
            writer.topology_event(0);
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
    if extent_repair {
        unsafe {
            writer.repair_oversize_packets(
                repair_start,
                (vertex_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE,
            )
        };
    }
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

/// Project and submit several contiguous convex fans as one scheduled batch.
///
/// Root vertices from adjacent surfaces share RTPT groups, avoiding the RTPS
/// tails paid when every small fan is projected independently. Surfaces are
/// still submitted in descriptor order with their own material state and the
/// exact same subdivision, quad-pairing, clipping, and underdraw rules as
/// [`submit_classic_affine_fan`].
///
/// # Safety
/// `vertices` must point to `vertex_count + 12` writable records, with the
/// final records reserved for shared subdivision scratch. `surfaces` must
/// contain `surface_count` descriptors whose vertex ranges fit entirely in
/// the first `vertex_count` records. `output` must have room for every fan's
/// worst-case packet expansion and remain live until submission completes.
#[cfg_attr(feature = "classic-affine-quake-specialized-kernel", inline(always))]
pub unsafe fn submit_classic_affine_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(vertex)) };
        vertex += 3;
    }
    while vertex < vertex_count {
        unsafe { project_one(vertices.add(vertex)) };
        vertex += 1;
    }

    let generated = unsafe { vertices.add(vertex_count) };
    let mut writer = PacketWriter {
        next: output,
        packets: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
        profile,
    };
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        writer.tpage_high_word = (surface.tpage as u32) << 16;
        writer.clut_high_word = (surface.clut as u32) << 16;

        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut writer,
            );
        }

        surface_ptr = unsafe { surface_ptr.add(1) };
    }

    unsafe { writer.finish(output) }
}

/// Quake-specific ordinary batch entry point with a compile-time renderer
/// profile. This is intentionally a separate non-calling ownership boundary:
/// the inlined generic body can discard runtime tests for affine-error policy,
/// viewport dimensions, OT depth and subdivision bands while the public
/// general entry point remains available to other PSoXide users.
///
/// # Safety
/// The pointer, capacity and scratch contracts are identical to
/// [`submit_classic_affine_batch`].
#[cfg(feature = "classic-affine-quake-specialized-kernel")]
#[inline(always)]
pub unsafe fn submit_quake_classic_affine_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    unsafe {
        submit_quake_classic_affine_batch_budget(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            ClassicAffineProfile::QUAKE_ERROR_BOUNDED.subdivide_error_px_q3,
        )
    }
}

/// Error budget, in eighths of a pixel, that
/// [`submit_quake_classic_affine_batch_budget`] callers can switch to when
/// their packet space runs short: four times the normal 16-pixel budget.
/// Faces still split as far as the GPU's extent limit needs.
pub const QUAKE_COARSE_ERROR_BUDGET_Q3: u8 = 255;

/// [`submit_quake_classic_affine_batch`] with the error-bounded budget
/// (`subdivide_error_px_q3`, eighths of a pixel) chosen per batch, so a
/// caller close to the end of its packet arena can trade warp for packets
/// (see [`QUAKE_COARSE_ERROR_BUDGET_Q3`]) instead of dropping faces. The
/// rest of the profile stays compile-time. Zero selects the depth bands of
/// [`ClassicAffineProfile::QUAKE_REFERENCE`]. Without the
/// `classic-affine-lattice` feature the budget is ignored and the kernel is
/// the reference one.
///
/// # Safety
/// The pointer, capacity and scratch contracts are identical to
/// [`submit_classic_affine_batch`].
#[cfg(feature = "classic-affine-quake-specialized-kernel")]
#[inline(never)]
pub unsafe fn submit_quake_classic_affine_batch_budget(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
    budget_q3: u8,
) -> ClassicAffineSubmit {
    #[cfg(feature = "classic-affine-lattice")]
    {
        if budget_q3 != 0 {
            return unsafe {
                quake_kernel::submit_error_bounded(
                    vertices,
                    vertex_count,
                    surfaces,
                    surface_count,
                    output,
                    budget_q3,
                )
            };
        }
        unsafe {
            quake_kernel::submit_reference(vertices, vertex_count, surfaces, surface_count, output)
        }
    }
    #[cfg(not(feature = "classic-affine-lattice"))]
    {
        let _ = budget_q3;
        unsafe {
            submit_classic_affine_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        }
    }
}

/// Quake's error-bounded world kernel, laid out for the PS1's 4 KB
/// direct-mapped instruction cache.
///
/// [`submit_classic_affine_batch`] inlines every fan, lattice and
/// subdivision path into one 18 KB body. On the E1M1 chain bench a call ran
/// up to 5.7 KB of it and refilled about 3 KB, because its surface loop
/// shared cache sets with its own split paths 4 KB and 8 KB further on.
/// Here the per-call path (projection, surface rejection,
/// the face's error level, unsplit quads and unsplit fans) is a 2 KB entry
/// function plus two frameless packet leaves. A fan that splits (under half
/// a face per call) runs one out-of-line function per face with the split
/// bodies inlined, and a split quad (rarer still) another. The packets are
/// the same, in the same order, as the generic kernel's with
/// [`quake_error_bounded_profile`].
#[cfg(all(
    feature = "classic-affine-quake-specialized-kernel",
    feature = "classic-affine-lattice"
))]
mod quake_kernel {
    use super::*;

    type V = ClassicAffineVertex;

    // A batch descriptor is read as two words, low halfword first.
    const _: () = assert!(cfg!(target_endian = "little"));

    /// Budget zero: the historical depth bands, kept out of the hot body.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn submit_reference(
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineBatchSurface,
        surface_count: usize,
        output: *mut u32,
    ) -> ClassicAffineSubmit {
        unsafe {
            submit_classic_affine_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        }
    }

    /// A surface's GP0 material as one word: tpage in the low half, CLUT in
    /// the high half, which is the second word of a
    /// [`ClassicAffineBatchSurface`] as it lies in memory. One register
    /// instead of two keeps both out of the RAM stack in the fan loop.
    #[inline(always)]
    fn material_writer(
        next: *mut u32,
        material: u32,
        profile: ClassicAffineProfile,
    ) -> PacketWriter {
        PacketWriter {
            next,
            packets: 0,
            clut_high_word: material & 0xffff_0000,
            tpage_high_word: material << 16,
            profile,
        }
    }

    /// The writer of a packet leaf. A single packet reads only the screen
    /// size of its profile (in the CPU rejection the GPU-clip features
    /// remove), and every Quake budget shares it.
    #[inline(always)]
    fn leaf_writer(next: *mut u32, material: u32) -> PacketWriter {
        material_writer(next, material, ClassicAffineProfile::QUAKE_ERROR_BOUNDED)
    }

    /// The error-bounded batch (`budget_q3 != 0`).
    #[inline(always)]
    pub(super) unsafe fn submit_error_bounded(
        vertices: *mut ClassicAffineVertex,
        vertex_count: usize,
        surfaces: *const ClassicAffineBatchSurface,
        surface_count: usize,
        output: *mut u32,
        budget_q3: u8,
    ) -> ClassicAffineSubmit {
        if vertices.is_null()
            || surfaces.is_null()
            || output.is_null()
            || vertex_count == 0
            || surface_count == 0
        {
            return ClassicAffineSubmit {
                next_packet: output,
                packets: 0,
                hardware_triangles: 0,
            };
        }

        let mut vertex = 0usize;
        while vertex + 2 < vertex_count {
            unsafe { project_three_consecutive(vertices.add(vertex)) };
            vertex += 3;
        }
        while vertex < vertex_count {
            unsafe { project_one(vertices.add(vertex)) };
            vertex += 1;
        }

        let profile = quake_error_bounded_profile(budget_q3);
        let budget = u32::from(budget_q3);
        let generated = unsafe { vertices.add(vertex_count) };
        let mut next_packet = output;
        let mut packets = 0u32;
        let surface_end = unsafe { surfaces.add(surface_count) };
        let mut surface_ptr = surfaces;
        while surface_ptr != surface_end {
            // Two word loads instead of four halfword ones: the descriptors
            // sit in RAM, where each load stalls.
            let [range, material] = unsafe { ptr::read(surface_ptr.cast::<[u32; 2]>()) };
            surface_ptr = unsafe { surface_ptr.add(1) };
            // Opaque per surface, so the loops below address the vertices
            // from it instead of from hoisted copies parked on the RAM stack.
            let first = unsafe { opaque(vertices).add((range & 0xffff) as usize) };
            let count = (range >> 16) as usize;
            debug_assert!(count >= 3);
            debug_assert!((range & 0xffff) as usize + count <= vertex_count);

            // Whole-surface screen rejection.
            let mut surface_clip = 0x0fu8;
            let mut clip_index = 0usize;
            while clip_index < count && surface_clip != 0 {
                surface_clip &=
                    classic_clip_code(unsafe { (*first.add(clip_index)).screen }, profile);
                clip_index += 1;
            }
            if surface_clip != 0 {
                continue;
            }

            let flagged = unsafe {
                lattice::error_bounded_face_level_flagged(
                    first,
                    count,
                    budget,
                    profile.error_gate_depth,
                )
            };
            let level = flagged & !lattice::EXTENT_BIG;
            if level != 0 {
                // The split paths read the budget from the profile.
                let mut lent = material_writer(next_packet, material, profile);
                unsafe {
                    if count == 4 {
                        split_quad(first, generated, &mut lent, level);
                    } else {
                        split_fan(first, count, generated, &mut lent, level);
                    }
                    if flagged & lattice::EXTENT_BIG != 0 {
                        split_oversize_packets(
                            &mut lent,
                            next_packet,
                            (count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE,
                        );
                    }
                }
                next_packet = lent.next;
                packets += lent.packets;
                continue;
            }
            if count == 4 {
                // `lattice::submit_quad_lattice` at face level zero: one GT4
                // in the GPU's Z order when the face is in the OT.
                let c = unsafe { [&*first, &*first.add(1), &*first.add(2), &*first.add(3)] };
                let depth_sum = c[0].depth as u16 as u32
                    + c[1].depth as u16 as u32
                    + c[2].depth as u16 as u32
                    + c[3].depth as u16 as u32;
                let face_otz = (depth_sum >> 4) as u16;
                if face_otz != 0 && face_otz < profile.ot_depth {
                    let next = unsafe {
                        leaf_quad(next_packet, [c[1], c[2], c[0], c[3]], face_otz, material)
                    };
                    packets += u32::from(next != next_packet);
                    next_packet = next;
                }
                continue;
            }

            // Unsplit fan: adjacent roots at one OT key pair into a GT4
            // (see `submit_classic_affine_projected_fan_into_writer`).
            // `previous` is always the vertex before `current` (a pair steps
            // both by two), and the root's depth is a one-cycle scratchpad
            // load: neither needs a register across the leaf calls.
            let root = unsafe { &*first };
            let end = unsafe { first.add(count) };
            let mut current = unsafe { first.add(2) };
            while current != end {
                let previous_ref = unsafe { &*current.sub(1) };
                let current_ref = unsafe { &*current };
                let root_depth = unsafe { ptr::read_volatile(ptr::addr_of!(root.depth)) } as u16;
                let otz = average3_depths(
                    root_depth,
                    previous_ref.depth as u16,
                    current_ref.depth as u16,
                );
                if otz > 0 && otz < profile.ot_depth {
                    let next = unsafe { current.add(1) };
                    if next != end {
                        let next_ref = unsafe { &*next };
                        let next_otz = average3_depths(
                            root_depth,
                            current_ref.depth as u16,
                            next_ref.depth as u16,
                        );
                        if next_otz > 0 && next_otz < profile.ot_depth && next_otz == otz {
                            let emitted = unsafe {
                                leaf_quad(
                                    next_packet,
                                    [previous_ref, current_ref, root, next_ref],
                                    otz,
                                    material,
                                )
                            };
                            packets += u32::from(emitted != next_packet);
                            next_packet = emitted;
                            current = unsafe { next.add(1) };
                            continue;
                        }
                    }
                    let emitted = unsafe {
                        leaf_tri(
                            next_packet,
                            [root, previous_ref, current_ref],
                            otz,
                            material,
                        )
                    };
                    packets += u32::from(emitted != next_packet);
                    next_packet = emitted;
                }
                current = unsafe { current.add(1) };
            }
        }

        let writer = leaf_writer(next_packet, 0);
        unsafe { PacketWriter { packets, ..writer }.finish(output) }
    }

    /// The hot loop's packet emitters as frameless leaf calls. Inlined, LLVM
    /// merged the GT3 and GT4 store tails into one sequence addressed through
    /// a register per packet word, and the registers that took pushed the
    /// material words onto the RAM stack, reloaded for every packet. Each
    /// returns the cursor after its packet (unmoved when the writer rejects
    /// the primitive).
    #[inline(always)]
    unsafe fn leaf_tri(next: *mut u32, tri: [&V; 3], otz: u16, material: u32) -> *mut u32 {
        unsafe { leaf_tri_words(next, tri[0], tri[1], tri[2], otz, material) }
    }

    #[inline(always)]
    unsafe fn leaf_quad(next: *mut u32, quad: [&V; 4], otz: u16, material: u32) -> *mut u32 {
        unsafe { leaf_quad_words(next, quad[0], quad[1], quad[2], quad[3], otz, material) }
    }

    #[inline(never)]
    unsafe fn leaf_tri_words(
        next: *mut u32,
        a: *const V,
        b: *const V,
        c: *const V,
        otz: u16,
        material: u32,
    ) -> *mut u32 {
        let mut writer = leaf_writer(next, material);
        let tri = unsafe { [&*a, &*b, &*c] };
        unsafe { writer.emit_tri(tri, tri, otz) };
        writer.next
    }

    #[inline(never)]
    unsafe fn leaf_quad_words(
        next: *mut u32,
        a: *const V,
        b: *const V,
        c: *const V,
        d: *const V,
        otz: u16,
        material: u32,
    ) -> *mut u32 {
        let mut writer = leaf_writer(next, material);
        let quad = unsafe { [&*a, &*b, &*c, &*d] };
        unsafe { writer.emit_quad(quad, quad, otz) };
        writer.next
    }

    /// A four-corner face whose error level is not zero: the quad lattice.
    #[cold]
    #[inline(never)]
    unsafe fn split_quad(
        first: *mut ClassicAffineVertex,
        generated: *mut ClassicAffineVertex,
        writer: &mut PacketWriter,
        level: u8,
    ) {
        unsafe { lattice::submit_quad_lattice(first, generated, writer, level) };
    }

    /// The same pointer, opaque to the optimiser, so addresses derived from
    /// it are recomputed where they are used instead of hoisted out of a
    /// loop and spilled to the RAM stack.
    #[inline(always)]
    fn opaque(pointer: *mut V) -> *mut V {
        #[cfg(target_arch = "mips")]
        {
            let mut pointer = pointer;
            unsafe {
                core::arch::asm!(
                    "# {0}",
                    inout(reg) pointer,
                    options(nomem, nostack, preserves_flags)
                )
            };
            pointer
        }
        #[cfg(not(target_arch = "mips"))]
        pointer
    }

    /// A fan whose error level is not zero: every root splits the same way
    /// (`submit_classic_affine_projected_fan_into_writer` with a face-wide
    /// level, which never pairs roots). The split bodies inline here, so a
    /// face pays one call and one register save, not one per root.
    #[inline(never)]
    unsafe fn split_fan(
        first: *mut ClassicAffineVertex,
        count: usize,
        generated: *mut ClassicAffineVertex,
        lent: &mut PacketWriter,
        level: u8,
    ) {
        // A local copy, so the cursor stays in registers across the roots.
        let mut writer = PacketWriter { ..*lent };
        let writer = &mut writer;
        let root = unsafe { &*first };
        let ot_depth = ClassicAffineProfile::QUAKE_ERROR_BOUNDED.ot_depth;
        let end = unsafe { first.add(count) };
        let first_previous = unsafe { first.add(1) };
        let mut previous = first_previous;
        let mut current = unsafe { first.add(2) };
        while current != end {
            let scratch = opaque(generated);
            let previous_ref = unsafe { &*previous };
            let current_ref = unsafe { &*current };
            let otz = average3_depths(
                root.depth as u16,
                previous_ref.depth as u16,
                current_ref.depth as u16,
            );
            if otz > 0 && otz < ot_depth {
                let next = unsafe { current.add(1) };
                let underdraw_edges =
                    2 | u8::from(previous == first_previous) | (u8::from(next == end) << 2);
                if level == 2 {
                    unsafe {
                        subdivide_twice(
                            writer,
                            root,
                            previous_ref,
                            current_ref,
                            scratch,
                            otz,
                            underdraw_edges,
                        )
                    };
                } else if level == 1 {
                    unsafe {
                        subdivide_once(
                            writer,
                            root,
                            previous_ref,
                            current_ref,
                            scratch,
                            otz,
                            underdraw_edges,
                        )
                    };
                } else {
                    let root_refs = [root, previous_ref, current_ref];
                    unsafe { writer.emit_tri(root_refs, root_refs, otz) };
                }
            }
            previous = current;
            current = unsafe { current.add(1) };
        }
        lent.next = writer.next;
        lent.packets = writer.packets;
    }
}

/// [`ClassicAffineProfile::QUAKE_ERROR_BOUNDED`] with `budget_q3` as its
/// error budget; zero keeps the reference bands.
#[inline(always)]
pub const fn quake_error_bounded_profile(budget_q3: u8) -> ClassicAffineProfile {
    if budget_q3 == 0 {
        ClassicAffineProfile::QUAKE_REFERENCE
    } else {
        ClassicAffineProfile {
            subdivide_error_px_q3: budget_q3,
            ..ClassicAffineProfile::QUAKE_ERROR_BOUNDED
        }
    }
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

// The CPU screen rejection the GPU-clip features remove.
#[allow(dead_code)]
#[inline(always)]
fn classic_tri_screen_clipped(screens: [[i16; 2]; 3], profile: ClassicAffineProfile) -> bool {
    classic_triangle_screen_rejected(
        screens,
        profile.screen_width as i32 - 1,
        profile.screen_height as i32 - 1,
    )
}

#[allow(dead_code)]
#[inline(always)]
fn classic_quad_screen_clipped(screens: [[i16; 2]; 4], profile: ClassicAffineProfile) -> bool {
    classic_quad_screen_rejected(
        screens,
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

    #[test]
    fn midpoint_matches_byte_attribute_and_signed_position_averages() {
        let a = ClassicAffineVertex {
            position: [-3, 10, 21],
            uv: [1, 250],
            color: 0x0010_1010,
            ..ClassicAffineVertex::default()
        };
        let b = ClassicAffineVertex {
            position: [2, 15, 20],
            uv: [4, 10],
            color: 0x0020_2020,
            ..ClassicAffineVertex::default()
        };
        let mid = midpoint(&a, &b);
        assert_eq!(mid.position, [-1, 12, 20]);
        assert_eq!(mid.uv, [2, 130]);
        assert_eq!(mid.color, 0x0018_1818);
    }

    #[test]
    fn midpoint_averages_every_colour_channel() {
        // Baked RGB light on a PXBSP floor: teal corners must stay teal at
        // the generated vertex, not collapse to the red channel.
        let a = ClassicAffineVertex {
            color: 0x00ec_c62b,
            ..ClassicAffineVertex::default()
        };
        let b = ClassicAffineVertex {
            color: 0x00ff_db31,
            ..ClassicAffineVertex::default()
        };
        assert_eq!(midpoint(&a, &b).color, 0x00f5_d02e);
        let dark = ClassicAffineVertex {
            color: 0x0000_00ff,
            ..ClassicAffineVertex::default()
        };
        assert_eq!(midpoint(&dark, &dark).color, 0x0000_00ff);
    }

    #[test]
    fn midpoint_packed_uv_matches_independent_byte_averages() {
        for a in u8::MIN..=u8::MAX {
            for b in u8::MIN..=u8::MAX {
                let left = ClassicAffineVertex {
                    uv: [a, b],
                    ..ClassicAffineVertex::default()
                };
                let right = ClassicAffineVertex {
                    uv: [b, a],
                    ..ClassicAffineVertex::default()
                };
                let expected = ((a as u16 + b as u16) >> 1) as u8;
                assert_eq!(midpoint(&left, &right).uv, [expected; 2]);
            }
        }
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

    #[cfg(all(
        feature = "classic-affine-lattice",
        feature = "classic-affine-quake-specialized-kernel"
    ))]
    mod lattice_tests {
        use super::*;

        /// Records what a submitter emits: `(screens, otz)` per packet.
        struct RecordingWriter {
            profile: ClassicAffineProfile,
            tris: [([[i16; 2]; 3], u16); 64],
            quads: [([[i16; 2]; 4], u16); 64],
            tri_count: usize,
            quad_count: usize,
        }

        impl RecordingWriter {
            fn new(profile: ClassicAffineProfile) -> Self {
                Self {
                    profile,
                    tris: [([[0; 2]; 3], 0); 64],
                    quads: [([[0; 2]; 4], 0); 64],
                    tri_count: 0,
                    quad_count: 0,
                }
            }
        }

        impl AffinePacketWriter for RecordingWriter {
            fn profile(&self) -> ClassicAffineProfile {
                self.profile
            }

            unsafe fn emit_tri(
                &mut self,
                projected: [&ClassicAffineVertex; 3],
                _attributes: [&ClassicAffineVertex; 3],
                otz: u16,
            ) {
                self.tris[self.tri_count] = (projected.map(|v| v.screen), otz);
                self.tri_count += 1;
            }

            unsafe fn emit_quad(
                &mut self,
                projected: [&ClassicAffineVertex; 4],
                _attributes: [&ClassicAffineVertex; 4],
                otz: u16,
            ) {
                self.quads[self.quad_count] = (projected.map(|v| v.screen), otz);
                self.quad_count += 1;
            }
        }

        const ERROR_BOUNDED: ClassicAffineProfile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            subdivide_error_px_q3: 64,
            quad_lattice: true,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };

        /// Project a camera-space face (fan order) with the host GTE and submit
        /// it through the error-bounded fan path.
        fn submit_error_bounded_face(
            corners: &[[i16; 3]],
            profile: ClassicAffineProfile,
        ) -> RecordingWriter {
            psx_gte::host::reset();
            scene::set_screen_offset(160 << 16, 120 << 16);
            scene::set_projection_plane(160);
            scene::set_average_z_weights(0x155, 0x100);
            scene::load_rotation(&Mat3I16::IDENTITY);
            scene::load_translation(Vec3I32::ZERO);
            let mut vertices = [ClassicAffineVertex::default(); 6 + EXTRA_VERTICES];
            for (index, position) in corners.iter().enumerate() {
                vertices[index] = ClassicAffineVertex {
                    position: *position,
                    uv: [(index as u8 & 1) * 63, (index as u8 >> 1) * 63],
                    color: 0x0080_8080,
                    ..ClassicAffineVertex::default()
                };
                unsafe { project_one(vertices.as_mut_ptr().add(index)) };
            }
            let mut writer = RecordingWriter::new(profile);
            unsafe {
                submit_classic_affine_projected_fan_into_writer(
                    vertices.as_mut_ptr(),
                    corners.len(),
                    vertices.as_mut_ptr().add(corners.len()),
                    &mut writer,
                )
            };
            writer
        }

        /// Twice the signed area of the GPU's two triangles of a Z-ordered quad
        /// `[a, b, c, d]` (`a b c` then `b d c`): equal signs mean the quad is
        /// drawn without a bow-tie.
        fn z_order_halves(q: [[i16; 2]; 4]) -> (i32, i32) {
            let cross = |o: [i16; 2], p: [i16; 2], r: [i16; 2]| {
                (p[0] as i32 - o[0] as i32) * (r[1] as i32 - o[1] as i32)
                    - (p[1] as i32 - o[1] as i32) * (r[0] as i32 - o[0] as i32)
            };
            (cross(q[0], q[1], q[2]), cross(q[1], q[3], q[2]))
        }

        #[test]
        fn error_bounded_floor_splits_only_along_its_depth_axis() {
            // A floor receding from z = 150 to z = 1500: its near and far edges
            // have constant depth, so only the receding axis may split.
            let floor = [
                [-200, 100, 150],
                [200, 100, 150],
                [200, 100, 1500],
                [-200, 100, 1500],
            ];
            let unsealed = ClassicAffineProfile {
                subdivide_once_at: u16::MAX,
                subdivide_twice_at: u16::MAX,
                ..ERROR_BOUNDED
            };
            let w = submit_error_bounded_face(&floor, unsealed);
            assert_eq!((w.quad_count, w.tri_count), (4, 0), "a 1x4 strip of quads");
            for (q, _) in &w.quads[..w.quad_count] {
                let (a, b) = z_order_halves(*q);
                assert!(
                    a != 0 && b != 0 && (a > 0) == (b > 0),
                    "bow-tie or sliver {q:?}"
                );
            }
            // Consecutive rows share their edge exactly: cell k's far edge is
            // cell k+1's near edge (Z order [p(1,j), p(1,j+1), p(0,j), p(0,j+1)]).
            for k in 0..w.quad_count - 1 {
                let (a, _) = w.quads[k];
                let (b, _) = w.quads[k + 1];
                assert_eq!([a[1], a[3]], [b[0], b[2]]);
            }
            // Sealed, both receding edges get their crack slivers.
            let sealed = submit_error_bounded_face(&floor, ERROR_BOUNDED);
            assert_eq!((sealed.quad_count, sealed.tri_count), (4 + 2, 2));
        }

        #[test]
        fn error_bounded_quad_edges_use_the_triangle_lattice_midpoints() {
            // A wall receding sideways: its near-to-far edges split twice. The
            // generated edge vertices must be the recursive midpoints the
            // triangle lattice produces on a shared edge (mid, then the
            // midpoints of each half), or a neighbour split the same way would
            // meet it at a T-junction.
            let wall = [
                [-100, -120, 200],
                [-100, -120, 1400],
                [-100, 120, 1400],
                [-100, 120, 200],
            ];
            let w = submit_error_bounded_face(&wall, ERROR_BOUNDED);
            assert!(w.quad_count >= 4);
            let corner = |p: [i16; 3]| ClassicAffineVertex {
                position: p,
                ..ClassicAffineVertex::default()
            };
            let (a, b) = (corner(wall[0]), corner(wall[1]));
            let half = midpoint(&a, &b);
            let mut expected = [midpoint(&a, &half), half, midpoint(&half, &b)];
            for v in &mut expected {
                unsafe { project_one(v) };
            }
            for v in &expected {
                let found = w.quads[..w.quad_count]
                    .iter()
                    .any(|(q, _)| q.contains(&v.screen));
                assert!(
                    found,
                    "edge point {:?} missing from the quad lattice",
                    v.screen
                );
            }
        }

        /// Every emitted primitive that covers pixels keeps its vertices
        /// within the GPU's 1023 x 511 extent (a quad is two triangles sharing
        /// its vertices). Zero-area crack seals along a split edge are skipped:
        /// on a face this flat they cover nothing either way.
        fn assert_within_gpu_extent(w: &RecordingWriter) {
            let fits = |points: &[[i16; 2]]| {
                points.iter().all(|a| {
                    points.iter().all(|b| {
                        (i32::from(a[0]) - i32::from(b[0])).abs() <= 1023
                            && (i32::from(a[1]) - i32::from(b[1])).abs() <= 511
                    })
                })
            };
            let flat = |points: &[[i16; 2]]| {
                let cross = |o: [i16; 2], p: [i16; 2], r: [i16; 2]| {
                    (i32::from(p[0]) - i32::from(o[0])) * (i32::from(r[1]) - i32::from(o[1]))
                        - (i32::from(p[1]) - i32::from(o[1])) * (i32::from(r[0]) - i32::from(o[0]))
                };
                points.windows(3).all(|w| cross(w[0], w[1], w[2]) == 0)
            };
            let mut covering = 0;
            for (q, _) in &w.quads[..w.quad_count] {
                if !flat(q) {
                    assert!(fits(q), "quad {q:?} exceeds the GPU extent");
                    covering += 1;
                }
            }
            for (t, _) in &w.tris[..w.tri_count] {
                if !flat(t) {
                    assert!(fits(t), "triangle {t:?} exceeds the GPU extent");
                    covering += 1;
                }
            }
            assert!(covering > 1, "the face must split into several primitives");
        }

        #[test]
        fn error_bounded_face_taller_than_the_gpu_extent_splits() {
            // A wall strip square to the camera, 640 px tall on screen: no
            // depth change, so no warp, but one primitive over 511 px tall is
            // dropped by the GPU. Four corners take the quad lattice.
            let strip = [
                [-50, -200, 100],
                [50, -200, 100],
                [50, 200, 100],
                [-50, 200, 100],
            ];
            let w = submit_error_bounded_face(&strip, ERROR_BOUNDED);
            assert!(w.quad_count + w.tri_count > 1, "the strip must split");
            assert_within_gpu_extent(&w);
            // Five corners take the fan with the face-wide level.
            let pentagon = [
                [-50, -200, 100],
                [50, -200, 100],
                [60, 0, 100],
                [50, 200, 100],
                [-50, 200, 100],
            ];
            let w = submit_error_bounded_face(&pentagon, ERROR_BOUNDED);
            assert!(w.quad_count + w.tri_count > 3, "the fan must split");
            assert_within_gpu_extent(&w);
        }

        /// Submit one camera-space face through Quake's batch kernel and return
        /// how many packets the GPU would draw (skipped ones, whose slot is
        /// 0xffff, are not counted) and how many it would drop for their size.
        fn kernel_packets_for_face(corners: &[[i16; 3]]) -> (usize, usize, usize) {
            psx_gte::host::reset();
            scene::set_screen_offset(160 << 16, 120 << 16);
            scene::set_projection_plane(160);
            scene::set_average_z_weights(0x155, 0x100);
            scene::load_rotation(&Mat3I16::IDENTITY);
            scene::load_translation(Vec3I32::ZERO);
            let mut vertices = [ClassicAffineVertex::default(); 6 + EXTRA_VERTICES];
            for (index, position) in corners.iter().enumerate() {
                vertices[index] = ClassicAffineVertex {
                    position: *position,
                    uv: [(index as u8 & 1) * 63, (index as u8 >> 1) * 63],
                    color: 0x0080_8080,
                    ..ClassicAffineVertex::default()
                };
            }
            let surface = [ClassicAffineBatchSurface {
                first_vertex: 0,
                vertex_count: corners.len() as u16,
                tpage: 0,
                clut: 0,
            }];
            let mut words = [0u32; 16 * 1024];
            let submit = unsafe {
                submit_quake_classic_affine_batch_budget(
                    vertices.as_mut_ptr(),
                    corners.len(),
                    surface.as_ptr(),
                    1,
                    words.as_mut_ptr(),
                    ClassicAffineProfile::QUAKE_ERROR_BOUNDED.subdivide_error_px_q3,
                )
            };
            let written = unsafe { submit.next_packet.offset_from(words.as_ptr()) } as usize;
            assert!(written <= (corners.len() - 2) * WORST_PACKET_WORDS_PER_TRIANGLE);
            let (mut at, mut drawn, mut dropped, mut skipped) = (0usize, 0usize, 0usize, 0usize);
            while at < written {
                let body = (words[at] >> 24) as usize;
                if words[at] & 0xffff == 0xffff {
                    skipped += 1;
                } else {
                    let quad = body == ClassicQuadTexturedGouraud::WORDS as usize;
                    let xy = |corner: usize| {
                        let word = words[at + 2 + 3 * corner];
                        [i32::from(word as i16), i32::from((word >> 16) as i16)]
                    };
                    let over = |a: [i32; 2], b: [i32; 2], c: [i32; 2]| {
                        let span = |axis: usize| {
                            a[axis].max(b[axis]).max(c[axis]) - a[axis].min(b[axis]).min(c[axis])
                        };
                        span(0) > 1023 || span(1) > 511
                    };
                    drawn += 1;
                    if over(xy(0), xy(1), xy(2)) || (quad && over(xy(1), xy(2), xy(3))) {
                        dropped += 1;
                    }
                }
                at += 1 + body;
            }
            let _ = skipped;
            (drawn, dropped, skipped)
        }

        #[test]
        fn kernel_wall_beside_the_eye_keeps_every_cell_within_the_gpu_extent() {
            // A wall 60 units to the side that runs from 40 to 400 deep and
            // 800 tall, the shape of a lift shaft's wall seen from inside it.
            // Two lattice levels cut its 800 units into 200-unit rows, and the
            // row at the near end is 800 px tall: the GPU would drop that
            // cell and show the sky behind it.
            let wall = [
                [-60, -400, 40],
                [-60, -400, 400],
                [-60, 400, 400],
                [-60, 400, 40],
            ];
            let (drawn, dropped, skipped) = kernel_packets_for_face(&wall);
            assert!(skipped > 0, "the wall must have had cells over the limit");
            assert!(drawn > 16);
            assert_eq!(
                dropped, 0,
                "{dropped} of {drawn} packets exceed the GPU extent"
            );
        }

        #[test]
        fn kernel_fan_beside_the_eye_keeps_every_piece_within_the_gpu_extent() {
            // The same wall with a fifth corner, which takes the fan's
            // face-wide level instead of the quad lattice.
            let wall = [
                [-60, -400, 40],
                [-60, -400, 400],
                [-60, 0, 450],
                [-60, 400, 400],
                [-60, 400, 40],
            ];
            let (drawn, dropped, skipped) = kernel_packets_for_face(&wall);
            assert!(skipped > 0, "the fan must have had pieces over the limit");
            assert!(drawn > 16);
            assert_eq!(
                dropped, 0,
                "{dropped} of {drawn} packets exceed the GPU extent"
            );
        }

        #[test]
        fn error_bounded_face_within_the_gpu_extent_stays_whole() {
            // The same strip three times as deep is 213 px tall: one quad.
            let strip = [
                [-50, -200, 300],
                [50, -200, 300],
                [50, 200, 300],
                [-50, 200, 300],
            ];
            let w = submit_error_bounded_face(&strip, ERROR_BOUNDED);
            assert_eq!((w.quad_count, w.tri_count), (1, 0));
        }

        #[test]
        fn coarse_budget_emits_fewer_primitives_but_keeps_the_extent_floor() {
            // The receding floor splits less under the coarse budget than the
            // normal one, and the tall strip still splits for the GPU.
            let unsealed = |budget: u8| ClassicAffineProfile {
                subdivide_once_at: u16::MAX,
                subdivide_twice_at: u16::MAX,
                quad_lattice: true,
                ..quake_error_bounded_profile(budget)
            };
            let floor = [
                [-200, 100, 150],
                [200, 100, 150],
                [200, 100, 1500],
                [-200, 100, 1500],
            ];
            let normal = ClassicAffineProfile::QUAKE_ERROR_BOUNDED.subdivide_error_px_q3;
            let fine = submit_error_bounded_face(&floor, unsealed(normal));
            let coarse = submit_error_bounded_face(&floor, unsealed(QUAKE_COARSE_ERROR_BUDGET_Q3));
            assert!(
                coarse.quad_count + coarse.tri_count < fine.quad_count + fine.tri_count,
                "coarse {} vs fine {}",
                coarse.quad_count + coarse.tri_count,
                fine.quad_count + fine.tri_count
            );
            let strip = [
                [-50, -200, 100],
                [50, -200, 100],
                [50, 200, 100],
                [-50, 200, 100],
            ];
            let w = submit_error_bounded_face(&strip, unsealed(QUAKE_COARSE_ERROR_BUDGET_Q3));
            assert_within_gpu_extent(&w);
        }

        #[test]
        fn error_bounded_gate_skips_only_faces_beyond_it() {
            // The same floor pushed out beyond the gate keeps the historical
            // unsplit fan (two roots); in front of it the error bound splits it.
            let near = [
                [-200, 100, 150],
                [200, 100, 150],
                [200, 100, 1500],
                [-200, 100, 1500],
            ];
            let far = near.map(|[x, y, z]| [x, y, z + 1000]);
            let gated = ClassicAffineProfile {
                error_gate_depth: 1000,
                ..ERROR_BOUNDED
            };
            assert!(submit_error_bounded_face(&near, gated).quad_count > 1);
            let w = submit_error_bounded_face(&far, gated);
            assert!(w.quad_count + w.tri_count <= 2, "unsplit fan");
        }

        #[test]
        fn banded_quad_lattice_splits_only_receding_axes() {
            // Band mode: a floor inside the two-level band splits only along
            // its receding axis (4 cells); a wall facing the camera at the same
            // depth has no receding axis and keeps the historical pairing.
            let banded = ClassicAffineProfile {
                subdivide_once_at: u16::MAX,
                subdivide_twice_at: u16::MAX,
                quad_lattice: true,
                ..ClassicAffineProfile::QUAKE_REFERENCE
            };
            let floor = [
                [-200, 100, 150],
                [200, 100, 150],
                [200, 100, 600],
                [-200, 100, 600],
            ];
            let w = submit_error_bounded_face(&floor, banded);
            // Bands this deep also gate sealing (depth >= band), so none here.
            assert_eq!((w.quad_count, w.tri_count), (4, 0), "a 1x4 strip");
            let wall = [
                [-200, -100, 300],
                [200, -100, 300],
                [200, 100, 300],
                [-200, 100, 300],
            ];
            let w = submit_error_bounded_face(&wall, banded);
            assert_eq!((w.quad_count, w.tri_count), (1, 0));
        }
    }

    #[test]
    fn packet_writer_derives_hardware_triangles_from_stream_length() {
        let tri_words = size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>();
        let quad_words = size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>();
        let mut storage = [0u32; 128];
        let output = storage.as_mut_ptr();

        for (triangles, quads) in [(0usize, 0usize), (1, 0), (0, 1), (2, 3)] {
            let packets = triangles + quads;
            let words = triangles * tri_words + quads * quad_words;
            let writer = PacketWriter {
                next: unsafe { output.add(words) },
                packets: packets as u32,
                clut_high_word: 0,
                tpage_high_word: 0,
                profile: ClassicAffineProfile::QUAKE_REFERENCE,
            };
            let submit = unsafe { writer.finish(output) };
            assert_eq!(submit.next_packet, unsafe { output.add(words) });
            assert_eq!(submit.packets, packets as u32);
            assert_eq!(submit.hardware_triangles, (triangles + quads * 2) as u32);
        }
    }

    #[test]
    fn underlay_keys_at_the_far_vertex_behind_its_pieces() {
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        // A deep face: the far vertex sorts well behind average + bias.
        assert_eq!(underlay_otz(175, 1000, profile), 250);
        // A shallow face: never nearer than the historical average + bias.
        assert_eq!(underlay_otz(175, 704, profile), 183);
        // Clamped to the table.
        assert_eq!(underlay_otz(2040, 9000, profile), profile.ot_depth - 1);
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

    /// Quake's split kernel against the generic batch it replaces, on
    /// random batches that reach every path: rejected and partly visible
    /// surfaces, unsplit and paired fans, one- and two-level splits, quad
    /// lattices, and roots outside the OT.
    #[cfg(all(
        feature = "classic-affine-quake-specialized-kernel",
        feature = "classic-affine-lattice"
    ))]
    #[test]
    fn quake_kernel_matches_the_generic_batch_packet_for_packet() {
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::new(0, 0, 0));
        let mut seed = 0x2545_f491u32;
        let mut random = move |bound: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % bound
        };
        const VERTICES: usize = 39;
        const WORDS: usize = 16 * 1024;
        let mut packets = 0u32;
        for batch in 0..3000u32 {
            let budget_q3 = [128u8, 255, 0, 1][batch as usize % 4];
            let mut vertices = [ClassicAffineVertex::default(); VERTICES + EXTRA_VERTICES];
            let mut surfaces = [ClassicAffineBatchSurface::default(); 13];
            let mut vertex_count = 0usize;
            let mut surface_count = 0usize;
            while surface_count < surfaces.len() {
                let count = 3 + random(6) as usize;
                if vertex_count + count > VERTICES {
                    break;
                }
                // A plane-ish face: one depth and a random slope, near or
                // far, sometimes reaching behind the eye.
                let depth: i32 = [24, 60, 140, 400, 1200, 3000][random(6) as usize];
                let slope = random(5) as i32 * depth / 3;
                let (cx, cy) = (random(900) as i32 - 450, random(700) as i32 - 350);
                let span = 20 + random(600) as i32;
                for corner in 0..count {
                    let (x, y) = (
                        cx + random(span as u32) as i32 - span / 2,
                        cy + random(span as u32) as i32 - span / 2,
                    );
                    let z = depth + slope * (x - cx) / span.max(1) - random(8) as i32;
                    vertices[vertex_count + corner] = ClassicAffineVertex {
                        position: [
                            x.clamp(-32000, 32000) as i16,
                            y.clamp(-32000, 32000) as i16,
                            z.clamp(-200, 32000) as i16,
                        ],
                        uv: [random(256) as u8, random(256) as u8],
                        color: random(0x0100_0000),
                        screen: [0; 2],
                        depth: 0,
                    };
                }
                surfaces[surface_count] = ClassicAffineBatchSurface {
                    first_vertex: vertex_count as u16,
                    vertex_count: count as u16,
                    tpage: random(0x1_0000) as u16,
                    clut: random(0x1_0000) as u16,
                };
                vertex_count += count;
                surface_count += 1;
            }
            let mut generic_vertices = vertices;
            let mut quake_output = [0u32; WORDS];
            let mut generic_output = [0u32; WORDS];
            let (quake, generic) = unsafe {
                (
                    submit_quake_classic_affine_batch_budget(
                        vertices.as_mut_ptr(),
                        vertex_count,
                        surfaces.as_ptr(),
                        surface_count,
                        quake_output.as_mut_ptr(),
                        budget_q3,
                    ),
                    submit_classic_affine_batch(
                        generic_vertices.as_mut_ptr(),
                        vertex_count,
                        surfaces.as_ptr(),
                        surface_count,
                        generic_output.as_mut_ptr(),
                        quake_error_bounded_profile(budget_q3),
                    ),
                )
            };
            let words = unsafe { generic.next_packet.offset_from(generic_output.as_ptr()) };
            assert_eq!(
                unsafe { quake.next_packet.offset_from(quake_output.as_ptr()) },
                words,
                "batch {batch}"
            );
            assert_eq!(
                (quake.packets, quake.hardware_triangles),
                (generic.packets, generic.hardware_triangles),
                "batch {batch}"
            );
            assert_eq!(
                quake_output[..words as usize],
                generic_output[..words as usize],
                "batch {batch}"
            );
            packets += generic.packets;
        }
        assert!(
            packets > 100_000,
            "{packets} packets: the batches barely draw"
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
