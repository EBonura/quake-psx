//! A cooked Quake map as a PXBSP v6 file.
//!
//! The container is PSoXide's shared world format, read by
//! `psx_bsp::pxbsp_resident::PxbspResidentMap` and drawn by `psx_bsp::render`.
//! A Quake map keeps its Quake-space (Z-up) coordinates: the world is data
//! plus a view, so nothing is rotated or mirrored here. What Quake adds to the
//! container (map entities, the per-texture table) is described in
//! `quake_formats::quake_pxbsp`.

use quake_formats::pxbsp::{
    PxbspEntity, PXBSP_DIRECTORY_ENTRY_BYTES, PXBSP_ENTITY_TABLE_HEADER_BYTES, PXBSP_HEADER_BYTES,
    PXBSP_LUMP_COUNT, PXBSP_MAGIC, PXBSP_VERSION,
};
use quake_formats::pxbsp::PxbspLumpKind;
use quake_formats::quake_pxbsp::{
    ENTITY_CLASS, TEXTURE_TABLE_RECORD_BYTES, TEXTURE_TABLE_TRAILER_MAGIC,
};
use quake_formats::{CookedRecord, MapEntity};

use super::{CookError, CookedEntities, CookedModels, GeometryLumps};

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

/// Assemble the PXBSP file for one map from the lumps `cook_map` already built.
pub fn assemble_pxbsp(
    geometry: &GeometryLumps,
    models: &CookedModels,
    entities: &CookedEntities,
    sounds: &[u8],
) -> Result<Vec<u8>, CookError> {
    let pxbsp = &geometry.pxbsp;

    let mut strings = entities.strings.clone();
    strings.resize(align_up_4(strings.len()), 0);
    strings.extend_from_slice(&pxbsp.texture_table);
    let texture_count = u16::try_from(pxbsp.texture_table.len() / TEXTURE_TABLE_RECORD_BYTES)
        .map_err(|_| CookError::new("texture table exceeds u16"))?;
    strings.extend_from_slice(&TEXTURE_TABLE_TRAILER_MAGIC.to_le_bytes());
    strings.extend_from_slice(&texture_count.to_le_bytes());
    strings.extend_from_slice(&(TEXTURE_TABLE_RECORD_BYTES as u16).to_le_bytes());

    let entity_table = pack_entities(geometry, &entities.entities)?;

    let mut lumps: [Vec<u8>; PXBSP_LUMP_COUNT] = core::array::from_fn(|_| Vec::new());
    lumps[PxbspLumpKind::TextureData as usize] = geometry.texture_data.clone();
    lumps[PxbspLumpKind::SoundData as usize] = sounds.to_vec();
    lumps[PxbspLumpKind::ModelData as usize] = models.data.clone();
    lumps[PxbspLumpKind::Vertices as usize] = pxbsp.vertices.clone();
    lumps[PxbspLumpKind::Planes as usize] = pxbsp.planes.clone();
    lumps[PxbspLumpKind::Materials as usize] = pxbsp.materials.clone();
    lumps[PxbspLumpKind::Faces as usize] = geometry.faces.clone();
    lumps[PxbspLumpKind::MarkSurfaces as usize] = geometry.mark_surfaces.clone();
    lumps[PxbspLumpKind::Visibility as usize] = geometry.visibility.clone();
    lumps[PxbspLumpKind::Leaves as usize] = geometry.leaves.clone();
    lumps[PxbspLumpKind::Nodes as usize] = pxbsp.nodes.clone();
    lumps[PxbspLumpKind::ClipNodes as usize] = geometry.clip_nodes.clone();
    lumps[PxbspLumpKind::Models as usize] = geometry.models.clone();
    lumps[PxbspLumpKind::Strings as usize] = strings;
    lumps[PxbspLumpKind::Entities as usize] = entity_table;
    write_pxbsp(&lumps)
}

fn write_pxbsp(lumps: &[Vec<u8>; PXBSP_LUMP_COUNT]) -> Result<Vec<u8>, CookError> {
    let directory_end =
        PXBSP_HEADER_BYTES as usize + PXBSP_DIRECTORY_ENTRY_BYTES as usize * PXBSP_LUMP_COUNT;
    let mut output = vec![0; directory_end];
    output[0..4].copy_from_slice(&PXBSP_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&PXBSP_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(PXBSP_LUMP_COUNT as u16).to_le_bytes());
    for (index, kind) in PxbspLumpKind::ALL.into_iter().enumerate() {
        output.resize(align_up_4(output.len()), 0);
        let offset = u32::try_from(output.len())
            .map_err(|_| CookError::new("PXBSP offset exceeds u32"))?;
        let bytes = &lumps[index];
        let len = u32::try_from(bytes.len())
            .map_err(|_| CookError::new(format!("{kind:?} lump exceeds u32")))?;
        output.extend_from_slice(bytes);
        let entry = PXBSP_HEADER_BYTES as usize + index * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
        output[entry..entry + 2].copy_from_slice(&(kind as u16).to_le_bytes());
        output[entry + 4..entry + 8].copy_from_slice(&offset.to_le_bytes());
        output[entry + 8..entry + 12].copy_from_slice(&len.to_le_bytes());
    }
    u32::try_from(output.len()).map_err(|_| CookError::new("PXBSP file exceeds u32"))?;
    Ok(output)
}

/// One `PxbspEntity` per Quake map entity; the payload is the unchanged
/// 50-byte record, so the game decodes it exactly as it does from a PSB5 map.
fn pack_entities(geometry: &GeometryLumps, quake: &[u8]) -> Result<Vec<u8>, CookError> {
    let entity_bytes = MapEntity::SIZE;
    let count = quake.len() / entity_bytes;
    if quake.len() % entity_bytes != 0 {
        return Err(CookError::new("entity lump is not a whole number of records"));
    }
    let count_u16 = u16::try_from(count).map_err(|_| CookError::new("entity count exceeds u16"))?;
    let records_end = PXBSP_ENTITY_TABLE_HEADER_BYTES + count * PxbspEntity::SIZE;
    let payload_start = align_up_4(records_end);
    let mut output = vec![0; payload_start];
    output[0..2].copy_from_slice(&count_u16.to_le_bytes());
    output[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
    output[4..8].copy_from_slice(&(payload_start as u32).to_le_bytes());
    let walker = LeafWalker::new(geometry)?;
    for (index, record) in quake.chunks_exact(entity_bytes).enumerate() {
        let entity = MapEntity::decode(record);
        let leaf = walker.leaf_of([entity.origin.x, entity.origin.y, entity.origin.z]);
        let at = PXBSP_ENTITY_TABLE_HEADER_BYTES + index * PxbspEntity::SIZE;
        let slot = &mut output[at..at + PxbspEntity::SIZE];
        slot[0..2].copy_from_slice(&ENTITY_CLASS.to_le_bytes());
        slot[2..4].copy_from_slice(&0u16.to_le_bytes());
        // Quake stores brush entity `*N` as -N and an alias model id as a
        // non-negative number; PXBSP's model field names brush models only.
        let model = if entity.model < 0 {
            entity.model.unsigned_abs()
        } else {
            u16::MAX
        };
        slot[4..6].copy_from_slice(&model.to_le_bytes());
        slot[6..8].copy_from_slice(&leaf.to_le_bytes());
        slot[8..12].copy_from_slice(&entity.origin.x.to_le_bytes());
        slot[12..16].copy_from_slice(&entity.origin.y.to_le_bytes());
        slot[16..20].copy_from_slice(&entity.origin.z.to_le_bytes());
        slot[20..22].copy_from_slice(&entity.angles.x.to_le_bytes());
        slot[22..24].copy_from_slice(&entity.angles.y.to_le_bytes());
        slot[24..26].copy_from_slice(&entity.angles.z.to_le_bytes());
        slot[26..30].copy_from_slice(&((index * entity_bytes) as u32).to_le_bytes());
        slot[30..32].copy_from_slice(&(entity_bytes as u16).to_le_bytes());
    }
    output.extend_from_slice(quake);
    Ok(output)
}

/// Point-in-leaf over the world render tree, the way Quake finds a leaf: the
/// front child when the point is strictly in front of the splitting plane.
struct LeafWalker<'a> {
    planes: &'a [u8],
    nodes: &'a [u8],
    root: i16,
}

impl<'a> LeafWalker<'a> {
    fn new(geometry: &'a GeometryLumps) -> Result<Self, CookError> {
        let model = geometry
            .models
            .get(0..32)
            .ok_or_else(|| CookError::new("map has no world model"))?;
        Ok(Self {
            planes: &geometry.pxbsp.planes,
            nodes: &geometry.pxbsp.nodes,
            root: i16::from_le_bytes([model[18], model[19]]),
        })
    }

    /// Leaf index of a Q20.12 point. A malformed tree yields leaf 0.
    fn leaf_of(&self, point: [i32; 3]) -> u16 {
        let mut node = self.root;
        for _ in 0..self.nodes.len() / 16 + 1 {
            if node < 0 {
                return (-1 - i32::from(node)) as u16;
            }
            let Some(record) = self.nodes.get(node as usize * 16..node as usize * 16 + 16) else {
                return 0;
            };
            let plane_index = u16::from_le_bytes([record[0], record[1]]) as usize;
            let Some(plane) = self.planes.get(plane_index * 12..plane_index * 12 + 12) else {
                return 0;
            };
            let mut dot = 0i64;
            for axis in 0..3 {
                let normal = i16::from_le_bytes([plane[axis * 2], plane[axis * 2 + 1]]);
                dot += (i64::from(point[axis]) * i64::from(normal)) >> 12;
            }
            let distance = i32::from_le_bytes(plane[8..12].try_into().unwrap());
            let side = usize::from(dot - i64::from(distance) <= 0);
            node = i16::from_le_bytes([record[2 + side * 2], record[3 + side * 2]]);
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::pxbsp_resident::PxbspResidentMap;
    use quake_formats::quake_pxbsp::texture_table_records;

    /// A one-node, two-leaf, one-face "map": a floor at z = 0 seen from above.
    fn tiny_geometry() -> GeometryLumps {
        let mut geometry = GeometryLumps::default();
        // Four corners of a 64x64 square at z = 0.
        for (x, y) in [(0i16, 0i16), (64, 0), (64, 64), (0, 64)] {
            geometry.pxbsp.vertices.extend_from_slice(&x.to_le_bytes());
            geometry.pxbsp.vertices.extend_from_slice(&y.to_le_bytes());
            geometry.pxbsp.vertices.extend_from_slice(&0i16.to_le_bytes());
            geometry.pxbsp.vertices.extend_from_slice(&[0, 0]);
            geometry.pxbsp.vertices.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Plane z = 0, normal +z (Q3.12), axial class 2.
        for value in [0i16, 0, 4096] {
            geometry.pxbsp.planes.extend_from_slice(&value.to_le_bytes());
        }
        geometry.pxbsp.planes.extend_from_slice(&[2, 0]);
        geometry.pxbsp.planes.extend_from_slice(&0i32.to_le_bytes());
        // One material, one texture record.
        geometry.pxbsp.materials = vec![0, 0, 0, 0, 128, 128, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        geometry.pxbsp.texture_table = vec![0; TEXTURE_TABLE_RECORD_BYTES];
        // Node 0: plane 0, front = leaf 1 (-2), back = leaf 0 (-1), face 0.
        geometry.pxbsp.nodes.extend_from_slice(&0u16.to_le_bytes());
        geometry.pxbsp.nodes.extend_from_slice(&(-2i16).to_le_bytes());
        geometry.pxbsp.nodes.extend_from_slice(&(-1i16).to_le_bytes());
        geometry.pxbsp.nodes.extend_from_slice(&[0xff, 0xff, 0xff, 2, 2, 2]);
        geometry.pxbsp.nodes.extend_from_slice(&0u16.to_le_bytes());
        geometry.pxbsp.nodes.extend_from_slice(&1u16.to_le_bytes());
        // Face: plane 0, first vertex 0, material 0, flags 0, 4 corners.
        geometry.faces = vec![0, 0, 0, 0, 0, 0, 0, 4, 0, 0];
        geometry.mark_surfaces = vec![0, 0];
        geometry.visibility = vec![0b0000_0001];
        // Leaf 0 solid (no marks), leaf 1 empty (-1) seeing itself.
        let leaf = |contents: i8, marks: u16, vis: i32| {
            let mut bytes = vec![contents as u8, 0];
            bytes.extend_from_slice(&marks.to_le_bytes());
            bytes.extend_from_slice(&vis.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&[0, 0, 64, 64]);
            bytes
        };
        geometry.leaves = [leaf(-2, 0, -1), leaf(-1, 1, 0)].concat();
        geometry.clip_nodes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&0i16.to_le_bytes());
            bytes.extend_from_slice(&(-1i16).to_le_bytes());
            bytes.extend_from_slice(&(-2i16).to_le_bytes());
            bytes
        };
        // World model: head node 0 and three clip heads at clip node 0.
        let mut model = vec![0u8; 32];
        model[18..20].copy_from_slice(&0i16.to_le_bytes());
        for hull in 1..4 {
            model[18 + hull * 2..20 + hull * 2].copy_from_slice(&0i16.to_le_bytes());
        }
        model[26..28].copy_from_slice(&1i16.to_le_bytes());
        model[28..30].copy_from_slice(&0u16.to_le_bytes());
        model[30..32].copy_from_slice(&1u16.to_le_bytes());
        geometry.models = model;
        geometry
    }

    #[test]
    fn a_minimal_map_loads_in_the_shared_resident_map() {
        let geometry = tiny_geometry();
        let mut quake = vec![0u8; MapEntity::SIZE];
        quake[0] = 3;
        // Origin (10, 10, 8) units, Q20.12, above the floor: leaf 1.
        for (axis, units) in [10i32, 10, 8].into_iter().enumerate() {
            quake[38 + axis * 4..42 + axis * 4].copy_from_slice(&(units << 12).to_le_bytes());
        }
        let entities = CookedEntities {
            strings: b"hello\0".to_vec(),
            entities: quake.clone(),
            ..CookedEntities::default()
        };
        let bytes =
            assemble_pxbsp(&geometry, &CookedModels::default(), &entities, &[]).expect("assemble");
        let mut map = PxbspResidentMap::new();
        map.load(1, &mut quake_formats::SliceReader::new(&bytes))
            .expect("shared loader accepts the cooked map");
        assert_eq!(map.faces().len(), 1);
        assert_eq!(map.nodes().len(), 1);
        assert_eq!(map.materials().len(), 1);
        assert_eq!(map.entities().len(), 1);
        let entity = map.entities().get(0).unwrap();
        assert_eq!(entity.class_id, ENTITY_CLASS);
        assert_eq!(entity.leaf, 1, "an entity above the floor is in the empty leaf");
        assert_eq!(map.entities().payload(0), Some(&quake[..]));
        assert_eq!(map.string_at(0), Some(&b"hello"[..]));
        assert_eq!(
            texture_table_records(map.strings()).map(<[u8]>::len),
            Some(TEXTURE_TABLE_RECORD_BYTES)
        );
    }
}
