//! Quake-space facade over PSoXide's canonical BSP hull tracer.
//!
//! Quake map planes remain encoded in their original Z-up local axes. Public
//! queries and results also remain Quake Z-up. The facade applies the exact
//! PSoXide Y-up boundary around the shared caller-owned tracer and contains no
//! BSP traversal implementation of its own.

use psx_bsp::collision::{
    CollisionHull as SharedCollisionHull, Trace as SharedTrace,
    TransformedCollisionHull as SharedTransformedCollisionHull,
};
use psx_bsp::Vec3I32 as SharedVec3I32;
use psx_engine::div_q12_i32;
use psx_math::int32::mul_q12_i32;
use quake_formats::{ClipNode, Leaf, Node, Plane, RecordSlice, Vec3I16, Vec3I32};

use crate::bsp_axis_adapter::{
    psoxide_normal_to_quake, psoxide_point_to_quake, quake_point_to_psoxide,
    quake_raw_hull_transform_to_psoxide, RotationQ12, IDENTITY_ROTATION_Q12,
};

pub use psx_bsp::collision::{
    TraceScratch, CONTENTS_EMPTY, CONTENTS_LAVA, CONTENTS_SKY, CONTENTS_SLIME, CONTENTS_SOLID,
    CONTENTS_WATER, Q12_ONE, TRACE_PLANE_EPSILON_Q12, TRACE_STACK_CAPACITY,
};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct Trace {
    pub all_solid: bool,
    pub start_solid: bool,
    pub in_open: bool,
    pub in_water: bool,
    pub fraction: i32,
    pub end: Vec3I32,
    pub normal: Vec3I16,
    pub plane_distance: i32,
}

#[derive(Copy, Clone)]
struct RenderTraceContinuation {
    far_child: i16,
    plane_index: u16,
    side: u8,
    middle_fraction: i32,
    end_fraction: i32,
    middle: Vec3I32,
    end: Vec3I32,
}

impl RenderTraceContinuation {
    const EMPTY: Self = Self {
        far_child: 0,
        plane_index: 0,
        side: 0,
        middle_fraction: 0,
        end_fraction: 0,
        middle: Vec3I32 { x: 0, y: 0, z: 0 },
        end: Vec3I32 { x: 0, y: 0, z: 0 },
    };
}

/// Caller-owned fixed workspace for tracing a point through the render BSP.
///
/// Quake model head zero indexes the render-node lump, not the clipnode lump.
/// Weapon and sight point traces therefore use this tree and its leaf
/// contents; body movement continues to use the canonical clipnode tracer.
pub struct RenderTraceScratch {
    continuations: [RenderTraceContinuation; TRACE_STACK_CAPACITY],
}

impl RenderTraceScratch {
    pub const fn new() -> Self {
        Self {
            continuations: [RenderTraceContinuation::EMPTY; TRACE_STACK_CAPACITY],
        }
    }
}

impl Default for RenderTraceScratch {
    /// Out of line and size-optimised: the sixty-four-slot array literal is
    /// lowered as one `memset` call per slot, and inline that is a kilobyte
    /// of call setup in every caller.
    #[inline(never)]
    fn default() -> Self {
        Self::new()
    }
}

/// Trace a Quake-space point segment through a checked render BSP.
///
/// This is integer-only and allocation-free. Malformed nodes, planes, leaves,
/// cycles, or a traversal deeper than [`TRACE_STACK_CAPACITY`] return `false`
/// and leave `output` unchanged.
pub fn trace_render_bsp_into(
    planes: RecordSlice<'_, Plane>,
    nodes: RecordSlice<'_, Node>,
    leaves: RecordSlice<'_, Leaf>,
    head_node: i16,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut RenderTraceScratch,
    output: &mut Trace,
) -> bool {
    let mut trace = Trace {
        all_solid: true,
        start_solid: false,
        in_open: false,
        in_water: false,
        fraction: Q12_ONE,
        end: *end,
        normal: Vec3I16 { x: 0, y: 0, z: 0 },
        plane_distance: 0,
    };
    let mut continuation_count = 0usize;
    let mut node_index = head_node;
    let mut start_fraction: i32 = 0;
    let mut end_fraction = Q12_ONE;
    let mut segment_start = *start;
    let mut segment_end = *end;

    loop {
        let mut descent_budget = nodes.len();
        while node_index >= 0 {
            if descent_budget == 0 {
                return false;
            }
            descent_budget -= 1;
            let Some(node) = nodes.get(node_index as usize) else {
                return false;
            };
            let Some(plane) = planes.get(node.plane as usize) else {
                return false;
            };
            let start_distance = render_plane_distance(plane, segment_start);
            let end_distance = render_plane_distance(plane, segment_end);
            if start_distance >= 0 && end_distance >= 0 {
                node_index = node.children[0];
                continue;
            }
            if start_distance < 0 && end_distance < 0 {
                node_index = node.children[1];
                continue;
            }
            let numerator = if start_distance < 0 {
                start_distance.saturating_add(TRACE_PLANE_EPSILON_Q12)
            } else {
                start_distance.saturating_sub(TRACE_PLANE_EPSILON_Q12)
            };
            let fraction = div_q12_i32(numerator, start_distance.saturating_sub(end_distance))
                .clamp(0, Q12_ONE);
            let middle_fraction = start_fraction.saturating_add(mul_q12_i32(
                end_fraction.saturating_sub(start_fraction),
                fraction,
            ));
            let middle = render_interpolate(segment_start, segment_end, fraction);
            let side = usize::from(start_distance < 0);
            if continuation_count == TRACE_STACK_CAPACITY {
                return false;
            }
            scratch.continuations[continuation_count] = RenderTraceContinuation {
                far_child: node.children[side ^ 1],
                plane_index: node.plane,
                side: side as u8,
                middle_fraction,
                end_fraction,
                middle,
                end: segment_end,
            };
            continuation_count += 1;
            node_index = node.children[side];
            end_fraction = middle_fraction;
            segment_end = middle;
        }

        let Some(contents) = render_leaf_contents(leaves, node_index) else {
            return false;
        };
        if contents != CONTENTS_SOLID {
            trace.all_solid = false;
            if contents == CONTENTS_EMPTY {
                trace.in_open = true;
            } else {
                trace.in_water = true;
            }
        } else {
            trace.start_solid = true;
        }
        if continuation_count == 0 {
            *output = trace;
            return true;
        }
        continuation_count -= 1;
        let continuation = scratch.continuations[continuation_count];
        let Some(far_contents) = render_contents_from(
            planes,
            nodes,
            leaves,
            continuation.far_child,
            continuation.middle,
        ) else {
            return false;
        };
        if far_contents != CONTENTS_SOLID {
            node_index = continuation.far_child;
            start_fraction = continuation.middle_fraction;
            end_fraction = continuation.end_fraction;
            segment_start = continuation.middle;
            segment_end = continuation.end;
            continue;
        }
        if trace.all_solid {
            *output = trace;
            return true;
        }
        let Some(plane) = planes.get(continuation.plane_index as usize) else {
            return false;
        };
        if continuation.side == 0 {
            trace.normal = plane.normal;
            trace.plane_distance = plane.distance;
        } else {
            trace.normal = Vec3I16 {
                x: plane.normal.x.saturating_neg(),
                y: plane.normal.y.saturating_neg(),
                z: plane.normal.z.saturating_neg(),
            };
            trace.plane_distance = plane.distance.saturating_neg();
        }
        trace.fraction = continuation.middle_fraction;
        trace.end = continuation.middle;
        *output = trace;
        return true;
    }
}

/// Trace a world-space point segment through a translated brush model's render BSP.
///
/// Quake brush-model head zero indexes the render-node lump. Movers keep those
/// nodes in their authored local coordinates and publish a world-space origin,
/// so point traces must translate the query into model space. Interpreting head
/// zero as a clipnode index can make a closed door block bodies while allowing
/// sight and weapon rays through it.
#[allow(clippy::too_many_arguments)]
pub fn trace_translated_render_bsp_into(
    planes: RecordSlice<'_, Plane>,
    nodes: RecordSlice<'_, Node>,
    leaves: RecordSlice<'_, Leaf>,
    head_node: i16,
    origin: Vec3I32,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut RenderTraceScratch,
    output: &mut Trace,
) -> bool {
    let subtract_origin = |point: Vec3I32| Vec3I32 {
        x: point.x.saturating_sub(origin.x),
        y: point.y.saturating_sub(origin.y),
        z: point.z.saturating_sub(origin.z),
    };
    let local_start = subtract_origin(*start);
    let local_end = subtract_origin(*end);
    let mut local_trace = Trace::default();
    if !trace_render_bsp_into(
        planes,
        nodes,
        leaves,
        head_node,
        &local_start,
        &local_end,
        scratch,
        &mut local_trace,
    ) {
        return false;
    }
    local_trace.end = Vec3I32 {
        x: local_trace.end.x.saturating_add(origin.x),
        y: local_trace.end.y.saturating_add(origin.y),
        z: local_trace.end.z.saturating_add(origin.z),
    };
    let translated_plane = mul_q12_i32(origin.x, i32::from(local_trace.normal.x))
        .saturating_add(mul_q12_i32(origin.y, i32::from(local_trace.normal.y)))
        .saturating_add(mul_q12_i32(origin.z, i32::from(local_trace.normal.z)));
    local_trace.plane_distance = local_trace.plane_distance.saturating_add(translated_plane);
    *output = local_trace;
    true
}

fn render_contents_from(
    planes: RecordSlice<'_, Plane>,
    nodes: RecordSlice<'_, Node>,
    leaves: RecordSlice<'_, Leaf>,
    mut node_index: i16,
    point: Vec3I32,
) -> Option<i16> {
    let mut descent_budget = nodes.len();
    while node_index >= 0 {
        if descent_budget == 0 {
            return None;
        }
        descent_budget -= 1;
        let node = nodes.get(node_index as usize)?;
        let plane = planes.get(node.plane as usize)?;
        node_index = node.children[(render_plane_distance(plane, point) < 0) as usize];
    }
    render_leaf_contents(leaves, node_index)
}

fn render_leaf_contents(leaves: RecordSlice<'_, Leaf>, encoded: i16) -> Option<i16> {
    if encoded >= 0 {
        return None;
    }
    let index = (-1i32 - i32::from(encoded)) as usize;
    Some(leaves.get(index)?.contents)
}

fn render_plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32(point.x, plane.normal.x as i32)
            .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
            .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
    };
    dot.saturating_sub(plane.distance)
}

fn render_interpolate(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    Vec3I32 {
        x: start
            .x
            .saturating_add(mul_q12_i32(end.x.saturating_sub(start.x), fraction)),
        y: start
            .y
            .saturating_add(mul_q12_i32(end.y.saturating_sub(start.y), fraction)),
        z: start
            .z
            .saturating_add(mul_q12_i32(end.z.saturating_sub(start.z), fraction)),
    }
}

/// Quake-local rigid brush transform.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrushTransform {
    /// Q20.12 origin in Quake Z-up world space.
    pub origin: Vec3I32,
    /// Q3.12 Quake-local to Quake-world rotation, stored by rows.
    pub rotation: RotationQ12,
}

impl BrushTransform {
    pub const IDENTITY: Self = Self {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        rotation: IDENTITY_ROTATION_Q12,
    };

    pub const fn translated(origin: Vec3I32) -> Self {
        Self {
            origin,
            rotation: IDENTITY_ROTATION_Q12,
        }
    }
}

#[derive(Copy, Clone)]
pub struct CollisionHull<'a> {
    shared: SharedCollisionHull<'a>,
}

impl<'a> CollisionHull<'a> {
    /// Wrap canonical records while retaining Quake's Z-up query boundary.
    pub fn new(
        planes: RecordSlice<'a, Plane>,
        nodes: RecordSlice<'a, ClipNode>,
        head_node: i16,
    ) -> Option<Self> {
        Some(Self {
            shared: SharedCollisionHull::new(planes, nodes, head_node)?,
        })
    }

    /// Wrap records decoded once by the resident-map owner.
    pub const fn new_decoded(
        planes: &'a [Plane],
        nodes: &'a [ClipNode],
        head_node: i16,
    ) -> Option<Self> {
        Some(Self {
            shared: SharedCollisionHull::new_decoded(planes, nodes, head_node),
        })
    }

    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.shared.point_contents(SharedVec3I32 {
            x: point.x,
            y: point.y,
            z: point.z,
        })
    }

    /// Trace into caller-owned storage. Structural failure or scratch
    /// overflow leaves `output` unchanged.
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        // The retained Quake hull records and these public points are both
        // already Z-up. Going through `TransformedCollisionHull::IDENTITY`
        // would convert the query to PSoXide axes, multiply it by the inverse
        // identity boundary matrix, and rotate the result back. Trace the raw
        // records directly and only normalize the shared flag representation.
        let mut shared_output = SharedTrace::default();
        if !self.shared.trace_into(
            &SharedVec3I32 {
                x: start.x,
                y: start.y,
                z: start.z,
            },
            &SharedVec3I32 {
                x: end.x,
                y: end.y,
                z: end.z,
            },
            scratch,
            &mut shared_output,
        ) {
            return false;
        }
        *output = trace_from_shared_raw_axes(shared_output);
        true
    }

    pub fn transformed(self, transform: BrushTransform) -> TransformedCollisionHull<'a> {
        TransformedCollisionHull {
            shared: self.shared.transformed(quake_raw_hull_transform_to_psoxide(
                transform.origin,
                transform.rotation,
            )),
        }
    }
}

/// Normalize a shared trace whose retained hull and query were both expressed
/// directly in Quake axes.
fn trace_from_shared_raw_axes(trace: SharedTrace) -> Trace {
    Trace {
        all_solid: trace.all_solid.is_set(),
        start_solid: trace.start_solid.is_set(),
        in_open: trace.in_open.is_set(),
        in_water: trace.in_water.is_set(),
        fraction: trace.fraction,
        end: Vec3I32 {
            x: trace.end.x,
            y: trace.end.y,
            z: trace.end.z,
        },
        normal: Vec3I16 {
            x: trace.normal.x,
            y: trace.normal.y,
            z: trace.normal.z,
        },
        plane_distance: trace.plane_distance,
    }
}

#[derive(Copy, Clone)]
pub struct TransformedCollisionHull<'a> {
    shared: SharedTransformedCollisionHull<'a>,
}

impl TransformedCollisionHull<'_> {
    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.shared.point_contents(quake_point_to_psoxide(point))
    }

    /// Trace a Quake Z-up world-space segment through one transformed hull.
    /// Failure and output-preservation semantics are inherited from psx-bsp.
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let mut shared_output = SharedTrace::default();
        if !self.shared.trace_into(
            &quake_point_to_psoxide(*start),
            &quake_point_to_psoxide(*end),
            scratch,
            &mut shared_output,
        ) {
            return false;
        }
        *output = trace_from_shared(shared_output);
        true
    }
}

/// Re-express one shared trace in Quake space.
///
/// The shared tracer's documented contract is that a failed trace leaves every
/// output byte, including padding, exactly as the caller left it. That makes
/// the flag slots byte-preserving storage rather than values, which is why
/// `psx_bsp` gives them the byte-backed `TraceFlag` type: every byte pattern
/// is a legal `TraceFlag`, so an arbitrary byte can be held, copied and passed
/// without ever being an invalid value.
///
/// This function is the one place those bytes become Quake `bool`s, and
/// `TraceFlag::is_set` normalizes rather than reinterprets, so no arbitrary
/// byte reaches a `bool` at all. The slots were once plain `bool`s, and on the
/// guest one of them was observed carrying 0xe7, which made `!all_solid` read
/// false for a trace that had plainly hit a walkable floor and froze the
/// player solid on legal E1M1 floor (see
/// `MovementTraceResult::restore_trace_invariants`). Reading such a slot back
/// as a byte could not have repaired it: by then the invalid `bool` had
/// already been constructed and copied.
pub fn trace_from_shared(trace: SharedTrace) -> Trace {
    Trace {
        all_solid: trace.all_solid.is_set(),
        start_solid: trace.start_solid.is_set(),
        in_open: trace.in_open.is_set(),
        in_water: trace.in_water.is_set(),
        fraction: trace.fraction,
        end: psoxide_point_to_quake(trace.end),
        normal: psoxide_normal_to_quake(trace.normal),
        plane_distance: trace.plane_distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flag slot carrying a byte that is neither zero nor one must still
    /// leave this boundary as a valid `bool`.
    ///
    /// The guest produced 0xe7 in exactly this slot, which made `!all_solid`
    /// read false for a trace that had plainly hit a walkable floor. The bytes
    /// below are that observation, written straight into the live struct.
    ///
    /// This test writes raw bytes and never constructs an invalid `bool`. When
    /// the slots were `bool`s, the only way to reproduce the failure was to
    /// write 0xe7 into one, which is itself undefined behaviour: the test could
    /// not describe the bug without committing it. Byte-backed slots make the
    /// same bytes an ordinary, defined value.
    #[test]
    fn a_poisoned_flag_byte_leaves_this_boundary_valid() {
        let mut shared = SharedTrace::default();
        // SAFETY: the flag slots are the first four bytes of the `#[repr(C)]`
        // shared trace and each is a `repr(transparent)` `u8`, so writing an
        // arbitrary byte through a `*mut u8` produces a valid `SharedTrace`.
        // This is the corruption the boundary exists to absorb.
        unsafe {
            let base = core::ptr::from_mut(&mut shared).cast::<u8>();
            base.write_volatile(0xe7);
            base.add(1).write_volatile(0x00);
            base.add(2).write_volatile(0x02);
            base.add(3).write_volatile(0xff);
        }
        // The bytes really are in the struct: the boundary is absorbing them,
        // not being handed a sanitized copy.
        assert_eq!(shared.all_solid.byte(), 0xe7);
        assert_eq!(shared.start_solid.byte(), 0x00);
        assert_eq!(shared.in_open.byte(), 0x02);
        assert_eq!(shared.in_water.byte(), 0xff);

        let trace = trace_from_shared(shared);
        for (label, flag) in [
            ("all_solid", trace.all_solid),
            ("in_open", trace.in_open),
            ("in_water", trace.in_water),
        ] {
            assert!(flag, "{label} should read as set");
            assert_eq!(
                unsafe { core::ptr::read_volatile(core::ptr::from_ref(&flag).cast::<u8>()) },
                1,
                "{label} must be a valid bool byte"
            );
        }
        assert!(!trace.start_solid);
        assert_eq!(
            unsafe {
                core::ptr::read_volatile(core::ptr::from_ref(&trace.start_solid).cast::<u8>())
            },
            0
        );
    }

    /// Every byte pattern a slot can hold produces exactly one of the two valid
    /// `bool` bytes, and only zero produces `false`.
    #[test]
    fn every_flag_byte_crosses_this_boundary_as_a_valid_bool() {
        for byte in 0..=u8::MAX {
            let mut shared = SharedTrace::default();
            // SAFETY: as above; the first byte is `all_solid`'s slot and every
            // byte pattern is a valid value there.
            unsafe {
                core::ptr::from_mut(&mut shared)
                    .cast::<u8>()
                    .write_volatile(byte);
            }
            let trace = trace_from_shared(shared);
            let observed = unsafe {
                core::ptr::read_volatile(core::ptr::from_ref(&trace.all_solid).cast::<u8>())
            };
            assert_eq!(
                observed,
                u8::from(byte != 0),
                "0x{byte:02x} must normalize, not reinterpret"
            );
        }
    }
}
