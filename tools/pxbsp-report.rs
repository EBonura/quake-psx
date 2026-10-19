//! Host-only check and size table for the PXBSP form of each Episode 1 map.
//!
//! Reads the cooked `id1psx/maps/<map>.psb` and `<map>.pxbsp` pairs that the
//! `assets` action writes, loads both through the shared resident-map readers,
//! proves the two files describe one map (every shared lump equal, planes and
//! nodes equal after decoding), and prints the resident size of each against
//! the guest's resident-map arena.
//!
//! ```sh
//! cargo run --release --manifest-path host/quake-build/Cargo.toml --bin pxbsp-report
//! ```

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use quake_formats::pxbsp::{PxbspIndex, PxbspLumpKind};
use quake_formats::pxbsp_resident::PxbspResidentMap;
use quake_formats::quake_pxbsp::texture_table_records;
use quake_formats::render::PXBSP_MAX_FACE_VERTICES;
use quake_formats::resident::ResidentMap;
use quake_formats::{
    CookedRecord, LumpKind, MapEntity, PsbIndex, SliceReader, RESIDENT_MAP_ARENA_BYTES,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

fn lump<'a>(bytes: &'a [u8], range: quake_formats::LumpRange) -> &'a [u8] {
    &bytes[range.offset as usize..range.offset as usize + range.len as usize]
}

fn check(condition: bool, map: &str, what: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(format!("{map}: {what}").into())
    }
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("built from <repo>/host/quake-build")
        .to_path_buf();
    let requested: Vec<String> = env::args().skip(1).collect();

    println!(
        "| map | psb file | pxbsp file | XBSP resident | PXBSP resident | delta | vs arena {} | faces | corners | positions | nodes | max face verts | oversize faces |",
        RESIDENT_MAP_ARENA_BYTES
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for map in MAPS {
        if !requested.is_empty() && !requested.iter().any(|name| name == map) {
            continue;
        }
        let psb = fs::read(root.join(format!("id1psx/maps/{map}.psb")))?;
        let pxbsp = fs::read(root.join(format!("id1psx/maps/{map}.pxbsp")))?;

        let psb_index = PsbIndex::read(&mut SliceReader::new(&psb))
            .map_err(|error| format!("{map}.psb: {error:?}"))?;
        let mut xbsp = ResidentMap::with_capacity(4_000_000);
        xbsp.load(1, &mut SliceReader::new(&psb))
            .map_err(|error| format!("{map}.psb load: {error:?}"))?;
        let mut shared = PxbspResidentMap::with_capacity(4_000_000);
        shared
            .load(1, &mut SliceReader::new(&pxbsp))
            .map_err(|error| format!("{map}.pxbsp load: {error}"))?;
        let index = PxbspIndex::read(&mut SliceReader::new(&pxbsp))
            .map_err(|error| format!("{map}.pxbsp index: {error:?}"))?;

        // Lumps the two containers share byte for byte.
        for (psb_kind, pxbsp_kind) in [
            (LumpKind::TextureData, PxbspLumpKind::TextureData),
            (LumpKind::SoundData, PxbspLumpKind::SoundData),
            (LumpKind::ModelData, PxbspLumpKind::ModelData),
            (LumpKind::Faces, PxbspLumpKind::Faces),
            (LumpKind::MarkSurfaces, PxbspLumpKind::MarkSurfaces),
            (LumpKind::Visibility, PxbspLumpKind::Visibility),
            (LumpKind::Leaves, PxbspLumpKind::Leaves),
            (LumpKind::ClipNodes, PxbspLumpKind::ClipNodes),
            (LumpKind::Models, PxbspLumpKind::Models),
        ] {
            check(
                lump(&psb, psb_index.lump(psb_kind)) == lump(&pxbsp, index.lump(pxbsp_kind)),
                map,
                &format!("{psb_kind:?} lump differs between the two files"),
            )?;
        }

        // Planes: the PXBSP record is the PSB5 one with a byte class and sign bits.
        let planes = xbsp.planes();
        check(planes.len() == shared.planes().len(), map, "plane count")?;
        for (index, (old, new)) in planes.iter().zip(shared.planes()).enumerate() {
            check(
                old.normal == new.normal && old.distance == new.distance && old.kind == new.kind as i32,
                map,
                &format!("plane {index} differs"),
            )?;
        }

        // Vertices: every face corner has the same position, UV and light.
        let corners = xbsp
            .indexed_vertices()
            .ok_or_else(|| format!("{map}: PSB5 map without indexed vertices"))?;
        check(
            corners.corners.len() == shared.vertices().len(),
            map,
            "corner count",
        )?;
        for (index, (old, new)) in corners.corners.iter().zip(shared.vertices().iter()).enumerate() {
            let position = corners.positions[old.position_index as usize].position;
            check(
                position == [new.position.x, new.position.y, new.position.z]
                    && old.texture == [new.texture.x, new.texture.y]
                    && old.light == new.light,
                map,
                &format!("corner {index} differs"),
            )?;
        }

        // Nodes: the old node is the new node's plane and children.
        check(xbsp.nodes().len() == shared.nodes().len(), map, "node count")?;
        let mut covered_faces = 0usize;
        for (index, (old, new)) in xbsp.nodes().iter().zip(shared.nodes().iter()).enumerate() {
            check(
                old.plane == new.plane && old.children == new.children,
                map,
                &format!("node {index} differs"),
            )?;
            check(
                new.mins.x <= new.maxs.x && new.mins.y <= new.maxs.y && new.mins.z <= new.maxs.z,
                map,
                &format!("node {index} has inverted bounds"),
            )?;
            covered_faces += new.face_count as usize;
        }
        let face_count = shared.faces().len();
        // Every cooked face belongs to exactly one node, except faces the BSP
        // gives to submodels only (their nodes are model-local).
        check(covered_faces <= face_count, map, "node face ranges overlap")?;

        // Materials and the texture table are one-per-texture.
        let texture_count = xbsp.textures().len();
        check(shared.materials().len() == texture_count, map, "material count")?;
        let table = texture_table_records(shared.strings())
            .ok_or_else(|| format!("{map}: missing texture table sidecar"))?;
        check(
            table == xbsp_texture_bytes(&psb, &psb_index),
            map,
            "texture table differs from the TextureInfo lump",
        )?;

        // Entities: each Quake record survives as its payload.
        let entities = shared.entities();
        let quake_entities = lump(&psb, psb_index.lump(LumpKind::Entities));
        check(
            entities.len() == quake_entities.len() / MapEntity::SIZE,
            map,
            "entity count",
        )?;
        for index in 0..entities.len() {
            check(
                entities.payload(index)
                    == Some(&quake_entities[index * MapEntity::SIZE..(index + 1) * MapEntity::SIZE]),
                map,
                &format!("entity {index} payload"),
            )?;
        }

        let mut max_vertices = 0usize;
        let mut oversize = 0usize;
        for face in shared.faces().iter() {
            let count = face.vertex_count as usize;
            max_vertices = max_vertices.max(count);
            oversize += usize::from(count > PXBSP_MAX_FACE_VERTICES);
        }
        if env::var_os("PXBSP_LUMPS").is_some() {
            println!("\n{map} lump bytes (PSB5 | PXBSP):");
            for (name, psb_kind, pxbsp_kind) in [
                ("model data", LumpKind::ModelData, PxbspLumpKind::ModelData),
                ("vertices", LumpKind::Vertices, PxbspLumpKind::Vertices),
                ("planes", LumpKind::Planes, PxbspLumpKind::Planes),
                ("texture info | materials", LumpKind::TextureInfo, PxbspLumpKind::Materials),
                ("faces", LumpKind::Faces, PxbspLumpKind::Faces),
                ("mark surfaces", LumpKind::MarkSurfaces, PxbspLumpKind::MarkSurfaces),
                ("visibility", LumpKind::Visibility, PxbspLumpKind::Visibility),
                ("leaves", LumpKind::Leaves, PxbspLumpKind::Leaves),
                ("nodes", LumpKind::Nodes, PxbspLumpKind::Nodes),
                ("clip nodes", LumpKind::ClipNodes, PxbspLumpKind::ClipNodes),
                ("models", LumpKind::Models, PxbspLumpKind::Models),
                ("strings", LumpKind::Strings, PxbspLumpKind::Strings),
                ("entities", LumpKind::Entities, PxbspLumpKind::Entities),
            ] {
                println!(
                    "  {name}: {} | {}",
                    psb_index.lump(psb_kind).len,
                    index.lump(pxbsp_kind).len
                );
            }
        }
        let old_resident = xbsp.resident_bytes_len();
        let new_resident = shared.resident_bytes();
        println!(
            "| {map} | {} | {} | {old_resident} | {new_resident} | {:+} | {:+} | {face_count} | {} | {} | {} | {max_vertices} | {oversize} |",
            psb.len(),
            pxbsp.len(),
            new_resident as i64 - old_resident as i64,
            new_resident as i64 - RESIDENT_MAP_ARENA_BYTES as i64,
            corners.corners.len(),
            corners.positions.len(),
            shared.nodes().len(),
        );
    }
    println!(
        "\nlimit for a drawn face: {PXBSP_MAX_FACE_VERTICES} vertices (the shared renderer skips wider faces)"
    );
    Ok(())
}

fn xbsp_texture_bytes<'a>(psb: &'a [u8], index: &PsbIndex) -> &'a [u8] {
    lump(psb, index.lump(LumpKind::TextureInfo))
}
