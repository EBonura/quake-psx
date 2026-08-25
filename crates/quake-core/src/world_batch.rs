//! Game-side classic-affine world batch writer.
//!
//! Emits exactly the packet stream that
//! `psx_engine::classic_affine::submit_classic_affine_projected_batch` emits for
//! a profile whose affine-error triggers are off (`QUAKE_REFERENCE`): the same
//! surface clip rejection, OT slot per triangle, depth-only subdivision
//! schedule, quad pairing, two-level lattice with its crack-sealing underdraw,
//! and the same packet words in the same order. It differs only in how the
//! packets are produced: every packet is written in place from the vertex
//! records (the uv bytes and the projected screen pair already have the packet
//! layout, so each word is one load and one store) instead of being assembled
//! as a struct and copied, and the fan runs in a single pass.
//!
//! `submit_projected_batch_matches_sdk` proves the equivalence on the host over
//! thousands of random batches, byte for byte, including the returned counts.

use core::ptr;

use psx_engine::classic_affine::{
    submit_classic_affine_projected_batch, ClassicAffineBatchSurface, ClassicAffineProfile,
    ClassicAffineSubmit, ClassicAffineVertex,
};
use psx_engine::projection::{classic_quad_screen_rejected, classic_triangle_screen_rejected};
use psx_gte::math::Vec3I16;
use psx_gte::scene::{self, classic_otz3_from_sum};

/// Words in a compact GP0(34h) textured Gouraud triangle packet (tag + 9).
const TRI_WORDS: usize = 10;
/// Words in a compact GP0(3Ch) textured Gouraud quad packet (tag + 12).
const QUAD_WORDS: usize = 13;
const TRI_TAG: u32 = 9 << 24;
const QUAD_TAG: u32 = 12 << 24;
const TRI_COMMAND: u32 = 0x3400_0000;
const QUAD_COMMAND: u32 = 0x3c00_0000;
/// Scratch records the two-level lattice needs after the batch's vertices.
pub const SUBDIVISION_SCRATCH: usize = 12;

/// Submit several contiguous convex fans whose vertices already carry screen
/// coordinates and cached GTE depths, exactly like
/// `submit_classic_affine_projected_batch`.
///
/// # Safety
/// Same contract as the SDK function: `vertices` points to
/// `vertex_count + SUBDIVISION_SCRATCH` writable records, every surface range
/// fits in the first `vertex_count` records, every record's `screen`/`depth`
/// were produced for the current camera (which is still loaded in the GTE, the
/// lattice reprojects midpoints), and `output` has room for every fan's
/// worst-case expansion.
pub unsafe fn submit_projected_batch(
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
    // The error triggers make the level depend on uv and depth deltas; this
    // writer only reproduces the depth-only schedule.
    if profile.subdivide_once_error_texels != 0 || profile.subdivide_twice_error_texels != 0 {
        return unsafe {
            submit_classic_affine_projected_batch(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                profile,
            )
        };
    }

    let scratch = unsafe { vertices.add(vertex_count) };
    let mut writer = Writer {
        next: output,
        packets: 0,
        clut_high: 0,
        tpage_high: 0,
        profile,
    };
    let mut surface_index = 0usize;
    while surface_index < surface_count {
        let surface = unsafe { ptr::read(surfaces.add(surface_index)) };
        surface_index += 1;
        let count = surface.vertex_count as usize;
        let fan = unsafe { vertices.add(surface.first_vertex as usize) };
        debug_assert!(count >= 3);
        debug_assert!(surface.first_vertex as usize + count <= vertex_count);

        // Whole-fan rejection: every vertex outside on one common side.
        let mut clip = 0x0fu8;
        let mut index = 0usize;
        while index < count && clip != 0 {
            clip &= clip_code(unsafe { (*fan.add(index)).screen }, profile);
            index += 1;
        }
        if clip != 0 {
            continue;
        }

        writer.clut_high = (surface.clut as u32) << 16;
        writer.tpage_high = (surface.tpage as u32) << 16;
        let root = fan;
        let root_depth = (unsafe { (*root).depth }) as u16 as u32;
        let end = unsafe { fan.add(count) };
        let mut previous = unsafe { fan.add(1) };
        let mut current = unsafe { fan.add(2) };
        while current != end {
            let otz = classic_otz3_from_sum(root_depth + depth(previous) + depth(current));
            if otz > 0 && otz < profile.ot_depth {
                let next = unsafe { current.add(1) };
                if otz >= profile.subdivide_once_at {
                    // Level zero. Two adjacent level-zero fan triangles at one
                    // OT slot become one GP0(3Ch) quad ordered (previous,
                    // current, root, next) so the hardware split lands on the
                    // fan's shared diagonal. Equal slots imply equal
                    // (depth-only) levels, so no second level test is needed.
                    if next != end {
                        let next_otz =
                            classic_otz3_from_sum(root_depth + depth(current) + depth(next));
                        if next_otz == otz {
                            unsafe { writer.quad([previous, current, root, next], otz) };
                            previous = next;
                            current = unsafe { next.add(1) };
                            continue;
                        }
                    }
                    unsafe { writer.tri([root, previous, current], otz) };
                } else if otz >= profile.subdivide_twice_at {
                    unsafe { subdivide_once(&mut writer, root, previous, current, scratch, otz) };
                } else {
                    unsafe { subdivide_twice(&mut writer, root, previous, current, scratch, otz) };
                }
            }
            previous = current;
            current = unsafe { current.add(1) };
        }
    }
    writer.finish(output)
}

struct Writer {
    next: *mut u32,
    packets: u32,
    clut_high: u32,
    tpage_high: u32,
    profile: ClassicAffineProfile,
}

impl Writer {
    /// GP0(34h) triangle whose screen words and attributes come from the same
    /// records, as `ClassicTriTexturedGouraud` lays it out.
    #[inline(always)]
    unsafe fn tri(&mut self, v: [*const ClassicAffineVertex; 3], otz: u16) {
        unsafe { self.tri_split(v, v, otz) };
    }

    /// Triangle with screen words from `p` and uv/colour from `a` (the
    /// lattice underdraw uses one such asymmetric packet).
    ///
    /// All ten words are loaded into locals before the first store: the
    /// output arena is a raw `*mut u32` the compiler cannot prove distinct
    /// from the vertex records, so interleaving would serialize every
    /// load/store pair behind a MIPS load-delay stall.
    #[inline(always)]
    unsafe fn tri_split(
        &mut self,
        p: [*const ClassicAffineVertex; 3],
        a: [*const ClassicAffineVertex; 3],
        otz: u16,
    ) {
        if classic_triangle_screen_rejected(
            unsafe { [(*p[0]).screen, (*p[1]).screen, (*p[2]).screen] },
            self.profile.screen_width as i32 - 1,
            self.profile.screen_height as i32 - 1,
        ) {
            return;
        }
        let w0 = TRI_TAG | u32::from(otz);
        let w1 = TRI_COMMAND | unsafe { color(a[0]) };
        let w2 = unsafe { screen_word(p[0]) };
        let w3 = unsafe { uv_word(a[0]) } | self.clut_high;
        let w4 = unsafe { color(a[1]) };
        let w5 = unsafe { screen_word(p[1]) };
        let w6 = unsafe { uv_word(a[1]) } | self.tpage_high;
        let w7 = unsafe { color(a[2]) };
        let w8 = unsafe { screen_word(p[2]) };
        let w9 = unsafe { uv_word(a[2]) };
        let out = self.next;
        unsafe {
            ptr::write(out, w0);
            ptr::write(out.add(1), w1);
            ptr::write(out.add(2), w2);
            ptr::write(out.add(3), w3);
            ptr::write(out.add(4), w4);
            ptr::write(out.add(5), w5);
            ptr::write(out.add(6), w6);
            ptr::write(out.add(7), w7);
            ptr::write(out.add(8), w8);
            ptr::write(out.add(9), w9);
        }
        self.next = unsafe { out.add(TRI_WORDS) };
        self.packets = self.packets.wrapping_add(1);
    }

    /// GP0(3Ch) quad as `ClassicQuadTexturedGouraud` lays it out. Loads
    /// complete before the first store for the same reason as `tri_split`.
    #[inline(always)]
    unsafe fn quad(&mut self, v: [*const ClassicAffineVertex; 4], otz: u16) {
        if classic_quad_screen_rejected(
            unsafe {
                [
                    (*v[0]).screen,
                    (*v[1]).screen,
                    (*v[2]).screen,
                    (*v[3]).screen,
                ]
            },
            self.profile.screen_width as i32 - 1,
            self.profile.screen_height as i32 - 1,
        ) {
            return;
        }
        let w0 = QUAD_TAG | u32::from(otz);
        let w1 = QUAD_COMMAND | unsafe { color(v[0]) };
        let w2 = unsafe { screen_word(v[0]) };
        let w3 = unsafe { uv_word(v[0]) } | self.clut_high;
        let w4 = unsafe { color(v[1]) };
        let w5 = unsafe { screen_word(v[1]) };
        let w6 = unsafe { uv_word(v[1]) } | self.tpage_high;
        let w7 = unsafe { color(v[2]) };
        let w8 = unsafe { screen_word(v[2]) };
        let w9 = unsafe { uv_word(v[2]) };
        let w10 = unsafe { color(v[3]) };
        let w11 = unsafe { screen_word(v[3]) };
        let w12 = unsafe { uv_word(v[3]) };
        let out = self.next;
        unsafe {
            ptr::write(out, w0);
            ptr::write(out.add(1), w1);
            ptr::write(out.add(2), w2);
            ptr::write(out.add(3), w3);
            ptr::write(out.add(4), w4);
            ptr::write(out.add(5), w5);
            ptr::write(out.add(6), w6);
            ptr::write(out.add(7), w7);
            ptr::write(out.add(8), w8);
            ptr::write(out.add(9), w9);
            ptr::write(out.add(10), w10);
            ptr::write(out.add(11), w11);
            ptr::write(out.add(12), w12);
        }
        self.next = unsafe { out.add(QUAD_WORDS) };
        self.packets = self.packets.wrapping_add(1);
    }

    /// Lattice triangle: own OT slot from its three depths, drawn when nonzero.
    #[inline(always)]
    unsafe fn sorted_tri(&mut self, v: [*const ClassicAffineVertex; 3]) {
        let otz = classic_otz3_from_sum(depth(v[0]) + depth(v[1]) + depth(v[2]));
        if otz > 0 {
            unsafe { self.tri(v, otz) };
        }
    }

    /// Lattice quad: OT slot is the four-depth average, drawn when nonzero.
    #[inline(always)]
    unsafe fn sorted_quad(&mut self, v: [*const ClassicAffineVertex; 4]) {
        let otz = ((depth(v[0]) + depth(v[1]) + depth(v[2]) + depth(v[3])) >> 4) as u16;
        if otz > 0 {
            unsafe { self.quad(v, otz) };
        }
    }

    #[inline(always)]
    fn finish(self, output: *mut u32) -> ClassicAffineSubmit {
        let words = unsafe { self.next.offset_from(output) } as u32;
        let quads = words.wrapping_sub(self.packets.wrapping_mul(TRI_WORDS as u32))
            / (QUAD_WORDS as u32 - TRI_WORDS as u32);
        ClassicAffineSubmit {
            next_packet: self.next,
            packets: self.packets,
            hardware_triangles: self.packets.wrapping_add(quads),
        }
    }
}

/// One-level lattice: three midpoints, one centre quad, two corner
/// triangles, plus root-edge underdraw when the triangle spans the boundary.
#[inline(always)]
unsafe fn subdivide_once(
    writer: &mut Writer,
    root0: *const ClassicAffineVertex,
    root1: *const ClassicAffineVertex,
    root2: *const ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root2, root0));
        project_three(scratch);
    }
    let h01 = scratch as *const ClassicAffineVertex;
    let h12 = unsafe { scratch.add(1) } as *const ClassicAffineVertex;
    let h20 = unsafe { scratch.add(2) } as *const ClassicAffineVertex;
    unsafe {
        writer.sorted_quad([root0, h01, h20, h12]);
        writer.sorted_tri([h01, root1, h12]);
        writer.sorted_tri([h12, root2, h20]);
    }
    let underdraw_at = i32::from(writer.profile.subdivide_once_at);
    if unsafe { (*root0).depth } >= underdraw_at
        || unsafe { (*root1).depth } >= underdraw_at
        || unsafe { (*root2).depth } >= underdraw_at
    {
        let underdraw = root_otz.saturating_add(writer.profile.underdraw_slot_bias);
        unsafe {
            writer.tri([root0, root1, h01], underdraw);
            // The SDK samples this seam's uv and colour from root0/root1
            // while placing it on root1/root2; reproduced as shipped.
            writer.tri_split([root1, root2, h12], [root0, root1, h12], underdraw);
            writer.tri([root2, root0, h20], underdraw);
        }
    }
}

/// Two-level lattice: twelve generated vertices, four triangles, six quads,
/// plus root-edge underdraw as quad/triangle pairs.
#[inline(always)]
unsafe fn subdivide_twice(
    writer: &mut Writer,
    root0: *const ClassicAffineVertex,
    root1: *const ClassicAffineVertex,
    root2: *const ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root0, root2));
        ptr::write(scratch.add(3), midpoint(root0, scratch));
        ptr::write(scratch.add(4), midpoint(root1, scratch));
        ptr::write(scratch.add(5), midpoint(root1, scratch.add(1)));
        ptr::write(scratch.add(6), midpoint(scratch.add(1), root2));
        ptr::write(scratch.add(7), midpoint(scratch.add(2), root2));
        ptr::write(scratch.add(8), midpoint(scratch.add(2), root0));
        ptr::write(scratch.add(9), midpoint(scratch.add(2), scratch));
        ptr::write(scratch.add(10), midpoint(scratch.add(9), scratch.add(5)));
        ptr::write(scratch.add(11), midpoint(scratch.add(2), scratch.add(1)));
        project_three(scratch);
        project_three(scratch.add(3));
        project_three(scratch.add(6));
        project_three(scratch.add(9));
    }
    let v = |index: usize| unsafe { scratch.add(index) } as *const ClassicAffineVertex;
    unsafe {
        writer.sorted_tri([root0, v(3), v(8)]);
        writer.sorted_tri([v(8), v(9), v(2)]);
        writer.sorted_tri([v(2), v(11), v(7)]);
        writer.sorted_tri([v(7), v(6), root2]);
        writer.sorted_quad([v(3), v(0), v(8), v(9)]);
        writer.sorted_quad([v(0), v(4), v(9), v(10)]);
        writer.sorted_quad([v(4), root1, v(10), v(5)]);
        writer.sorted_quad([v(9), v(10), v(2), v(11)]);
        writer.sorted_quad([v(10), v(5), v(11), v(1)]);
        writer.sorted_quad([v(11), v(1), v(7), v(6)]);
    }
    let underdraw_at = i32::from(writer.profile.subdivide_twice_at);
    if unsafe { (*root0).depth } >= underdraw_at
        || unsafe { (*root1).depth } >= underdraw_at
        || unsafe { (*root2).depth } >= underdraw_at
    {
        let underdraw = root_otz.saturating_add(writer.profile.underdraw_slot_bias);
        unsafe {
            writer.quad([root1, v(0), root0, v(3)], underdraw);
            writer.tri([v(0), root1, v(4)], underdraw);
            writer.quad([root2, v(1), root1, v(5)], underdraw);
            writer.tri([v(1), root2, v(6)], underdraw);
            writer.quad([root2, v(2), root0, v(8)], underdraw);
            writer.tri([v(2), root2, v(7)], underdraw);
        }
    }
}

/// Camera-space midpoint with the SDK's rounding: positions halve
/// arithmetically, the packed uv bytes average independently, and the light
/// is the halved red byte replicated to all three channels.
#[inline(always)]
unsafe fn midpoint(
    a: *const ClassicAffineVertex,
    b: *const ClassicAffineVertex,
) -> ClassicAffineVertex {
    let (a, b) = unsafe { (&*a, &*b) };
    let light = ((a.color as u8 as u16 + b.color as u8 as u16) >> 1) as u32;
    let a_uv = u16::from_le_bytes(a.uv);
    let b_uv = u16::from_le_bytes(b.uv);
    let uv = (a_uv & b_uv).wrapping_add(((a_uv ^ b_uv) & 0xfefe) >> 1);
    ClassicAffineVertex {
        position: [
            ((a.position[0] as i32 + b.position[0] as i32) >> 1) as i16,
            ((a.position[1] as i32 + b.position[1] as i32) >> 1) as i16,
            ((a.position[2] as i32 + b.position[2] as i32) >> 1) as i16,
        ],
        uv: uv.to_le_bytes(),
        color: light | (light << 8) | (light << 16),
        screen: [0; 2],
        depth: 0,
    }
}

/// Project three consecutive records with one RTPT, storing screen and depth.
#[inline(always)]
unsafe fn project_three(records: *mut ClassicAffineVertex) {
    let position = |index: usize| {
        let p = unsafe { (*records.add(index)).position };
        Vec3I16::new(p[0], p[1], p[2])
    };
    let out = scene::project_triangle_scheduled(position(0), position(1), position(2));
    let mut index = 0usize;
    while index < 3 {
        unsafe {
            let record = records.add(index);
            ptr::write(
                ptr::addr_of_mut!((*record).screen),
                [out[index].sx, out[index].sy],
            );
            ptr::write(ptr::addr_of_mut!((*record).depth), out[index].sz as i32);
        }
        index += 1;
    }
}

/// Outcode of one projected vertex against the viewport, matching the SDK's
/// `classic_clip_code`: bit 0 left, bit 1 right, bit 2 top, bit 3 bottom.
#[inline(always)]
fn clip_code(screen: [i16; 2], profile: ClassicAffineProfile) -> u8 {
    let x = screen[0] as i32;
    let y = screen[1] as i32;
    let right = profile.screen_width as i32 - 1;
    let bottom = profile.screen_height as i32 - 1;
    if (x as u32) <= right as u32 && (y as u32) <= bottom as u32 {
        return 0;
    }
    ((x as u32 >> 31) as u8)
        | ((((right - x) as u32 >> 31) as u8) << 1)
        | (((y as u32 >> 31) as u8) << 2)
        | ((((bottom - y) as u32 >> 31) as u8) << 3)
}

/// The vertex's packed screen coordinate: `[i16; 2]` little-endian is the
/// GP0 vertex word, so this is one aligned word load.
#[inline(always)]
unsafe fn screen_word(vertex: *const ClassicAffineVertex) -> u32 {
    unsafe { ptr::read(ptr::addr_of!((*vertex).screen).cast::<u32>()) }
}

/// The vertex's uv bytes as the low half of a packet word.
#[inline(always)]
unsafe fn uv_word(vertex: *const ClassicAffineVertex) -> u32 {
    u32::from(unsafe { ptr::read(ptr::addr_of!((*vertex).uv).cast::<u16>()) })
}

#[inline(always)]
unsafe fn color(vertex: *const ClassicAffineVertex) -> u32 {
    unsafe { (*vertex).color }
}

#[inline(always)]
unsafe fn depth(vertex: *const ClassicAffineVertex) -> u32 {
    (unsafe { (*vertex).depth }) as u16 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_BATCH: usize = 39;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }

        fn below(&mut self, bound: u32) -> u32 {
            self.next() % bound
        }
    }

    fn random_batch(
        rng: &mut Lcg,
    ) -> (
        [ClassicAffineVertex; MAX_BATCH + SUBDIVISION_SCRATCH],
        [ClassicAffineBatchSurface; 13],
        usize,
        usize,
    ) {
        let mut vertices = [ClassicAffineVertex::default(); MAX_BATCH + SUBDIVISION_SCRATCH];
        let mut surfaces = [ClassicAffineBatchSurface::default(); 13];
        let mut vertex_count = 0usize;
        let mut surface_count = 0usize;
        // Depth regime per batch so all schedules occur often: near (level 2),
        // mid (level 1), far (level 0), and beyond the OT (skipped).
        let regime = rng.below(4);
        while surface_count < 13 {
            let count = 3 + rng.below(6) as usize;
            if vertex_count + count > MAX_BATCH {
                break;
            }
            // Occasionally push a whole fan off one screen side.
            let offside = rng.below(8) == 0;
            let side = rng.below(4);
            for i in 0..count {
                let vertex = &mut vertices[vertex_count + i];
                let (mut x, mut y) = (rng.below(400) as i32 - 40, rng.below(300) as i32 - 30);
                if offside {
                    match side {
                        0 => x = -1 - rng.below(50) as i32,
                        1 => x = 319 + rng.below(50) as i32,
                        2 => y = -1 - rng.below(50) as i32,
                        _ => y = 239 + rng.below(50) as i32,
                    }
                }
                let depth = match regime {
                    0 => rng.below(900),
                    1 => 500 + rng.below(1400),
                    2 => 1500 + rng.below(6000),
                    _ => 6000 + rng.below(30_000),
                } as i32;
                vertex.screen = [x as i16, y as i16];
                vertex.depth = depth;
                vertex.uv = [rng.next() as u8, rng.next() as u8];
                vertex.color = rng.next() & 0x00ff_ffff;
                vertex.position = [
                    (rng.below(2000) as i32 - 1000) as i16,
                    (rng.below(2000) as i32 - 1000) as i16,
                    (rng.below(3000) as i32 + 16) as i16,
                ];
            }
            surfaces[surface_count] = ClassicAffineBatchSurface {
                first_vertex: vertex_count as u16,
                vertex_count: count as u16,
                tpage: rng.next() as u16,
                clut: rng.next() as u16,
            };
            vertex_count += count;
            surface_count += 1;
        }
        (vertices, surfaces, vertex_count, surface_count)
    }

    #[test]
    fn submit_projected_batch_matches_sdk() {
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        let mut rng = Lcg(0x5eed_1234_abcd_ef01);
        let mut sdk_out = [0u32; 32 * 1024];
        let mut own_out = [0u32; 32 * 1024];
        let mut quads = 0u32;
        let mut expanded = 0u32;
        let mut total_packets = 0u32;
        for _ in 0..3000 {
            let (vertices, surfaces, vertex_count, surface_count) = random_batch(&mut rng);
            if surface_count == 0 {
                continue;
            }
            let mut sdk_vertices = vertices;
            let mut own_vertices = vertices;
            sdk_out.fill(0xdead_beef);
            own_out.fill(0xdead_beef);
            let sdk = unsafe {
                submit_classic_affine_projected_batch(
                    sdk_vertices.as_mut_ptr(),
                    vertex_count,
                    surfaces.as_ptr(),
                    surface_count,
                    sdk_out.as_mut_ptr(),
                    profile,
                )
            };
            let own = unsafe {
                submit_projected_batch(
                    own_vertices.as_mut_ptr(),
                    vertex_count,
                    surfaces.as_ptr(),
                    surface_count,
                    own_out.as_mut_ptr(),
                    profile,
                )
            };
            let sdk_words = unsafe { sdk.next_packet.offset_from(sdk_out.as_ptr()) } as usize;
            let own_words = unsafe { own.next_packet.offset_from(own_out.as_ptr()) } as usize;
            assert_eq!(sdk_words, own_words, "stream length");
            assert_eq!(sdk.packets, own.packets, "packet count");
            assert_eq!(
                sdk.hardware_triangles, own.hardware_triangles,
                "triangle count"
            );
            assert_eq!(&sdk_out[..sdk_words], &own_out[..own_words], "packet words");
            // The lattice scratch must end up identical too (the SDK reprojects
            // midpoints into it; a caller could read it back).
            assert_eq!(
                &sdk_vertices[vertex_count..],
                &own_vertices[vertex_count..],
                "scratch records"
            );
            total_packets += own.packets;
            quads += own.hardware_triangles - own.packets;
            let fan_triangles: u32 = (0..surface_count)
                .map(|s| u32::from(surfaces[s].vertex_count) - 2)
                .sum();
            if own.packets > fan_triangles {
                expanded += 1;
            }
        }
        assert!(total_packets > 10_000, "{total_packets}");
        assert!(quads > 100, "no quad pairing exercised: {quads}");
        assert!(expanded > 100, "no subdivided fans exercised: {expanded}");
    }

    #[test]
    fn packet_word_layout_matches_prims() {
        // Pin the constants against the SDK primitive types this writer mirrors.
        use psx_gpu::prim::{ClassicQuadTexturedGouraud, ClassicTriTexturedGouraud};
        assert_eq!(
            core::mem::size_of::<ClassicTriTexturedGouraud>(),
            TRI_WORDS * 4
        );
        assert_eq!(
            core::mem::size_of::<ClassicQuadTexturedGouraud>(),
            QUAD_WORDS * 4
        );
        assert_eq!(u32::from(ClassicTriTexturedGouraud::WORDS) << 24, TRI_TAG);
        assert_eq!(u32::from(ClassicQuadTexturedGouraud::WORDS) << 24, QUAD_TAG);
        assert_eq!(core::mem::size_of::<ClassicAffineVertex>(), 20);
        assert_eq!(core::mem::align_of::<ClassicAffineVertex>(), 4);
    }
}
