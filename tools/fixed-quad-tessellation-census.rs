//! Memory and packet-footprint census for cooker-side fixed-grid tessellation.
//!
//! Quake II PSX keeps fixed resident GT4 sources and caches their rare
//! subdivision leaves. Quake-PSX currently creates subdivision vertices and
//! packets every frame. Before changing PSB5, this tool measures the other
//! extreme: clip every ordinary Quake polygon to texture-space cells at cook
//! time, then submit only fixed GT4/GT3 fans.

use quake_cook::{Bsp, BspLump, PakArchive};
use quake_formats::resident::{MapLoadError, ResidentMap};
use quake_formats::{SliceReader, RESIDENT_MAP_ARENA_BYTES};
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];
const CELL_SIZES: [f32; 3] = [64.0, 128.0, 256.0];

#[derive(Clone, Copy, Debug)]
struct FaceVertex {
    position: [f32; 3],
    texture: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default)]
struct GridDelta {
    ordinary_faces: usize,
    added_faces: usize,
    added_corners: usize,
    added_positions: usize,
    added_marks: usize,
    added_packet_bytes: usize,
    resident_delta: usize,
    projected_resident_bytes: usize,
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16> {
    Ok(i16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("truncated i16")?
            .try_into()?,
    ))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("truncated u16")?
            .try_into()?,
    ))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("truncated i32")?
            .try_into()?,
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32> {
    Ok(f32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("truncated f32")?
            .try_into()?,
    ))
}

fn record<'a>(bytes: &'a [u8], size: usize, index: usize) -> Result<&'a [u8]> {
    bytes
        .get(
            index.checked_mul(size).ok_or("record offset overflow")?
                ..(index + 1).checked_mul(size).ok_or("record end overflow")?,
        )
        .ok_or_else(|| "record is out of bounds".into())
}

fn ordinary_texture(name: Option<&str>) -> bool {
    name.is_some_and(|name| {
        !name.starts_with('*')
            && !name.starts_with("sky")
            && !name.starts_with("clip")
            && !name.starts_with("trigger")
    })
}

fn interpolate(a: FaceVertex, b: FaceVertex, amount: f32) -> FaceVertex {
    let mut output = a;
    for axis in 0..3 {
        output.position[axis] = a.position[axis] + (b.position[axis] - a.position[axis]) * amount;
    }
    for axis in 0..2 {
        output.texture[axis] = a.texture[axis] + (b.texture[axis] - a.texture[axis]) * amount;
    }
    output
}

fn clip_axis(
    polygon: &[FaceVertex],
    axis: usize,
    boundary: f32,
    keep_lower: bool,
) -> Vec<FaceVertex> {
    let mut output = Vec::with_capacity(polygon.len() + 1);
    let Some(mut previous) = polygon.last().copied() else {
        return output;
    };
    let inside = |vertex: FaceVertex| {
        if keep_lower {
            vertex.texture[axis] <= boundary
        } else {
            vertex.texture[axis] >= boundary
        }
    };
    let mut previous_inside = inside(previous);
    for &current in polygon {
        let current_inside = inside(current);
        if current_inside != previous_inside {
            let denominator = current.texture[axis] - previous.texture[axis];
            if denominator.abs() > f32::EPSILON {
                output.push(interpolate(
                    previous,
                    current,
                    (boundary - previous.texture[axis]) / denominator,
                ));
            }
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    output
}

fn tessellate(polygon: Vec<FaceVertex>, cell_size: f32) -> Vec<Vec<FaceVertex>> {
    let mut polygons = vec![polygon];
    for axis in 0..2 {
        let minimum = polygons[0]
            .iter()
            .map(|vertex| vertex.texture[axis])
            .fold(f32::MAX, f32::min);
        let maximum = polygons[0]
            .iter()
            .map(|vertex| vertex.texture[axis])
            .fold(f32::MIN, f32::max);
        let mut boundary = (minimum / cell_size).floor() * cell_size + cell_size;
        while boundary < maximum - 0.001 {
            let mut split = Vec::with_capacity(polygons.len() * 2);
            for current in polygons {
                let lower = clip_axis(&current, axis, boundary, true);
                let upper = clip_axis(&current, axis, boundary, false);
                if lower.len() >= 3 {
                    split.push(lower);
                }
                if upper.len() >= 3 {
                    split.push(upper);
                }
            }
            polygons = split;
            boundary += cell_size;
        }
    }
    polygons
}

fn fan_packet_bytes(corners: usize) -> usize {
    let triangles = corners.saturating_sub(2);
    triangles / 2 * 52 + (triangles & 1) * 40
}

fn source_face(bsp: &Bsp<'_>, face: &[u8]) -> Result<(bool, Vec<FaceVertex>)> {
    let first_edge = usize::try_from(read_i32(face, 4)?)?;
    let corner_count = usize::try_from(read_i16(face, 8)?)?;
    let texture_info_index = usize::try_from(read_i16(face, 10)?)?;
    let texture_info = record(bsp.lump(BspLump::TextureInfo), 40, texture_info_index)?;
    let texture_index = usize::try_from(read_i32(texture_info, 32)?)?;
    let name = bsp.mip_texture(texture_index)?.map(|texture| texture.name);

    let mut polygon = Vec::with_capacity(corner_count);
    for edge_offset in 0..corner_count {
        let surface_edge = read_i32(
            bsp.lump(BspLump::SurfaceEdges),
            (first_edge + edge_offset) * 4,
        )?;
        let edge = record(
            bsp.lump(BspLump::Edges),
            4,
            surface_edge.unsigned_abs() as usize,
        )?;
        let vertex_index = if surface_edge >= 0 {
            read_u16(edge, 0)?
        } else {
            read_u16(edge, 2)?
        } as usize;
        let vertex = record(bsp.lump(BspLump::Vertices), 12, vertex_index)?;
        let position = [
            read_f32(vertex, 0)?,
            read_f32(vertex, 4)?,
            read_f32(vertex, 8)?,
        ];
        let mut texture = [0.0; 2];
        for axis in 0..2 {
            let base = axis * 16;
            texture[axis] = position[0] * read_f32(texture_info, base)?
                + position[1] * read_f32(texture_info, base + 4)?
                + position[2] * read_f32(texture_info, base + 8)?
                + read_f32(texture_info, base + 12)?;
        }
        polygon.push(FaceVertex { position, texture });
    }
    Ok((ordinary_texture(name), polygon))
}

fn required_resident_bytes(bytes: &[u8]) -> Result<usize> {
    let mut resident = ResidentMap::with_capacity(0);
    let mut reader = SliceReader::new(bytes);
    match resident.load(1, &mut reader) {
        Err(MapLoadError::TooLarge { required, .. }) => Ok(required),
        Err(error) => Err(format!("cannot size resident map: {error:?}").into()),
        Ok(()) => Ok(0),
    }
}

fn grid_delta(bsp: &Bsp<'_>, cooked: &[u8], cell_size: f32) -> Result<GridDelta> {
    let required = required_resident_bytes(cooked)?;
    let mut resident = ResidentMap::with_capacity(required);
    resident
        .load(1, &mut SliceReader::new(cooked))
        .map_err(|error| format!("cannot load resident map: {error:?}"))?;
    let indexed = resident
        .indexed_vertices()
        .ok_or("map is not indexed PSB5")?;
    let current_positions = indexed
        .positions
        .iter()
        .map(|position| position.position)
        .collect::<BTreeSet<_>>();
    let mut all_positions = current_positions.clone();
    let mut pieces_per_source = Vec::new();
    let mut delta = GridDelta::default();

    for face in bsp.lump(BspLump::Faces).chunks_exact(20) {
        let (ordinary, polygon) = source_face(bsp, face)?;
        if !ordinary {
            pieces_per_source.push(1usize);
            continue;
        }
        delta.ordinary_faces += 1;
        let source_corners = polygon.len();
        let source_packet_bytes = fan_packet_bytes(source_corners);
        let pieces = tessellate(polygon, cell_size);
        pieces_per_source.push(pieces.len());
        delta.added_faces += pieces.len().saturating_sub(1);
        let piece_corners = pieces.iter().map(Vec::len).sum::<usize>();
        delta.added_corners += piece_corners.saturating_sub(source_corners);
        delta.added_packet_bytes += pieces
            .iter()
            .map(|piece| fan_packet_bytes(piece.len()))
            .sum::<usize>()
            .saturating_sub(source_packet_bytes);
        for vertex in pieces.into_iter().flatten() {
            all_positions.insert(vertex.position.map(|value| value.round() as i16));
        }
    }

    for mark in bsp.lump(BspLump::MarkSurfaces).chunks_exact(2) {
        let source_face = u16::from_le_bytes([mark[0], mark[1]]) as usize;
        delta.added_marks += pieces_per_source
            .get(source_face)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
    }
    delta.added_positions = all_positions.len().saturating_sub(current_positions.len());
    delta.resident_delta = delta.added_faces * 10
        + delta.added_corners * 8
        + delta.added_positions * 6
        + delta.added_marks * 2;
    delta.projected_resident_bytes = required + delta.resident_delta;
    Ok(delta)
}

fn print_map(map: &str, bsp: &Bsp<'_>, cooked: &[u8]) -> Result<()> {
    for cell in CELL_SIZES {
        let delta = grid_delta(bsp, cooked, cell)?;
        println!(
            "| {map} | {} | {} | {} | {} | {} | {} | {} | {:+} | {} | {} |",
            cell as usize,
            delta.ordinary_faces,
            delta.added_faces,
            delta.added_corners,
            delta.added_positions,
            delta.added_marks,
            delta.added_packet_bytes / 1024,
            delta.resident_delta as isize,
            delta.projected_resident_bytes,
            RESIDENT_MAP_ARENA_BYTES as isize - delta.projected_resident_bytes as isize,
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut arguments = env::args_os().skip(1);
    let maps_dir = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("id1psx/maps"));
    let pak_path = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".quakepsx/cache/shareware/ID1/PAK0.PAK"));
    let pak_bytes = fs::read(&pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    println!("# Fixed-quad offline tessellation census");
    println!();
    println!("Resident delta includes PSB5 face, indexed-corner, deduplicated-position and expanded mark-surface records; packet delta is the map-global immutable GT3/GT4 base footprint.");
    println!();
    println!("| map | cell | ordinary faces | +faces | +corners | +positions | +marks | +base packets KiB | resident delta bytes | projected resident | arena headroom |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for map in MAPS {
        let source = Bsp::parse(pak.require(&format!("maps/{map}.bsp"))?)?;
        let cooked = fs::read(maps_dir.join(format!("{map}.psb")))?;
        print_map(map, &source, &cooked)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_splits_into_four_grid_cells() {
        let point = |x, y| FaceVertex {
            position: [x, y, 0.0],
            texture: [x, y],
        };
        let pieces = tessellate(
            vec![
                point(0.0, 0.0),
                point(128.0, 0.0),
                point(128.0, 128.0),
                point(0.0, 128.0),
            ],
            64.0,
        );
        assert_eq!(pieces.len(), 4);
        assert!(pieces.iter().all(|piece| piece.len() == 4));
    }
}
