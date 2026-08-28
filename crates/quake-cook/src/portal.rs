//! Conservative leaf-portal reconstruction for the retained renderer.
//!
//! Quake BSPs do not retain the `.prt` file produced by the map compiler. The
//! runtime nevertheless benefits from the same coarse doorway hierarchy: a
//! camera leaf should be able to reject whole branches before visiting their
//! faces. This module reconstructs each non-solid leaf as the intersection of
//! its BSP half-spaces, intersects opposite descendants at every split plane,
//! and stores an outward-quantized AABB for every surviving portal fragment.
//!
//! The AABB deliberately overestimates the source winding. It is suitable for
//! a visual-neutral screen gate: false admission costs work, while a portal
//! made smaller by quantization could incorrectly remove geometry.

use std::collections::BTreeMap;

use quake_formats::{
    encode_leaf_bound_max, encode_leaf_bound_min, PORTAL_GRAPH_AREA_RECORD_BYTES,
    PORTAL_GRAPH_EDGE_RECORD_BYTES, PORTAL_GRAPH_FOOTER_BYTES, PORTAL_GRAPH_LEAF_RECORD_BYTES,
    PORTAL_GRAPH_TRAILER_MAGIC,
};

use super::{Bsp, BspLump, CookError};

const SOLID_CONTENTS: i32 = -2;
const BASE_WINDING_EXTENT: f64 = 32_768.0;
const CLIP_EPSILON: f64 = 0.01;
const PORTAL_AREA_EPSILON: f64 = 0.25;
/// BSP splits inside one room normally expose a broad opening. Collapse those
/// convex leaves into one cooked area and keep smaller openings as doorways.
/// The merge is conservative: an over-merged area admits extra faces but can
/// never hide a face.
const AREA_MERGE_PORTAL_AREA: f64 = 48.0 * 48.0;

#[derive(Clone, Copy, Debug)]
struct SourcePlane {
    normal: [f64; 3],
    distance: f64,
}

#[derive(Clone, Copy, Debug)]
struct SourceNode {
    plane: usize,
    children: [i16; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HalfSpace {
    node: usize,
    plane: usize,
    keep_front: bool,
}

#[derive(Clone, Debug)]
struct LeafRegion {
    contents: i32,
    mins: [i16; 3],
    maxs: [i16; 3],
    path: Vec<HalfSpace>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PortalEdge {
    neighbor: u16,
    mins: [i16; 3],
    maxs: [i16; 3],
}

impl PortalEdge {
    fn include(&mut self, mins: [i16; 3], maxs: [i16; 3]) {
        for axis in 0..3 {
            self.mins[axis] = self.mins[axis].min(mins[axis]);
            self.maxs[axis] = self.maxs[axis].max(maxs[axis]);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PortalFragment {
    leaves: [usize; 2],
    mins: [i16; 3],
    maxs: [i16; 3],
    area: f64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PortalGraphStats {
    pub leaf_count: usize,
    pub area_count: usize,
    pub undirected_portal_count: usize,
    pub directed_edge_count: usize,
    pub maximum_leaf_degree: usize,
    pub byte_len: usize,
}

/// Reconstruct and serialize a checked `QPG1` graph. The returned bytes end in
/// the graph footer and are intended to be placed immediately before `QLB1`.
pub fn cook_portal_graph(bsp: &Bsp<'_>) -> Result<(Vec<u8>, PortalGraphStats), CookError> {
    cook_portal_graph_with_merge_area(bsp, AREA_MERGE_PORTAL_AREA)
}

/// Reconstruct and serialize a checked `QPG1` graph with an explicit minimum
/// portal area for merging adjacent leaves. This is primarily an offline
/// partitioning hook: `f64::INFINITY` retains every non-solid BSP leaf as its
/// own area so a RAM-budgeted sectioner can cut through geometrically open
/// rooms. Runtime maps should normally use [`cook_portal_graph`].
pub fn cook_portal_graph_with_merge_area(
    bsp: &Bsp<'_>,
    merge_portal_area: f64,
) -> Result<(Vec<u8>, PortalGraphStats), CookError> {
    if merge_portal_area.is_nan() || merge_portal_area < 0.0 {
        return Err(CookError::new("portal merge area must be non-negative"));
    }
    let planes = source_planes(bsp)?;
    let nodes = source_nodes(bsp, planes.len())?;
    let mut leaves = source_leaves(bsp)?;
    let root = world_head_node(bsp)?;
    if root < 0 {
        return Err(CookError::new("world render head node is a leaf"));
    }
    collect_leaf_paths(root as usize, &nodes, &mut leaves, &mut Vec::new())?;

    let mut front = vec![Vec::<usize>::new(); nodes.len()];
    let mut back = vec![Vec::<usize>::new(); nodes.len()];
    for (leaf_index, leaf) in leaves.iter().enumerate() {
        if leaf.contents == SOLID_CONTENTS {
            continue;
        }
        for half_space in &leaf.path {
            if half_space.keep_front {
                front[half_space.node].push(leaf_index);
            } else {
                back[half_space.node].push(leaf_index);
            }
        }
    }

    let mut fragments = Vec::<PortalFragment>::new();
    for node_index in 0..nodes.len() {
        let separator = planes[nodes[node_index].plane];
        for &front_leaf in &front[node_index] {
            for &back_leaf in &back[node_index] {
                if !bounds_overlap(&leaves[front_leaf], &leaves[back_leaf]) {
                    continue;
                }
                let mut winding = base_winding(separator)?;
                for half_space in leaves[front_leaf]
                    .path
                    .iter()
                    .chain(&leaves[back_leaf].path)
                {
                    if half_space.node == node_index {
                        continue;
                    }
                    winding =
                        clip_winding(&winding, planes[half_space.plane], half_space.keep_front);
                    if winding.len() < 3 {
                        break;
                    }
                }
                let area = if winding.len() < 3 {
                    0.0
                } else {
                    winding_area(&winding, separator.normal)
                };
                if area < PORTAL_AREA_EPSILON {
                    continue;
                }
                let (mins, maxs) = winding_bounds(&winding);
                fragments.push(PortalFragment {
                    leaves: [front_leaf, back_leaf],
                    mins,
                    maxs,
                    area,
                });
            }
        }
    }

    serialize_area_graph(&leaves, &fragments, merge_portal_area)
}

fn serialize_area_graph(
    leaves: &[LeafRegion],
    fragments: &[PortalFragment],
    merge_portal_area: f64,
) -> Result<(Vec<u8>, PortalGraphStats), CookError> {
    let mut union = UnionFind::new(leaves.len());
    for fragment in fragments {
        let [left, right] = fragment.leaves;
        if fragment.area >= merge_portal_area && leaves[left].contents == leaves[right].contents {
            union.join(left, right);
        }
    }

    let mut roots = BTreeMap::<usize, u16>::new();
    let mut leaf_areas = vec![u16::MAX; leaves.len()];
    for (leaf_index, leaf) in leaves.iter().enumerate() {
        if leaf.contents == SOLID_CONTENTS || leaf.path.is_empty() {
            continue;
        }
        let root = union.root(leaf_index);
        let next_area = u16::try_from(roots.len())
            .map_err(|_| CookError::new("portal area count exceeds u16"))?;
        let area = *roots.entry(root).or_insert(next_area);
        leaf_areas[leaf_index] = area;
    }
    let area_count = roots.len();
    let mut edges = vec![BTreeMap::<u16, PortalEdge>::new(); area_count];
    for fragment in fragments {
        let left = leaf_areas[fragment.leaves[0]];
        let right = leaf_areas[fragment.leaves[1]];
        if left == u16::MAX || right == u16::MAX || left == right {
            continue;
        }
        insert_area_edge(
            &mut edges[left as usize],
            right,
            fragment.mins,
            fragment.maxs,
        );
        insert_area_edge(
            &mut edges[right as usize],
            left,
            fragment.mins,
            fragment.maxs,
        );
    }
    serialize_graph(leaf_areas, edges, fragments.len())
}

fn insert_area_edge(
    edges: &mut BTreeMap<u16, PortalEdge>,
    neighbor: u16,
    mins: [i16; 3],
    maxs: [i16; 3],
) {
    match edges.get_mut(&neighbor) {
        Some(edge) => edge.include(mins, maxs),
        None => {
            edges.insert(
                neighbor,
                PortalEdge {
                    neighbor,
                    mins,
                    maxs,
                },
            );
        }
    }
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn root(&mut self, mut index: usize) -> usize {
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        while self.parent[index] != index {
            let next = self.parent[index];
            self.parent[index] = root;
            index = next;
        }
        root
    }

    fn join(&mut self, left: usize, right: usize) {
        let left = self.root(left);
        let right = self.root(right);
        if left != right {
            self.parent[right] = left;
        }
    }
}

fn source_planes(bsp: &Bsp<'_>) -> Result<Vec<SourcePlane>, CookError> {
    bsp.lump(BspLump::Planes)
        .chunks_exact(20)
        .map(|record| {
            let normal = [
                f64::from(f32_at(record, 0)?),
                f64::from(f32_at(record, 4)?),
                f64::from(f32_at(record, 8)?),
            ];
            let length = dot(normal, normal).sqrt();
            if !length.is_finite() || length < 0.5 {
                return Err(CookError::new("BSP portal plane has an invalid normal"));
            }
            Ok(SourcePlane {
                normal: normal.map(|value| value / length),
                distance: f64::from(f32_at(record, 12)?) / length,
            })
        })
        .collect()
}

fn source_nodes(bsp: &Bsp<'_>, plane_count: usize) -> Result<Vec<SourceNode>, CookError> {
    bsp.lump(BspLump::Nodes)
        .chunks_exact(24)
        .map(|record| {
            let plane = nonnegative_i32(i32_at(record, 0)?, "node plane")?;
            if plane >= plane_count {
                return Err(CookError::new("BSP portal node plane is out of range"));
            }
            Ok(SourceNode {
                plane,
                children: [i16_at(record, 4)?, i16_at(record, 6)?],
            })
        })
        .collect()
}

fn source_leaves(bsp: &Bsp<'_>) -> Result<Vec<LeafRegion>, CookError> {
    bsp.lump(BspLump::Leaves)
        .chunks_exact(28)
        .map(|record| {
            Ok(LeafRegion {
                contents: i32_at(record, 0)?,
                mins: [i16_at(record, 8)?, i16_at(record, 10)?, i16_at(record, 12)?],
                maxs: [
                    i16_at(record, 14)?,
                    i16_at(record, 16)?,
                    i16_at(record, 18)?,
                ],
                path: Vec::new(),
            })
        })
        .collect()
}

fn world_head_node(bsp: &Bsp<'_>) -> Result<i32, CookError> {
    let model = bsp
        .lump(BspLump::Models)
        .get(..64)
        .ok_or_else(|| CookError::new("BSP has no world model"))?;
    i32_at(model, 36)
}

fn collect_leaf_paths(
    node_index: usize,
    nodes: &[SourceNode],
    leaves: &mut [LeafRegion],
    path: &mut Vec<HalfSpace>,
) -> Result<(), CookError> {
    let node = *nodes
        .get(node_index)
        .ok_or_else(|| CookError::new("BSP portal node child is out of range"))?;
    for (side, child) in node.children.into_iter().enumerate() {
        path.push(HalfSpace {
            node: node_index,
            plane: node.plane,
            keep_front: side == 0,
        });
        if child >= 0 {
            collect_leaf_paths(child as usize, nodes, leaves, path)?;
        } else {
            let leaf_index = usize::from(child.unsigned_abs())
                .checked_sub(1)
                .ok_or_else(|| CookError::new("invalid BSP portal leaf child"))?;
            let leaf = leaves
                .get_mut(leaf_index)
                .ok_or_else(|| CookError::new("BSP portal leaf child is out of range"))?;
            // Quake aliases every solid child to leaf zero, so that sentinel
            // legitimately has hundreds of tree paths and owns no portal
            // geometry. Non-solid leaves must still have one unique path.
            if leaf.contents != SOLID_CONTENTS {
                if !leaf.path.is_empty() {
                    return Err(CookError::new("BSP portal leaf has multiple tree paths"));
                }
                leaf.path.clone_from(path);
            }
        }
        path.pop();
    }
    Ok(())
}

fn bounds_overlap(left: &LeafRegion, right: &LeafRegion) -> bool {
    (0..3).all(|axis| left.mins[axis] <= right.maxs[axis] && right.mins[axis] <= left.maxs[axis])
}

fn base_winding(plane: SourcePlane) -> Result<Vec<[f64; 3]>, CookError> {
    let mut major = 0usize;
    for axis in 1..3 {
        if plane.normal[axis].abs() > plane.normal[major].abs() {
            major = axis;
        }
    }
    let mut up = [0.0; 3];
    up[(major + 1) % 3] = 1.0;
    let projection = dot(up, plane.normal);
    for axis in 0..3 {
        up[axis] -= projection * plane.normal[axis];
    }
    let up_length = dot(up, up).sqrt();
    if up_length < 0.5 {
        return Err(CookError::new("cannot construct BSP portal winding"));
    }
    up = up.map(|value| value / up_length);
    let right = cross(up, plane.normal);
    let origin = plane.normal.map(|value| value * plane.distance);
    let scaled_up = up.map(|value| value * BASE_WINDING_EXTENT);
    let scaled_right = right.map(|value| value * BASE_WINDING_EXTENT);
    Ok(vec![
        add(add(origin, scaled_right), scaled_up),
        add(sub(origin, scaled_right), scaled_up),
        sub(sub(origin, scaled_right), scaled_up),
        add(sub(origin, scaled_up), scaled_right),
    ])
}

fn clip_winding(winding: &[[f64; 3]], plane: SourcePlane, keep_front: bool) -> Vec<[f64; 3]> {
    if winding.is_empty() {
        return Vec::new();
    }
    let signed_distance = |point: [f64; 3]| {
        let distance = dot(point, plane.normal) - plane.distance;
        if keep_front {
            distance
        } else {
            -distance
        }
    };
    let mut output = Vec::with_capacity(winding.len() + 2);
    let mut previous = *winding.last().expect("nonempty winding");
    let mut previous_distance = signed_distance(previous);
    let mut previous_inside = previous_distance >= -CLIP_EPSILON;
    for &current in winding {
        let current_distance = signed_distance(current);
        let current_inside = current_distance >= -CLIP_EPSILON;
        if current_inside != previous_inside {
            let denominator = previous_distance - current_distance;
            if denominator.abs() > f64::EPSILON {
                let fraction = (previous_distance / denominator).clamp(0.0, 1.0);
                output.push([
                    previous[0] + (current[0] - previous[0]) * fraction,
                    previous[1] + (current[1] - previous[1]) * fraction,
                    previous[2] + (current[2] - previous[2]) * fraction,
                ]);
            }
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
        previous_inside = current_inside;
    }
    output
}

fn winding_area(winding: &[[f64; 3]], normal: [f64; 3]) -> f64 {
    let origin = winding[0];
    let mut twice_area = 0.0;
    for index in 1..winding.len() - 1 {
        let left = sub(winding[index], origin);
        let right = sub(winding[index + 1], origin);
        twice_area += dot(cross(left, right), normal).abs();
    }
    twice_area * 0.5
}

fn winding_bounds(winding: &[[f64; 3]]) -> ([i16; 3], [i16; 3]) {
    let mut mins = [f64::INFINITY; 3];
    let mut maxs = [f64::NEG_INFINITY; 3];
    for point in winding {
        for axis in 0..3 {
            mins[axis] = mins[axis].min(point[axis]);
            maxs[axis] = maxs[axis].max(point[axis]);
        }
    }
    (
        mins.map(|value| value.floor().clamp(i16::MIN as f64, i16::MAX as f64) as i16),
        maxs.map(|value| value.ceil().clamp(i16::MIN as f64, i16::MAX as f64) as i16),
    )
}

fn serialize_graph(
    leaf_areas: Vec<u16>,
    edges: Vec<BTreeMap<u16, PortalEdge>>,
    undirected_portal_count: usize,
) -> Result<(Vec<u8>, PortalGraphStats), CookError> {
    let leaf_count = u16::try_from(leaf_areas.len())
        .map_err(|_| CookError::new("portal graph leaf count exceeds u16"))?;
    let area_count = u16::try_from(edges.len())
        .map_err(|_| CookError::new("portal graph area count exceeds u16"))?;
    let directed_edge_count = edges.iter().map(BTreeMap::len).sum::<usize>();
    let edge_count = u16::try_from(directed_edge_count)
        .map_err(|_| CookError::new("portal graph edge count exceeds u16"))?;
    let maximum_leaf_degree = edges.iter().map(BTreeMap::len).max().unwrap_or(0);
    let capacity = leaf_areas
        .len()
        .checked_mul(PORTAL_GRAPH_LEAF_RECORD_BYTES)
        .and_then(|bytes| {
            bytes.checked_add(edges.len().checked_mul(PORTAL_GRAPH_AREA_RECORD_BYTES)?)
        })
        .and_then(|bytes| {
            bytes.checked_add(directed_edge_count.checked_mul(PORTAL_GRAPH_EDGE_RECORD_BYTES)?)
        })
        .and_then(|bytes| bytes.checked_add(PORTAL_GRAPH_FOOTER_BYTES))
        .ok_or_else(|| CookError::new("portal graph byte size overflow"))?;
    let mut output = Vec::with_capacity(capacity);
    for area in leaf_areas {
        output.extend_from_slice(&area.to_le_bytes());
    }
    let mut first = 0usize;
    for leaf_edges in &edges {
        output.extend_from_slice(
            &u16::try_from(first)
                .map_err(|_| CookError::new("portal graph first edge exceeds u16"))?
                .to_le_bytes(),
        );
        output.extend_from_slice(
            &u16::try_from(leaf_edges.len())
                .map_err(|_| CookError::new("portal graph leaf degree exceeds u16"))?
                .to_le_bytes(),
        );
        first += leaf_edges.len();
    }
    for leaf_edges in &edges {
        for edge in leaf_edges.values() {
            output.extend_from_slice(&edge.neighbor.to_le_bytes());
            output.extend(
                edge.mins
                    .map(encode_leaf_bound_min)
                    .map(|value| value as u8),
            );
            output.extend(
                edge.maxs
                    .map(encode_leaf_bound_max)
                    .map(|value| value as u8),
            );
        }
    }
    output.extend_from_slice(&PORTAL_GRAPH_TRAILER_MAGIC.to_le_bytes());
    output.extend_from_slice(&leaf_count.to_le_bytes());
    output.extend_from_slice(&area_count.to_le_bytes());
    output.extend_from_slice(&edge_count.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    debug_assert_eq!(output.len(), capacity);
    Ok((
        output,
        PortalGraphStats {
            leaf_count: usize::from(leaf_count),
            area_count: usize::from(area_count),
            undirected_portal_count,
            directed_edge_count,
            maximum_leaf_degree,
            byte_len: capacity,
        },
    ))
}

#[inline]
fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[inline]
fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

#[inline]
fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

#[inline]
fn sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn i16_at(bytes: &[u8], offset: usize) -> Result<i16, CookError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| CookError::new("truncated portal i16"))?;
    Ok(i16::from_le_bytes(value.try_into().unwrap()))
}

fn i32_at(bytes: &[u8], offset: usize) -> Result<i32, CookError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| CookError::new("truncated portal i32"))?;
    Ok(i32::from_le_bytes(value.try_into().unwrap()))
}

fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, CookError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| CookError::new("truncated portal f32"))?;
    Ok(f32::from_le_bytes(value.try_into().unwrap()))
}

fn nonnegative_i32(value: i32, context: &str) -> Result<usize, CookError> {
    usize::try_from(value).map_err(|_| CookError::new(format!("negative BSP portal {context}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_a_square_keeps_the_conservative_half() {
        let winding = vec![
            [-2.0, -2.0, 0.0],
            [2.0, -2.0, 0.0],
            [2.0, 2.0, 0.0],
            [-2.0, 2.0, 0.0],
        ];
        let clipped = clip_winding(
            &winding,
            SourcePlane {
                normal: [1.0, 0.0, 0.0],
                distance: 0.0,
            },
            true,
        );
        assert_eq!(clipped.len(), 4);
        assert!(clipped.iter().all(|point| point[0] >= -CLIP_EPSILON));
        assert!((winding_area(&clipped, [0.0, 0.0, 1.0]) - 8.0).abs() < 0.001);
    }

    #[test]
    fn serialized_graph_is_sorted_symmetric_and_wire_bounded() {
        let mut edges = vec![BTreeMap::new(), BTreeMap::new()];
        insert_area_edge(&mut edges[0], 1, [-33, -1, 0], [1, 33, 64]);
        insert_area_edge(&mut edges[1], 0, [-33, -1, 0], [1, 33, 64]);
        let (bytes, stats) = serialize_graph(vec![0, 1], edges, 1).unwrap();
        assert_eq!(stats.leaf_count, 2);
        assert_eq!(stats.area_count, 2);
        assert_eq!(stats.undirected_portal_count, 1);
        assert_eq!(stats.directed_edge_count, 2);
        assert_eq!(stats.maximum_leaf_degree, 1);
        assert_eq!(stats.byte_len, 2 * 2 + 2 * 4 + 2 * 8 + 12);
        assert_eq!(bytes.len(), stats.byte_len);
        assert_eq!(&bytes[12..14], &1u16.to_le_bytes());
        assert_eq!(&bytes[20..22], &0u16.to_le_bytes());
    }
}
