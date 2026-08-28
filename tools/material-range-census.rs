//! Inspect the cooked UV extent of Quake's windowed liquid faces.
//!
//! A face whose material-relative UVs fit inside one repeated page-local
//! allocation can use the compact packet shape without GP0(E2). This census
//! reports the smallest power-of-two allocation which contains each face.

use quake_formats::resident::{MapLoadError, ResidentMap};
use quake_formats::{
    liquid_alternate_texture, SliceReader, FACE_PAGE_LOCAL_UV, TEXTURE_LIQUID,
};
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    faces: usize,
    roots: usize,
}

fn class(required: [u16; 2]) -> usize {
    if required[0] <= 64 && required[1] <= 64 {
        0
    } else if required[0] <= 128 && required[1] <= 128 {
        1
    } else {
        2
    }
}

fn cyclic_required(mut coordinates: Vec<u8>) -> u16 {
    if coordinates.is_empty() {
        return 0;
    }
    coordinates.sort_unstable();
    coordinates.dedup();
    let mut largest_gap = 0u16;
    for pair in coordinates.windows(2) {
        largest_gap = largest_gap.max(u16::from(pair[1] - pair[0]));
    }
    largest_gap = largest_gap.max(256 - u16::from(*coordinates.last().unwrap())
        + u16::from(coordinates[0]));
    257 - largest_gap
}

fn main() -> Result<()> {
    let paths = env::args_os().skip(1).map(PathBuf::from).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("usage: material-range-census MAP.psb [MAP.psb ...]".into());
    }
    println!("| map | resident bytes | all faces | liquid tiles | liquid faces | fan roots | page-local marked | fits 64x64 | fits 128x128 | exceeds 128 |");
    println!("|---|---:|---:|---|---:|---:|---:|---:|---:|---:|");
    for path in paths {
        let bytes = fs::read(&path)?;
        let mut probe = ResidentMap::with_capacity(0);
        let required = match probe.load(1, &mut SliceReader::new(&bytes)) {
            Err(MapLoadError::TooLarge { required, .. }) => required,
            result => return Err(format!("cannot size {}: {result:?}", path.display()).into()),
        };
        // This is a diagnostic, so admit structurally larger candidates and
        // report their geometry even when the shipping 880 KiB gate rejects
        // them later.
        let mut resident = ResidentMap::with_capacity(1_100_000);
        resident
            .load(1, &mut SliceReader::new(&bytes))
            .map_err(|error| format!("cannot load {}: {error:?}", path.display()))?;
        let indexed = resident
            .indexed_vertices()
            .ok_or_else(|| format!("{} is not an indexed PSB5 map", path.display()))?;
        let textures = resident.textures();
        let liquid_tiles = textures
            .iter()
            .filter(|texture| texture.flags & TEXTURE_LIQUID != 0)
            .map(|texture| {
                let alternate = liquid_alternate_texture(texture).expect("validated liquid pair");
                format!(
                    "{:02x}:{},{}->{:02x}:{},{} {}x{}",
                    texture.texture_page,
                    texture.atlas.x,
                    texture.atlas.y,
                    alternate.texture_page,
                    alternate.atlas.x,
                    alternate.atlas.y,
                    texture.size.x * 2,
                    texture.size.y,
                )
            })
            .collect::<BTreeSet<_>>();
        let mut bins = [Counts::default(); 3];
        let mut marked = Counts::default();
        for face in resident.faces().iter() {
            let texture = textures
                .get(face.texture as usize)
                .expect("validated texture index");
            if texture.flags & TEXTURE_LIQUID == 0 {
                continue;
            }
            if u16::from(face.flags) & FACE_PAGE_LOCAL_UV != 0 {
                marked.faces += 1;
                marked.roots += face.vertex_count.saturating_sub(2) as usize;
            }
            let first = face.first_vertex as usize;
            let end = first + face.vertex_count as usize;
            let mut u = Vec::with_capacity(face.vertex_count as usize);
            let mut v = Vec::with_capacity(face.vertex_count as usize);
            for corner in &indexed.corners[first..end] {
                u.push(corner.texture[0]);
                v.push(corner.texture[1]);
            }
            let required = [cyclic_required(u), cyclic_required(v)];
            let bin = &mut bins[class(required)];
            bin.faces += 1;
            bin.roots += face.vertex_count.saturating_sub(2) as usize;
        }
        let total_faces = bins.iter().map(|bin| bin.faces).sum::<usize>();
        let total_roots = bins.iter().map(|bin| bin.roots).sum::<usize>();
        println!(
            "| {} | {} | {} | {} | {} | {} | {} ({}) | {} ({}) | {} ({}) | {} ({}) |",
            path.file_stem().and_then(|name| name.to_str()).unwrap_or("?"),
            required,
            resident.faces().len(),
            liquid_tiles.into_iter().collect::<Vec<_>>().join("; "),
            total_faces,
            total_roots,
            marked.faces,
            marked.roots,
            bins[0].faces,
            bins[0].roots,
            bins[1].faces,
            bins[1].roots,
            bins[2].faces,
            bins[2].roots,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cyclic_required;

    #[test]
    fn cyclic_extent_chooses_the_gap_across_u8_wrap() {
        assert_eq!(cyclic_required(vec![250, 2, 8]), 15);
        assert_eq!(cyclic_required(vec![0, 64]), 65);
        assert_eq!(cyclic_required(vec![0, 128]), 129);
    }
}
