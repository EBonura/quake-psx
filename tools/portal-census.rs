//! Host-only exact BSP portal reconstruction and doorway census.
//!
//! No `.prt` or `.map` files exist for the shareware episode, so the portal
//! graph is rebuilt from the compiled BSP with the standard qbsp
//! `MakeHeadnodePortals`/`MakeTreePortals` construction: a huge box around the
//! world seeds six portals, then every internal node clips a base winding of
//! its own plane against the portals already attached to it and pushes the
//! remainder down to its children. When the descent finishes, every surviving
//! portal joins exactly two leaves and its winding is the exact convex opening
//! between them.
//!
//! This binary only measures. It answers whether a leaf-portal graph is small
//! enough to traverse per frame, how much clustering leaves into rooms helps,
//! and how much of the PVS a portal walk could actually remove. Runtime
//! behaviour is a separate, later step.

use quake_cook::{Bsp, BspLump, PakArchive};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

const CONTENTS_SOLID: i32 = -2;

/// qbsp's winding tolerance. Points nearer than this to a plane are on it.
const ON_EPSILON: f64 = 0.1;
/// Half-extent of a freshly built plane winding, larger than any Quake map.
const BASE_WINDING_EXTENT: f64 = 65536.0;
/// qbsp's tiny-winding edge length. A winding with fewer than three edges
/// longer than this is numerical debris, not an opening.
const TINY_EDGE: f64 = 0.2;

#[derive(Clone, Copy, Debug)]
struct Plane {
    normal: [f64; 3],
    dist: f64,
}

impl Plane {
    fn distance_to(&self, point: [f64; 3]) -> f64 {
        self.normal[0] * point[0] + self.normal[1] * point[1] + self.normal[2] * point[2]
            - self.dist
    }

    fn flipped(&self) -> Self {
        Self {
            normal: [-self.normal[0], -self.normal[1], -self.normal[2]],
            dist: -self.dist,
        }
    }
}

type Winding = Vec<[f64; 3]>;

/// Tree identifier in the BSP child convention: `>= 0` is a node, `< 0` is
/// leaf `-1 - id`. The synthetic outside leaf takes the index one past the
/// real leaves so the head node's box portals have somewhere to attach.
type TreeId = i32;

fn leaf_id(leaf: usize) -> TreeId {
    -1 - (leaf as TreeId)
}

fn as_leaf(id: TreeId) -> Option<usize> {
    (id < 0).then(|| (-1 - id) as usize)
}

#[derive(Clone, Debug)]
struct Portal {
    plane: Plane,
    winding: Winding,
    /// `nodes[0]` lies on the front of `plane`, `nodes[1]` on the back.
    nodes: [TreeId; 2],
    /// Set when the portal is split away or discarded.
    dropped: bool,
}

struct Node {
    plane: usize,
    children: [i16; 2],
}

struct Leaf {
    contents: i32,
    visibility_offset: i32,
    mark_surface_count: usize,
    mins: [i16; 3],
    maxs: [i16; 3],
}

fn base_winding(plane: &Plane) -> Winding {
    let n = plane.normal;
    let major = if n[0].abs() >= n[1].abs() && n[0].abs() >= n[2].abs() {
        0
    } else if n[1].abs() >= n[2].abs() {
        1
    } else {
        2
    };
    let mut up = [0.0f64; 3];
    // Pick an axis that is not the dominant one so the cross products are
    // well conditioned.
    up[if major == 2 { 0 } else { 2 }] = 1.0;
    let dot = up[0] * n[0] + up[1] * n[1] + up[2] * n[2];
    for axis in 0..3 {
        up[axis] -= dot * n[axis];
    }
    normalize(&mut up);

    let right = cross(up, n);
    let origin = [n[0] * plane.dist, n[1] * plane.dist, n[2] * plane.dist];
    let up = scale(up, BASE_WINDING_EXTENT);
    let right = scale(right, BASE_WINDING_EXTENT);
    vec![
        add(sub(origin, right), up),
        add(add(origin, right), up),
        sub(add(origin, right), up),
        sub(sub(origin, right), up),
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], by: f64) -> [f64; 3] {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn normalize(v: &mut [f64; 3]) {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length > 0.0 {
        v[0] /= length;
        v[1] /= length;
        v[2] /= length;
    }
}

/// Split `winding` by `plane`. Returns the piece in front and the piece
/// behind; a piece is `None` when it is empty or degenerate.
fn split_winding(winding: &Winding, plane: &Plane) -> (Option<Winding>, Option<Winding>) {
    let mut sides = Vec::with_capacity(winding.len() + 1);
    let mut distances = Vec::with_capacity(winding.len() + 1);
    let mut counts = [0usize; 3];
    for point in winding {
        let distance = plane.distance_to(*point);
        let side = if distance > ON_EPSILON {
            0
        } else if distance < -ON_EPSILON {
            1
        } else {
            2
        };
        counts[side] += 1;
        sides.push(side);
        distances.push(distance);
    }
    sides.push(sides[0]);
    distances.push(distances[0]);

    if counts[0] == 0 && counts[1] == 0 {
        // Coplanar: the caller decides, but for portal splitting a coplanar
        // fragment belongs to neither side.
        return (None, None);
    }
    if counts[0] == 0 {
        return (None, Some(winding.clone()));
    }
    if counts[1] == 0 {
        return (Some(winding.clone()), None);
    }

    let mut front = Winding::with_capacity(winding.len() + 4);
    let mut back = Winding::with_capacity(winding.len() + 4);
    for index in 0..winding.len() {
        let point = winding[index];
        if sides[index] == 2 {
            front.push(point);
            back.push(point);
            continue;
        }
        if sides[index] == 0 {
            front.push(point);
        } else {
            back.push(point);
        }
        if sides[index + 1] == 2 || sides[index + 1] == sides[index] {
            continue;
        }
        let next = winding[(index + 1) % winding.len()];
        let fraction = distances[index] / (distances[index] - distances[index + 1]);
        let mut mid = [0.0f64; 3];
        for axis in 0..3 {
            // Snap to the plane on its own axis to stop error accumulating
            // through repeated splits.
            mid[axis] = if plane.normal[axis] == 1.0 {
                plane.dist
            } else if plane.normal[axis] == -1.0 {
                -plane.dist
            } else {
                point[axis] + fraction * (next[axis] - point[axis])
            };
        }
        front.push(mid);
        back.push(mid);
    }
    (tidy(front), tidy(back))
}

fn tidy(winding: Winding) -> Option<Winding> {
    if winding.len() < 3 {
        return None;
    }
    Some(winding)
}

/// Keep only the part of `winding` on the front of `plane`.
fn clip_winding(winding: &Winding, plane: &Plane) -> Option<Winding> {
    let (front, _) = split_winding(winding, plane);
    front
}

/// qbsp's `WindingIsTiny`: fewer than three edges longer than `TINY_EDGE`.
fn winding_is_tiny(winding: &Winding) -> bool {
    let mut edges = 0usize;
    for index in 0..winding.len() {
        let delta = sub(winding[(index + 1) % winding.len()], winding[index]);
        let length = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        if length > TINY_EDGE {
            edges += 1;
            if edges == 3 {
                return false;
            }
        }
    }
    true
}

fn winding_area(winding: &Winding) -> f64 {
    let mut total = 0.0;
    for index in 2..winding.len() {
        let a = sub(winding[index - 1], winding[0]);
        let b = sub(winding[index], winding[0]);
        let c = cross(a, b);
        total += 0.5 * (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    }
    total
}

fn winding_bounds(winding: &Winding) -> ([f64; 3], [f64; 3]) {
    let mut mins = [f64::MAX; 3];
    let mut maxs = [f64::MIN; 3];
    for point in winding {
        for axis in 0..3 {
            mins[axis] = mins[axis].min(point[axis]);
            maxs[axis] = maxs[axis].max(point[axis]);
        }
    }
    (mins, maxs)
}

struct PortalGraph {
    portals: Vec<Portal>,
    /// Portal indices currently attached to each tree id.
    node_portals: Vec<Vec<usize>>,
    leaf_portals: Vec<Vec<usize>>,
}

impl PortalGraph {
    fn attached(&self, id: TreeId) -> &Vec<usize> {
        match as_leaf(id) {
            Some(leaf) => &self.leaf_portals[leaf],
            None => &self.node_portals[id as usize],
        }
    }

    fn attached_mut(&mut self, id: TreeId) -> &mut Vec<usize> {
        match as_leaf(id) {
            Some(leaf) => &mut self.leaf_portals[leaf],
            None => &mut self.node_portals[id as usize],
        }
    }

    fn attach(&mut self, portal: usize, front: TreeId, back: TreeId) {
        self.portals[portal].nodes = [front, back];
        self.attached_mut(front).push(portal);
        self.attached_mut(back).push(portal);
    }

    fn detach(&mut self, portal: usize, id: TreeId) {
        let list = self.attached_mut(id);
        if let Some(position) = list.iter().position(|&entry| entry == portal) {
            list.swap_remove(position);
        }
    }
}

fn build_portals(nodes: &[Node], leaves: &[Leaf], planes: &[Plane]) -> Result<PortalGraph> {
    if nodes.is_empty() {
        return Err("BSP has no nodes".into());
    }
    let outside = leaves.len();
    let mut graph = PortalGraph {
        portals: Vec::new(),
        node_portals: vec![Vec::new(); nodes.len()],
        leaf_portals: vec![Vec::new(); leaves.len() + 1],
    };

    // A box comfortably outside the world, so the head node's own portals
    // bound every descent.
    let mut mins = [f64::MAX; 3];
    let mut maxs = [f64::MIN; 3];
    for leaf in leaves {
        if leaf.contents == CONTENTS_SOLID {
            continue;
        }
        for axis in 0..3 {
            mins[axis] = mins[axis].min(leaf.mins[axis] as f64);
            maxs[axis] = maxs[axis].max(leaf.maxs[axis] as f64);
        }
    }
    for axis in 0..3 {
        mins[axis] -= 128.0;
        maxs[axis] += 128.0;
    }

    let mut box_planes = Vec::with_capacity(6);
    for axis in 0..3 {
        for side in 0..2 {
            let mut normal = [0.0f64; 3];
            let dist;
            if side == 0 {
                normal[axis] = 1.0;
                dist = mins[axis];
            } else {
                normal[axis] = -1.0;
                dist = -maxs[axis];
            }
            box_planes.push(Plane { normal, dist });
        }
    }
    for (index, plane) in box_planes.iter().enumerate() {
        let mut winding = base_winding(plane);
        for (other, clip) in box_planes.iter().enumerate() {
            if other == index {
                continue;
            }
            winding = clip_winding(&winding, clip).ok_or("head node box portal collapsed")?;
        }
        let portal = graph.portals.len();
        graph.portals.push(Portal {
            plane: *plane,
            winding,
            nodes: [0, leaf_id(outside)],
            dropped: false,
        });
        graph.attach(portal, 0, leaf_id(outside));
    }

    let mut stack = vec![0i32];
    while let Some(id) = stack.pop() {
        if as_leaf(id).is_some() {
            continue;
        }
        let node = &nodes[id as usize];
        let plane = planes[node.plane];
        let front = node.children[0] as TreeId;
        let back = node.children[1] as TreeId;

        // The node's own portal is its plane clipped by every portal already
        // bounding this node's region.
        let mut winding = Some(base_winding(&plane));
        let bounds = graph.attached(id).clone();
        for &portal in &bounds {
            let bound = &graph.portals[portal];
            let clip = if bound.nodes[0] == id {
                bound.plane
            } else {
                bound.plane.flipped()
            };
            winding = winding.and_then(|w| clip_winding(&w, &clip));
            if winding.is_none() {
                break;
            }
        }
        if let Some(winding) = winding.filter(|w| !winding_is_tiny(w)) {
            {
                let portal = graph.portals.len();
                graph.portals.push(Portal {
                    plane,
                    winding,
                    nodes: [front, back],
                    dropped: false,
                });
                graph.attach(portal, front, back);
            }
        }

        // Push every other portal on this node to whichever children it
        // reaches.
        for portal in std::mem::take(graph.attached_mut(id)) {
            if graph.portals[portal].dropped {
                continue;
            }
            let side = if graph.portals[portal].nodes[0] == id {
                0
            } else {
                1
            };
            let other = graph.portals[portal].nodes[1 - side];
            graph.detach(portal, other);
            let (front_piece, back_piece) =
                split_winding(&graph.portals[portal].winding, &plane);
            let front_piece = front_piece.filter(|w| !winding_is_tiny(w));
            let back_piece = back_piece.filter(|w| !winding_is_tiny(w));
            match (front_piece, back_piece) {
                (None, None) => graph.portals[portal].dropped = true,
                (Some(w), None) => {
                    graph.portals[portal].winding = w;
                    let (a, b) = if side == 0 { (front, other) } else { (other, front) };
                    graph.attach(portal, a, b);
                }
                (None, Some(w)) => {
                    graph.portals[portal].winding = w;
                    let (a, b) = if side == 0 { (back, other) } else { (other, back) };
                    graph.attach(portal, a, b);
                }
                (Some(front_w), Some(back_w)) => {
                    let clone = graph.portals.len();
                    let mut copy = graph.portals[portal].clone();
                    copy.winding = back_w;
                    graph.portals.push(copy);
                    graph.portals[portal].winding = front_w;
                    let (a, b) = if side == 0 { (front, other) } else { (other, front) };
                    graph.attach(portal, a, b);
                    let (a, b) = if side == 0 { (back, other) } else { (other, back) };
                    graph.attach(clone, a, b);
                }
            }
        }

        stack.push(front);
        stack.push(back);
    }

    Ok(graph)
}

fn parse_planes(bsp: &Bsp<'_>) -> Vec<Plane> {
    bsp.lump(BspLump::Planes)
        .chunks_exact(20)
        .map(|record| Plane {
            normal: [
                f32_at(record, 0) as f64,
                f32_at(record, 4) as f64,
                f32_at(record, 8) as f64,
            ],
            dist: f32_at(record, 12) as f64,
        })
        .collect()
}

fn parse_nodes(bsp: &Bsp<'_>) -> Vec<Node> {
    bsp.lump(BspLump::Nodes)
        .chunks_exact(24)
        .map(|record| Node {
            plane: i32_at(record, 0) as usize,
            children: [i16_at(record, 4), i16_at(record, 6)],
        })
        .collect()
}

fn parse_leaves(bsp: &Bsp<'_>) -> Vec<Leaf> {
    bsp.lump(BspLump::Leaves)
        .chunks_exact(28)
        .map(|record| Leaf {
            contents: i32_at(record, 0),
            visibility_offset: i32_at(record, 4),
            mark_surface_count: u16_at(record, 22) as usize,
            mins: [i16_at(record, 8), i16_at(record, 10), i16_at(record, 12)],
            maxs: [i16_at(record, 14), i16_at(record, 16), i16_at(record, 18)],
        })
        .collect()
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

/// Decompress one Quake PVS row into a leaf bitset.
fn decompress_visibility(visibility: &[u8], offset: usize, leaves: usize) -> Vec<bool> {
    let mut out = vec![false; leaves];
    let mut index = 0usize;
    let mut cursor = offset;
    while index < leaves {
        let Some(&byte) = visibility.get(cursor) else {
            break;
        };
        cursor += 1;
        if byte != 0 {
            for bit in 0..8 {
                if index + bit < leaves && byte & (1 << bit) != 0 {
                    out[index + bit] = true;
                }
            }
            index += 8;
            continue;
        }
        let Some(&run) = visibility.get(cursor) else {
            break;
        };
        cursor += 1;
        index += run as usize * 8;
    }
    out
}

/// The runtime frustum is `forward +- right` and `forward +- up`, so the
/// admitted region is exactly `|x| <= z` and `|y| <= z` in camera space. Screen
/// rectangles therefore live in tangent space clamped to [-1, 1].
const NEAR_PLANE_UNITS: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    mins: [f64; 2],
    maxs: [f64; 2],
}

impl Rect {
    const FULL: Self = Self {
        mins: [-1.0, -1.0],
        maxs: [1.0, 1.0],
    };

    fn intersect(self, other: Self) -> Option<Self> {
        let mins = [
            self.mins[0].max(other.mins[0]),
            self.mins[1].max(other.mins[1]),
        ];
        let maxs = [
            self.maxs[0].min(other.maxs[0]),
            self.maxs[1].min(other.maxs[1]),
        ];
        (mins[0] < maxs[0] && mins[1] < maxs[1]).then_some(Self { mins, maxs })
    }

    fn union(self, other: Self) -> Self {
        Self {
            mins: [
                self.mins[0].min(other.mins[0]),
                self.mins[1].min(other.mins[1]),
            ],
            maxs: [
                self.maxs[0].max(other.maxs[0]),
                self.maxs[1].max(other.maxs[1]),
            ],
        }
    }
}

struct Basis {
    eye: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    forward: [f64; 3],
}

impl Basis {
    fn new(eye: [f64; 3], yaw: f64, pitch: f64) -> Self {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let forward = [cp * cy, cp * sy, -sp];
        // Quake's AngleVectors with roll = 0: right is -sy, cy, 0 negated.
        let right = [sy, -cy, 0.0];
        let up = cross(right, forward);
        Self {
            eye,
            right,
            up,
            forward,
        }
    }

    fn to_camera(&self, point: [f64; 3]) -> [f64; 3] {
        let d = sub(point, self.eye);
        [
            d[0] * self.right[0] + d[1] * self.right[1] + d[2] * self.right[2],
            d[0] * self.up[0] + d[1] * self.up[1] + d[2] * self.up[2],
            d[0] * self.forward[0] + d[1] * self.forward[1] + d[2] * self.forward[2],
        ]
    }
}

/// Project a portal winding to its conservative screen rectangle. The winding
/// is clipped to the near plane in 3D first, so a portal that straddles the
/// eye plane keeps an exact bound instead of forcing a full-screen admission.
fn project_portal(winding: &Winding, basis: &Basis) -> Option<Rect> {
    let mut camera: Vec<[f64; 3]> = winding.iter().map(|p| basis.to_camera(*p)).collect();
    // Clip to z >= near.
    let mut clipped = Vec::with_capacity(camera.len() + 2);
    for index in 0..camera.len() {
        let current = camera[index];
        let next = camera[(index + 1) % camera.len()];
        let inside = current[2] >= NEAR_PLANE_UNITS;
        let next_inside = next[2] >= NEAR_PLANE_UNITS;
        if inside {
            clipped.push(current);
        }
        if inside != next_inside {
            let fraction = (NEAR_PLANE_UNITS - current[2]) / (next[2] - current[2]);
            clipped.push([
                current[0] + fraction * (next[0] - current[0]),
                current[1] + fraction * (next[1] - current[1]),
                NEAR_PLANE_UNITS,
            ]);
        }
    }
    camera = clipped;
    if camera.len() < 3 {
        return None;
    }
    let mut mins = [f64::MAX; 2];
    let mut maxs = [f64::MIN; 2];
    for point in &camera {
        let x = point[0] / point[2];
        let y = point[1] / point[2];
        mins[0] = mins[0].min(x);
        mins[1] = mins[1].min(y);
        maxs[0] = maxs[0].max(x);
        maxs[1] = maxs[1].max(y);
    }
    Rect {
        mins,
        maxs,
    }
    .intersect(Rect::FULL)
}

/// Does a portal winding's world AABB survive the four frustum planes? This is
/// the cheap test: four dot products against the most positive corner, no GTE
/// projection and no division.
fn winding_in_frustum(winding: &Winding, basis: &Basis) -> bool {
    let (mins, maxs) = winding_bounds(winding);
    let planes = [
        add(basis.forward, basis.right),
        sub(basis.forward, basis.right),
        add(basis.forward, basis.up),
        sub(basis.forward, basis.up),
    ];
    for normal in planes {
        let mut best = 0.0;
        for axis in 0..3 {
            let low = mins[axis] - basis.eye[axis];
            let high = maxs[axis] - basis.eye[axis];
            best += normal[axis] * if normal[axis] >= 0.0 { high } else { low };
        }
        if best < 0.0 {
            return false;
        }
    }
    true
}

/// Round a bound outward onto the resident 32-unit leaf-bounds grid.
fn grid_min(value: i16) -> i16 {
    ((value as i32) >> 5 << 5) as i16
}

fn grid_max(value: i16) -> i16 {
    (((value as i32) + 31) >> 5 << 5) as i16
}

fn aabb_in_frustum(mins: [i16; 3], maxs: [i16; 3], basis: &Basis) -> bool {
    leaf_in_frustum(mins, maxs, basis)
}

/// Does the leaf AABB survive the same four frustum planes the runtime uses?
fn leaf_in_frustum(mins: [i16; 3], maxs: [i16; 3], basis: &Basis) -> bool {
    // forward +- right and forward +- up, evaluated against the AABB's most
    // positive corner, exactly like `aabb_outside_clip4`.
    let planes = [
        add(basis.forward, basis.right),
        sub(basis.forward, basis.right),
        add(basis.forward, basis.up),
        sub(basis.forward, basis.up),
    ];
    for normal in planes {
        let mut best = 0.0;
        for axis in 0..3 {
            let low = mins[axis] as f64 - basis.eye[axis];
            let high = maxs[axis] as f64 - basis.eye[axis];
            best += normal[axis] * if normal[axis] >= 0.0 { high } else { low };
        }
        if best < 0.0 {
            return false;
        }
    }
    true
}

/// Union-find over leaves so open-space BSP splits can be merged into rooms
/// while narrow openings stay as doorways.
struct Cells {
    parent: Vec<usize>,
}

impl Cells {
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count).collect(),
        }
    }

    fn find(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }
}

#[derive(Default, Clone, Copy)]
struct SimStats {
    samples: usize,
    frustum_leaves: usize,
    frustum_faces: usize,
    admitted_units: usize,
    admitted_faces: usize,
    portals_tested: usize,
    pvs_faces: usize,
    admitted_leaves_raw: usize,
    admitted_marks_raw: usize,
    pvs_leaves: usize,
}

struct Level {
    leaves: Vec<Leaf>,
    /// One entry per leaf: the cell it belongs to.
    cell_of: Vec<usize>,
    cell_count: usize,
    /// Inter-cell portals as `(other_cell, portal_index)` per cell.
    adjacency: Vec<Vec<(usize, usize)>>,
    portals: Vec<Winding>,
    /// Per portal: the conservative bound a runtime with no cooked geometry
    /// would use, namely the two leaves' 32-unit bounds intersected.
    portal_bounds: Vec<([i16; 3], [i16; 3])>,
}

/// Walk every open leaf centre at eight yaws and compare what the runtime's
/// PVS-plus-frustum path admits against what a conservative portal walk over
/// this cell partition admits.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Full recursive screen-rectangle narrowing, re-testing a cell whenever
    /// its admitted rectangle grows. Tightest and most expensive.
    Rect,
    /// One pass: a cell keeps the first rectangle that reached it. Cheaper,
    /// slightly less tight, still conservative because the rectangle only
    /// grows in the exact walk.
    RectOnce,
    /// No projection at all: a portal is crossed when its world AABB survives
    /// the same four frustum planes the face selector already uses.
    FrustumOnly,
    /// Same as `FrustumOnly` but the portal's own AABB is replaced by the
    /// intersection of the two leaves' already-resident 32-unit leaf bounds.
    /// That intersection contains the portal, so the test stays conservative
    /// and the cooked sidecar carries no geometry at all.
    PairBoundsOnly,
}

fn simulate(
    level: &Level,
    visibility: &[u8],
    visible_leaves: usize,
    mode: Mode,
) -> (SimStats, Vec<usize>) {
    simulate_scored(level, visibility, visible_leaves, mode, &mut Vec::new())
}

/// As [`simulate`], but also accumulates how often each portal was the test
/// that stopped the walk. A portal nothing is ever rejected at carries no
/// information and can be merged away.
fn simulate_scored(
    level: &Level,
    visibility: &[u8],
    visible_leaves: usize,
    mode: Mode,
    rejections: &mut Vec<u32>,
) -> (SimStats, Vec<usize>) {
    rejections.clear();
    rejections.resize(level.portals.len(), 0);
    let leaves = &level.leaves;
    let mut stats = SimStats::default();
    let mut admitted_face_samples = Vec::new();
    let mut cell_rect: Vec<Option<Rect>> = vec![None; level.cell_count];
    let mut queue: Vec<usize> = Vec::new();
    let mut cell_admitted: Vec<bool> = vec![false; level.cell_count];

    for (index, leaf) in leaves.iter().enumerate().skip(1) {
        if leaf.contents == CONTENTS_SOLID || leaf.visibility_offset < 0 {
            continue;
        }
        if (0..3).any(|axis| leaf.maxs[axis] <= leaf.mins[axis]) {
            continue;
        }
        let eye = [
            0.5 * (leaf.mins[0] as f64 + leaf.maxs[0] as f64),
            0.5 * (leaf.mins[1] as f64 + leaf.maxs[1] as f64),
            0.5 * (leaf.mins[2] as f64 + leaf.maxs[2] as f64),
        ];
        let row = decompress_visibility(visibility, leaf.visibility_offset as usize, visible_leaves);
        let mut pvs = vec![false; leaves.len()];
        pvs[index] = true;
        for (bit, &set) in row.iter().enumerate() {
            if set && bit + 1 < leaves.len() {
                pvs[bit + 1] = true;
            }
        }
        let start_cell = level.cell_of[index];

        for step in 0..8 {
            let basis = Basis::new(eye, std::f64::consts::TAU * step as f64 / 8.0, 0.0);
            let mut frustum_leaves = 0usize;
            let mut frustum_faces = 0usize;
            let mut pvs_faces = 0usize;
            for (candidate, &set) in pvs.iter().enumerate() {
                if !set {
                    continue;
                }
                pvs_faces += leaves[candidate].mark_surface_count;
                if candidate == index
                    || leaf_in_frustum(leaves[candidate].mins, leaves[candidate].maxs, &basis)
                {
                    frustum_leaves += 1;
                    frustum_faces += leaves[candidate].mark_surface_count;
                }
            }

            cell_rect.iter_mut().for_each(|slot| *slot = None);
            cell_rect[start_cell] = Some(Rect::FULL);
            queue.clear();
            queue.push(start_cell);
            let mut tested = 0usize;
            while let Some(current) = queue.pop() {
                let Some(parent) = cell_rect[current] else {
                    continue;
                };
                for &(neighbour, portal) in &level.adjacency[current] {
                    if mode != Mode::Rect && cell_rect[neighbour].is_some() {
                        continue;
                    }
                    tested += 1;
                    let clipped = if mode == Mode::PairBoundsOnly {
                        if !aabb_in_frustum(
                            level.portal_bounds[portal].0,
                            level.portal_bounds[portal].1,
                            &basis,
                        ) {
                            rejections[portal] += 1;
                            continue;
                        }
                        Rect::FULL
                    } else if mode == Mode::FrustumOnly {
                        if !winding_in_frustum(&level.portals[portal], &basis) {
                            rejections[portal] += 1;
                            continue;
                        }
                        Rect::FULL
                    } else {
                        let Some(screen) = project_portal(&level.portals[portal], &basis) else {
                            rejections[portal] += 1;
                            continue;
                        };
                        let Some(clipped) = screen.intersect(parent) else {
                            rejections[portal] += 1;
                            continue;
                        };
                        clipped
                    };
                    let merged = match cell_rect[neighbour] {
                        Some(existing) => {
                            let union = existing.union(clipped);
                            if union == existing {
                                continue;
                            }
                            union
                        }
                        None => clipped,
                    };
                    cell_rect[neighbour] = Some(merged);
                    queue.push(neighbour);
                }
            }

            cell_admitted
                .iter_mut()
                .enumerate()
                .for_each(|(cell, slot)| *slot = cell_rect[cell].is_some());
            // Portal admission is an extra filter in front of the existing
            // per-face backface and frustum tests, so the honest comparison is
            // admitted-and-in-frustum against in-frustum.
            let mut admitted_faces = 0usize;
            let mut admitted_leaves = 0usize;
            let mut admitted_leaves_raw = 0usize;
            let mut admitted_marks_raw = 0usize;
            let mut pvs_leaves = 0usize;
            for (candidate, &set) in pvs.iter().enumerate() {
                if !set {
                    continue;
                }
                pvs_leaves += 1;
                if !cell_admitted[level.cell_of[candidate]] {
                    continue;
                }
                admitted_leaves_raw += 1;
                admitted_marks_raw += leaves[candidate].mark_surface_count;
                if candidate != index
                    && !leaf_in_frustum(leaves[candidate].mins, leaves[candidate].maxs, &basis)
                {
                    continue;
                }
                admitted_leaves += 1;
                admitted_faces += leaves[candidate].mark_surface_count;
            }

            stats.samples += 1;
            stats.frustum_leaves += frustum_leaves;
            stats.frustum_faces += frustum_faces;
            stats.pvs_faces += pvs_faces;
            stats.admitted_leaves_raw += admitted_leaves_raw;
            stats.admitted_marks_raw += admitted_marks_raw;
            stats.pvs_leaves += pvs_leaves;
            stats.admitted_units += admitted_leaves;
            stats.admitted_faces += admitted_faces;
            stats.portals_tested += tested;
            admitted_face_samples.push(admitted_faces);
        }
    }
    admitted_face_samples.sort_unstable();
    (stats, admitted_face_samples)
}

fn percentile(sorted: &[usize], fraction: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64 * fraction).ceil() as usize).max(1) - 1;
    sorted[rank.min(sorted.len() - 1)]
}

/// Portals with an area at or above the threshold are treated as open space and
/// merged; everything smaller stays a doorway. `f64::MAX` keeps every leaf
/// separate, `0.0` merges the whole map into one cell.
const MERGE_THRESHOLDS: [f64; 7] = [f64::MAX, 16384.0, 8192.0, 4096.0, 2048.0, 1024.0, 256.0];

fn census(name: &str, bytes: &[u8]) -> Result<()> {
    let bsp = Bsp::parse(bytes)?;
    let planes = parse_planes(&bsp);
    let nodes = parse_nodes(&bsp);
    let leaves = parse_leaves(&bsp);
    let visibility = bsp.lump(BspLump::Visibility);
    let graph = build_portals(&nodes, &leaves, &planes)?;
    let outside = leaves.len();
    let visible_leaves = leaves.len().saturating_sub(1);

    // Keep only leaf-to-leaf portals between two open leaves; those are the
    // openings a camera can actually see through.
    let mut open_portals: Vec<(usize, usize, Winding, f64)> = Vec::new();
    for portal in &graph.portals {
        if portal.dropped {
            continue;
        }
        let (Some(front), Some(back)) = (as_leaf(portal.nodes[0]), as_leaf(portal.nodes[1])) else {
            continue;
        };
        if front == outside
            || back == outside
            || leaves[front].contents == CONTENTS_SOLID
            || leaves[back].contents == CONTENTS_SOLID
        {
            continue;
        }
        let area = winding_area(&portal.winding);
        open_portals.push((front, back, portal.winding.clone(), area));
    }

    let open_leaves = leaves
        .iter()
        .filter(|leaf| leaf.contents != CONTENTS_SOLID)
        .count();
    let vertices: usize = open_portals.iter().map(|(_, _, w, _)| w.len()).sum();
    let max_vertices = open_portals
        .iter()
        .map(|(_, _, w, _)| w.len())
        .max()
        .unwrap_or(0);
    let mut per_leaf = vec![0usize; leaves.len()];
    for (front, back, _, _) in &open_portals {
        per_leaf[*front] += 1;
        per_leaf[*back] += 1;
    }
    let mut degrees: Vec<usize> = per_leaf
        .iter()
        .enumerate()
        .filter(|(index, _)| leaves[*index].contents != CONTENTS_SOLID)
        .map(|(_, &count)| count)
        .collect();
    degrees.sort_unstable();
    let mut areas: Vec<f64> = open_portals.iter().map(|(_, _, _, area)| *area).collect();
    areas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let area_at = |fraction: f64| -> f64 {
        if areas.is_empty() {
            return 0.0;
        }
        let rank = ((areas.len() as f64 * fraction).ceil() as usize).max(1) - 1;
        areas[rank.min(areas.len() - 1)]
    };

    println!(
        "{name}: nodes={nodes_len} leaves={leaves_len} open={open_leaves} portals={portals} \
         vertices/portal mean={mean:.2} max={max_vertices}",
        nodes_len = nodes.len(),
        leaves_len = leaves.len(),
        portals = open_portals.len(),
        mean = vertices as f64 / open_portals.len().max(1) as f64,
    );
    println!(
        "  portals/leaf p50={p50} p95={p95} max={max}; area p50={a50:.0} p95={a95:.0}",
        p50 = percentile(&degrees, 0.50),
        p95 = percentile(&degrees, 0.95),
        max = degrees.last().copied().unwrap_or(0),
        a50 = area_at(0.50),
        a95 = area_at(0.95),
    );

    for threshold in MERGE_THRESHOLDS {
        let mut cells = Cells::new(leaves.len());
        for (front, back, _, area) in &open_portals {
            if *area >= threshold {
                cells.union(*front, *back);
            }
        }
        let mut label = vec![usize::MAX; leaves.len()];
        let mut cell_count = 0usize;
        let mut cell_of = vec![0usize; leaves.len()];
        for leaf in 0..leaves.len() {
            let root = cells.find(leaf);
            if label[root] == usize::MAX {
                label[root] = cell_count;
                cell_count += 1;
            }
            cell_of[leaf] = label[root];
        }

        let mut adjacency: Vec<Vec<(usize, usize)>> = vec![Vec::new(); cell_count];
        let mut portals = Vec::new();
        let mut portal_bounds = Vec::new();
        for (front, back, winding, _) in &open_portals {
            let (a, b) = (cell_of[*front], cell_of[*back]);
            if a == b {
                continue;
            }
            let index = portals.len();
            portals.push(winding.clone());
            let mut mins = [0i16; 3];
            let mut maxs = [0i16; 3];
            for axis in 0..3 {
                mins[axis] = grid_min(leaves[*front].mins[axis]).max(grid_min(leaves[*back].mins[axis]));
                maxs[axis] = grid_max(leaves[*front].maxs[axis]).min(grid_max(leaves[*back].maxs[axis]));
            }
            portal_bounds.push((mins, maxs));
            adjacency[a].push((b, index));
            adjacency[b].push((a, index));
        }

        let level = Level {
            leaves: leaves
                .iter()
                .map(|leaf| Leaf {
                    contents: leaf.contents,
                    visibility_offset: leaf.visibility_offset,
                    mark_surface_count: leaf.mark_surface_count,
                    mins: leaf.mins,
                    maxs: leaf.maxs,
                })
                .collect(),
            cell_of,
            cell_count,
            adjacency,
            portals,
            portal_bounds,
        };
        let modes: &[(Mode, &str)] = if threshold == f64::MAX {
            &[
                (Mode::Rect, "leaf/rect"),
                (Mode::RectOnce, "leaf/rect1"),
                (Mode::FrustumOnly, "leaf/aabb"),
                (Mode::PairBoundsOnly, "leaf/pair"),
            ]
        } else {
            &[(Mode::Rect, "merge/rect")]
        };
        for &(mode, mode_label) in modes {
        let (stats, samples) = simulate(&level, visibility, visible_leaves, mode);
        let n = stats.samples.max(1) as f64;
        let label = if threshold == f64::MAX {
            mode_label.to_string()
        } else {
            format!("{mode_label}>={threshold:.0}")
        };
        println!(
            "  [{label:>13}] cells={cells:5} doorways={doorways:5} | pvs={pv:6.1} frustum={ff:6.1} \
             | admitted={af:6.1} (p95 {p95:4}) removed={removed:5.2}% | tests/frame={tests:6.1}",
            cells = level.cell_count,
            doorways = level.portals.len(),
            pv = stats.pvs_faces as f64 / n,
            ff = stats.frustum_faces as f64 / n,
            af = stats.admitted_faces as f64 / n,
            p95 = percentile(&samples, 0.95),
            removed = 100.0 * (1.0 - stats.admitted_faces as f64 / stats.frustum_faces.max(1) as f64),
            tests = stats.portals_tested as f64 / n,
        );
        println!(
            "                 runtime cost proxy: pvs_leaves={pl:.1} admitted_leaves={al:.1} \
             mark_writes={mw:.1}",
            pl = stats.pvs_leaves as f64 / n,
            al = stats.admitted_leaves_raw as f64 / n,
            mw = stats.admitted_marks_raw as f64 / n,
        );
        }
    }

    // Can a bounded gate budget fit the 14,042-byte resident-map arena margin?
    // Score every portal by how often it actually stopped the walk, keep the
    // best K as gates and merge the leaves either side of everything else.
    let mut level = build_level(&leaves, &open_portals, &vec![usize::MAX; 0]);
    let mut scores = Vec::new();
    let _ = simulate_scored(&level, visibility, visible_leaves, Mode::FrustumOnly, &mut scores);
    let mut order: Vec<usize> = (0..open_portals.len()).collect();
    order.sort_by_key(|&index| core::cmp::Reverse(scores[index]));
    for gates in [64usize, 128, 256, 512, 1024, 2048] {
        if gates > open_portals.len() {
            continue;
        }
        let mut keep = vec![false; open_portals.len()];
        for &index in order.iter().take(gates) {
            keep[index] = true;
        }
        let mut cells = Cells::new(leaves.len());
        for (index, (front, back, _, _)) in open_portals.iter().enumerate() {
            if !keep[index] {
                cells.union(*front, *back);
            }
        }
        let mut label = vec![usize::MAX; leaves.len()];
        let mut cell_count = 0usize;
        let mut cell_of = vec![0usize; leaves.len()];
        for leaf in 0..leaves.len() {
            let root = cells.find(leaf);
            if label[root] == usize::MAX {
                label[root] = cell_count;
                cell_count += 1;
            }
            cell_of[leaf] = label[root];
        }
        level = build_level_with_cells(&leaves, &open_portals, cell_of, cell_count);
        let (stats, samples) = simulate(&level, visibility, visible_leaves, Mode::FrustumOnly);
        let n = stats.samples.max(1) as f64;
        let bytes = (cell_count + 1) * 2 + level.portals.len() * 2 * 2 + level.portals.len() * 6;
        println!(
            "  [gate/aabb {gates:5}] cells={cells:5} doorways={doorways:5} sidecar={bytes:6}B | \
             admitted={af:6.1} (p95 {p95:4}) removed={removed:5.2}% | tests/frame={tests:6.1}",
            cells = level.cell_count,
            doorways = level.portals.len(),
            af = stats.admitted_faces as f64 / n,
            p95 = percentile(&samples, 0.95),
            removed = 100.0
                * (1.0 - stats.admitted_faces as f64 / stats.frustum_faces.max(1) as f64),
            tests = stats.portals_tested as f64 / n,
        );
    }
    Ok(())
}

fn build_level(
    leaves: &[Leaf],
    open_portals: &[(usize, usize, Winding, f64)],
    _unused: &[usize],
) -> Level {
    build_level_with_cells(leaves, open_portals, (0..leaves.len()).collect(), leaves.len())
}

fn build_level_with_cells(
    leaves: &[Leaf],
    open_portals: &[(usize, usize, Winding, f64)],
    cell_of: Vec<usize>,
    cell_count: usize,
) -> Level {
    let mut adjacency: Vec<Vec<(usize, usize)>> = vec![Vec::new(); cell_count];
    let mut portals = Vec::new();
    let mut portal_bounds = Vec::new();
    for (front, back, winding, _) in open_portals {
        let (a, b) = (cell_of[*front], cell_of[*back]);
        if a == b {
            continue;
        }
        let index = portals.len();
        portals.push(winding.clone());
        let mut mins = [0i16; 3];
        let mut maxs = [0i16; 3];
        for axis in 0..3 {
            mins[axis] =
                grid_min(leaves[*front].mins[axis]).max(grid_min(leaves[*back].mins[axis]));
            maxs[axis] =
                grid_max(leaves[*front].maxs[axis]).min(grid_max(leaves[*back].maxs[axis]));
        }
        portal_bounds.push((mins, maxs));
        adjacency[a].push((b, index));
        adjacency[b].push((a, index));
    }
    Level {
        leaves: leaves
            .iter()
            .map(|leaf| Leaf {
                contents: leaf.contents,
                visibility_offset: leaf.visibility_offset,
                mark_surface_count: leaf.mark_surface_count,
                mins: leaf.mins,
                maxs: leaf.maxs,
            })
            .collect(),
        cell_of,
        cell_count,
        adjacency,
        portals,
        portal_bounds,
    }
}

fn resolve_pak(root: &Path) -> Result<PathBuf> {
    let candidates = [
        root.join(".quakepsx/cache/shareware/ID1/PAK0.PAK"),
        root.join(".quakepsx/cache/shareware/id1/pak0.pak"),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // Fall back to a case-insensitive scan of the extracted shareware tree.
    let base = root.join(".quakepsx/cache/shareware");
    let mut stack = vec![base];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("pak0.pak"))
            {
                return Ok(path);
            }
        }
    }
    Err("shareware PAK0.PAK was not found under .quakepsx/cache/shareware".into())
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let requested: Vec<String> = env::args().skip(1).collect();
    let pak_bytes = fs::read(resolve_pak(&root)?)?;
    let pak = PakArchive::parse(&pak_bytes)?;

    // Resident-budget context: the portal sidecar has to live inside the one
    // 880,000-byte resident-map arena the guest reserves for every map.
    println!("cooked source lump sizes (BSP bytes, not the cooked resident form)");
    for map in MAPS {
        let bytes = pak.require(&format!("maps/{map}.bsp"))?;
        let bsp = Bsp::parse(bytes)?;
        let leaves = parse_leaves(&bsp);
        let nodes = parse_nodes(&bsp);
        let planes = parse_planes(&bsp);
        let graph = build_portals(&nodes, &leaves, &planes)?;
        let outside = leaves.len();
        let mut sides = 0usize;
        let mut portals = 0usize;
        for portal in &graph.portals {
            if portal.dropped {
                continue;
            }
            let (Some(front), Some(back)) = (as_leaf(portal.nodes[0]), as_leaf(portal.nodes[1]))
            else {
                continue;
            };
            if front == outside
                || back == outside
                || leaves[front].contents == CONTENTS_SOLID
                || leaves[back].contents == CONTENTS_SOLID
            {
                continue;
            }
            portals += 1;
            sides += 2;
        }
        let offsets = (leaves.len() + 1) * 2;
        let neighbours = sides * 2;
        let bounds = portals * 6;
        println!(
            "  {map}: leaves={l} portals={portals} | sidecar offsets={offsets} \
             neighbours={neighbours} portal_bounds={bounds} total={total} bytes",
            l = leaves.len(),
            total = offsets + neighbours + bounds,
        );
    }
    println!();
    println!("quake-psx exact BSP portal census (host only)");
    for map in MAPS {
        if !requested.is_empty() && !requested.iter().any(|name| name == map) {
            continue;
        }
        let bytes = pak.require(&format!("maps/{map}.bsp"))?;
        census(map, bytes)?;
    }
    Ok(())
}
