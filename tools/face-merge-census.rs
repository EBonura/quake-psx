//! How much of Quake's BSP face fragmentation can be merged back?
//!
//! Quake II PSX considers 226 source quads per present and only 56 survive.
//! quake-psx considers 774 PVS faces and selects 254. A large part of that gap
//! is that a Quake BSP splits one authored wall into many faces, and every
//! fragment then pays selection, materialization and packet cost separately.
//!
//! This measures the ceiling of merging coplanar neighbours back together with
//! qbsp's own `TryMerge` rule: same plane and side, same texture information,
//! same light styles, sharing a complete edge, and convex after the join.
//! Collinear vertices on the merged boundary are deliberately retained, because
//! removing them would create T-junctions against the perpendicular faces that
//! still meet there.

use quake_cook::{Bsp, BspLump, PakArchive};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];
const CONTINUOUS_EPSILON: f64 = 0.1;

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

#[derive(Clone)]
struct Face {
    plane: u16,
    side: i16,
    texinfo: u16,
    styles: [u8; 4],
    points: Vec<[f64; 3]>,
    alive: bool,
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn same(a: [f64; 3], b: [f64; 3]) -> bool {
    (a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05 && (a[2] - b[2]).abs() < 0.05
}

/// qbsp's `TryMerge`: join two coplanar polygons across a shared edge and keep
/// the result only if it stays convex.
fn try_merge(a: &[[f64; 3]], b: &[[f64; 3]], normal: [f64; 3]) -> Option<Vec<[f64; 3]>> {
    // Find the shared edge, traversed in opposite directions.
    let mut join = None;
    for i in 0..a.len() {
        let a0 = a[i];
        let a1 = a[(i + 1) % a.len()];
        for j in 0..b.len() {
            let b0 = b[j];
            let b1 = b[(j + 1) % b.len()];
            if same(a0, b1) && same(a1, b0) {
                if join.is_some() {
                    // More than one shared edge: joining would leave a hole.
                    return None;
                }
                join = Some((i, j));
            }
        }
    }
    let (i, j) = join?;

    // Walk a from the end of the shared edge, then b from the end of its own.
    let mut merged: Vec<[f64; 3]> = Vec::with_capacity(a.len() + b.len());
    for step in 1..a.len() {
        merged.push(a[(i + 1 + step) % a.len()]);
    }
    for step in 1..b.len() {
        merged.push(b[(j + 1 + step) % b.len()]);
    }

    // Convexity: every turn must keep the same sign against the face normal.
    // Collinear turns are allowed and their vertex is retained, because a
    // perpendicular neighbour still meets the boundary there.
    for index in 0..merged.len() {
        let previous = merged[(index + merged.len() - 1) % merged.len()];
        let current = merged[index];
        let next = merged[(index + 1) % merged.len()];
        let edge = sub(current, previous);
        let turn = cross(edge, sub(next, current));
        if dot(turn, normal) < -CONTINUOUS_EPSILON {
            return None;
        }
    }
    Some(merged)
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let requested: Vec<String> = env::args().skip(1).collect();
    let pak_bytes = fs::read(root.join(".quakepsx/cache/shareware/ID1/PAK0.PAK"))?;
    let pak = PakArchive::parse(&pak_bytes)?;

    let mut total_before = 0usize;
    let mut total_after = 0usize;
    for name in MAPS {
        if !requested.is_empty() && !requested.iter().any(|entry| entry == name) {
            continue;
        }
        let bsp = Bsp::parse(pak.require(&format!("maps/{name}.bsp"))?)?;
        let planes = bsp.lump(BspLump::Planes);
        let vertices = bsp.lump(BspLump::Vertices);
        let edges = bsp.lump(BspLump::Edges);
        let surfedges = bsp.lump(BspLump::SurfaceEdges);
        let face_bytes = bsp.lump(BspLump::Faces);

        let mut faces = Vec::new();
        for record in face_bytes.chunks_exact(20) {
            let plane = u16_at(record, 0);
            let side = i16_at(record, 2);
            let first_edge = i32_at(record, 4) as usize;
            let edge_count = i16_at(record, 8) as usize;
            let texinfo = u16_at(record, 10);
            let styles = [record[12], record[13], record[14], record[15]];
            let mut points = Vec::with_capacity(edge_count);
            for step in 0..edge_count {
                let surfedge = i32_at(surfedges, (first_edge + step) * 4);
                let (edge, reverse) = if surfedge >= 0 {
                    (surfedge as usize, false)
                } else {
                    ((-surfedge) as usize, true)
                };
                let a = u16_at(edges, edge * 4) as usize;
                let b = u16_at(edges, edge * 4 + 2) as usize;
                let index = if reverse { b } else { a };
                points.push([
                    f32_at(vertices, index * 12) as f64,
                    f32_at(vertices, index * 12 + 4) as f64,
                    f32_at(vertices, index * 12 + 8) as f64,
                ]);
            }
            faces.push(Face {
                plane,
                side,
                texinfo,
                styles,
                points,
                alive: true,
            });
        }

        let before = faces.len();
        let before_points: usize = faces.iter().map(|face| face.points.len()).sum();

        // Group by the exact merge key so only real candidates are compared.
        let mut groups: HashMap<(u16, i16, u16, [u8; 4]), Vec<usize>> = HashMap::new();
        for (index, face) in faces.iter().enumerate() {
            groups
                .entry((face.plane, face.side, face.texinfo, face.styles))
                .or_default()
                .push(index);
        }

        let mut merges = 0usize;
        for members in groups.values() {
            let mut changed = true;
            while changed {
                changed = false;
                for a_pos in 0..members.len() {
                    let a = members[a_pos];
                    if !faces[a].alive {
                        continue;
                    }
                    for b_pos in (a_pos + 1)..members.len() {
                        let b = members[b_pos];
                        if !faces[b].alive {
                            continue;
                        }
                        let normal = {
                            let record = &planes[faces[a].plane as usize * 20..];
                            let sign = if faces[a].side != 0 { -1.0 } else { 1.0 };
                            [
                                f32_at(record, 0) as f64 * sign,
                                f32_at(record, 4) as f64 * sign,
                                f32_at(record, 8) as f64 * sign,
                            ]
                        };
                        let Some(merged) =
                            try_merge(&faces[a].points, &faces[b].points, normal)
                        else {
                            continue;
                        };
                        faces[a].points = merged;
                        faces[b].alive = false;
                        merges += 1;
                        changed = true;
                    }
                }
            }
        }

        let after = faces.iter().filter(|face| face.alive).count();
        let after_points: usize = faces
            .iter()
            .filter(|face| face.alive)
            .map(|face| face.points.len())
            .sum();
        total_before += before;
        total_after += after;
        println!(
            "{name}: faces {before} -> {after} (-{removed}, {share:.1}%); merges={merges}; \
             corners {before_points} -> {after_points} ({corner_share:+.1}%); \
             fan triangles {tri_before} -> {tri_after} ({tri_share:+.1}%)",
            removed = before - after,
            share = 100.0 * (before - after) as f64 / before as f64,
            corner_share = 100.0 * (after_points as f64 - before_points as f64)
                / before_points as f64,
            tri_before = before_points - 2 * before,
            tri_after = after_points - 2 * after,
            tri_share = 100.0
                * ((after_points - 2 * after) as f64 - (before_points - 2 * before) as f64)
                / (before_points - 2 * before) as f64,
        );
    }
    if total_before > 0 {
        println!(
            "episode: faces {total_before} -> {total_after} ({share:.1}% removed)",
            share = 100.0 * (total_before - total_after) as f64 / total_before as f64,
        );
    }
    Ok(())
}
