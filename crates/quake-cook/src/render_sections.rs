//! Canonical host encoders for `QRS3` and its quad-native `QRP3` payloads.

use quake_formats::{
    RenderQuad, RenderQuadCommand, RenderQuadFace, RenderQuadObject, RenderQuadPayload,
    RenderQuadRun, RenderSectionDirectory, RENDER_QUAD_CELL_BYTES, RENDER_QUAD_COMMAND_BYTES,
    RENDER_QUAD_FACE_BYTES, RENDER_QUAD_HEADER_BYTES, RENDER_QUAD_OBJECT_BYTES,
    RENDER_QUAD_PACKET_BYTES, RENDER_QUAD_PAYLOAD_MAGIC, RENDER_QUAD_PAYLOAD_VERSION,
    RENDER_QUAD_POSITION_BYTES, RENDER_QUAD_PROJECTED_POSITION_BYTES, RENDER_QUAD_RECORD_BYTES,
    RENDER_QUAD_REFERENCE_BYTES, RENDER_QUAD_RUN_BYTES, RENDER_SECTION_EDGE_BYTES,
    RENDER_SECTION_HEADER_BYTES, RENDER_SECTION_MAGIC, RENDER_SECTION_NONE,
    RENDER_SECTION_RECORD_BYTES, RENDER_SECTION_VERSION,
};

use super::CookError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadCellInput {
    pub leaf: u16,
    pub flags: u16,
    pub commands: Vec<RenderQuadCommand>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadPayloadInput {
    pub objects: Vec<RenderQuadObject>,
    pub faces: Vec<RenderQuadFace>,
    pub quads: Vec<RenderQuad>,
    pub positions: Vec<[i16; 3]>,
    pub runs: Vec<RenderQuadRun>,
    pub cells: Vec<RenderQuadCellInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EncodedRenderQuadPayload {
    pub bytes: Vec<u8>,
    pub packet_pool_bytes: u32,
    pub projection_bytes: u32,
    pub runtime_metadata_bytes: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionInput {
    pub neighbors: Vec<u16>,
    pub payload: EncodedRenderQuadPayload,
    pub fallback_bytes: u32,
    pub flags: u16,
}

pub fn encode_render_quad_payload(
    input: &RenderQuadPayloadInput,
) -> Result<EncodedRenderQuadPayload, CookError> {
    let object_count = count_u16(input.objects.len(), "render object")?;
    let face_count = count_u16(input.faces.len(), "render face")?;
    let quad_count = count_u16(input.quads.len(), "render quad")?;
    let position_count = count_u16(input.positions.len(), "render position")?;
    let run_count = count_u16(input.runs.len(), "render material run")?;
    let cell_count = count_u16(input.cells.len(), "render cell")?;
    let packet_pool_bytes = byte_count_u32(
        input.quads.len(),
        RENDER_QUAD_PACKET_BYTES,
        "render packet pool",
    )?;
    let projection_bytes = byte_count_u32(
        input.positions.len(),
        RENDER_QUAD_PROJECTED_POSITION_BYTES,
        "render projection cache",
    )?;
    let command_count = input.cells.iter().try_fold(0usize, |total, cell| {
        count_u16(cell.commands.len(), "render cell command")?;
        total
            .checked_add(cell.commands.len())
            .ok_or_else(|| CookError::new("render cell command count overflow"))
    })?;

    let objects_offset = RENDER_QUAD_HEADER_BYTES;
    let objects_end = objects_offset + input.objects.len() * RENDER_QUAD_OBJECT_BYTES;
    let faces_offset = objects_end;
    let faces_end = faces_offset + input.faces.len() * RENDER_QUAD_FACE_BYTES;
    let quads_offset = faces_end;
    let quads_end = quads_offset + input.quads.len() * RENDER_QUAD_RECORD_BYTES;
    let positions_offset = quads_end;
    let positions_end = positions_offset + input.positions.len() * RENDER_QUAD_POSITION_BYTES;
    let runs_offset = align_up_4(positions_end);
    let runs_end = runs_offset + input.runs.len() * RENDER_QUAD_RUN_BYTES;
    let cells_offset = runs_end;
    let cells_end = cells_offset + input.cells.len() * RENDER_QUAD_CELL_BYTES;
    let streams_offset = cells_end;
    let file_bytes = streams_offset
        .checked_add(command_count * RENDER_QUAD_COMMAND_BYTES)
        .ok_or_else(|| CookError::new("quad-native payload exceeds address space"))?;
    let file_bytes_u32 =
        u32::try_from(file_bytes).map_err(|_| CookError::new("quad-native payload exceeds u32"))?;
    let runtime_metadata_bytes = input
        .objects
        .len()
        .checked_mul(RENDER_QUAD_OBJECT_BYTES)
        .and_then(|bytes| bytes.checked_add(input.faces.len() * RENDER_QUAD_FACE_BYTES))
        .and_then(|bytes| bytes.checked_add(input.quads.len() * RENDER_QUAD_REFERENCE_BYTES))
        .and_then(|bytes| bytes.checked_add(input.positions.len() * RENDER_QUAD_POSITION_BYTES))
        .and_then(|bytes| bytes.checked_add(input.cells.len() * RENDER_QUAD_CELL_BYTES))
        .and_then(|bytes| bytes.checked_add(command_count * RENDER_QUAD_COMMAND_BYTES))
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| CookError::new("quad-native runtime metadata exceeds u32"))?;
    let mut output = vec![0u8; file_bytes];
    output[0..4].copy_from_slice(&RENDER_QUAD_PAYLOAD_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_QUAD_PAYLOAD_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_QUAD_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&object_count.to_le_bytes());
    output[10..12].copy_from_slice(&face_count.to_le_bytes());
    output[12..14].copy_from_slice(&quad_count.to_le_bytes());
    output[14..16].copy_from_slice(&position_count.to_le_bytes());
    output[16..18].copy_from_slice(&run_count.to_le_bytes());
    output[18..20].copy_from_slice(&cell_count.to_le_bytes());
    for (offset, value) in [
        (20, objects_offset),
        (24, faces_offset),
        (28, quads_offset),
        (32, positions_offset),
        (36, runs_offset),
        (40, cells_offset),
        (44, streams_offset),
    ] {
        output[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    output[48..52].copy_from_slice(&file_bytes_u32.to_le_bytes());
    output[52..56].copy_from_slice(&packet_pool_bytes.to_le_bytes());
    output[56..60].copy_from_slice(&projection_bytes.to_le_bytes());
    output[60..64].copy_from_slice(&runtime_metadata_bytes.to_le_bytes());

    for (index, object) in input.objects.iter().enumerate() {
        let start = objects_offset + index * RENDER_QUAD_OBJECT_BYTES;
        output[start..start + 2].copy_from_slice(&object.first_face.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&object.face_count.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&object.first_quad.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&object.quad_count.to_le_bytes());
        output[start + 8..start + 10].copy_from_slice(&object.first_position.to_le_bytes());
        output[start + 10..start + 12].copy_from_slice(&object.position_count.to_le_bytes());
        output[start + 12..start + 14].copy_from_slice(&object.first_run.to_le_bytes());
        output[start + 14..start + 16].copy_from_slice(&object.run_count.to_le_bytes());
        for axis in 0..3 {
            output[start + 16 + axis * 2..start + 18 + axis * 2]
                .copy_from_slice(&object.mins[axis].to_le_bytes());
            output[start + 22 + axis * 2..start + 24 + axis * 2]
                .copy_from_slice(&object.maxs[axis].to_le_bytes());
        }
        output[start + 28..start + 30].copy_from_slice(&object.flags.to_le_bytes());
    }
    for (index, face) in input.faces.iter().enumerate() {
        let start = faces_offset + index * RENDER_QUAD_FACE_BYTES;
        output[start..start + 2].copy_from_slice(&face.source_face.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&face.first_quad.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&face.quad_count.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&face.plane.to_le_bytes());
        output[start + 8..start + 10].copy_from_slice(&face.material.to_le_bytes());
        output[start + 10..start + 12].copy_from_slice(&face.flags.to_le_bytes());
    }
    for (index, quad) in input.quads.iter().enumerate() {
        let start = quads_offset + index * RENDER_QUAD_RECORD_BYTES;
        output[start..start + 4].copy_from_slice(&quad.positions);
        for (word, value) in quad.invariant_words.iter().enumerate() {
            output[start + 4 + word * 4..start + 8 + word * 4]
                .copy_from_slice(&value.to_le_bytes());
        }
    }
    for (index, position) in input.positions.iter().enumerate() {
        let start = positions_offset + index * RENDER_QUAD_POSITION_BYTES;
        for axis in 0..3 {
            output[start + axis * 2..start + axis * 2 + 2]
                .copy_from_slice(&position[axis].to_le_bytes());
        }
    }
    for (index, run) in input.runs.iter().enumerate() {
        let start = runs_offset + index * RENDER_QUAD_RUN_BYTES;
        output[start..start + 2].copy_from_slice(&run.first_quad.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&run.quad_count.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&run.material.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&run.flags.to_le_bytes());
    }
    let mut stream_offset = streams_offset;
    for (index, cell) in input.cells.iter().enumerate() {
        let start = cells_offset + index * RENDER_QUAD_CELL_BYTES;
        output[start..start + 2].copy_from_slice(&cell.leaf.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&(cell.commands.len() as u16).to_le_bytes());
        output[start + 4..start + 8].copy_from_slice(&(stream_offset as u32).to_le_bytes());
        output[start + 8..start + 10].copy_from_slice(&cell.flags.to_le_bytes());
        for command in &cell.commands {
            output[stream_offset..stream_offset + 2].copy_from_slice(&command.object.to_le_bytes());
            output[stream_offset + 2..stream_offset + 4]
                .copy_from_slice(&command.flags.to_le_bytes());
            output[stream_offset + 4..stream_offset + 8]
                .copy_from_slice(&command.visible_faces.to_le_bytes());
            output[stream_offset + 8..stream_offset + 12]
                .copy_from_slice(&command.dynamic_faces.to_le_bytes());
            stream_offset += RENDER_QUAD_COMMAND_BYTES;
        }
    }
    debug_assert_eq!(stream_offset, output.len());
    RenderQuadPayload::parse(&output).map_err(|error| {
        CookError::new(format!("encoded quad-native payload is invalid: {error:?}"))
    })?;
    Ok(EncodedRenderQuadPayload {
        bytes: output,
        packet_pool_bytes,
        projection_bytes,
        runtime_metadata_bytes,
    })
}

pub fn encode_render_sections(
    leaf_sections: &[u16],
    sections: &[RenderSectionInput],
    resident_core_bytes: u32,
    streaming_budget_bytes: u32,
    gpu_arena_budget_bytes: u32,
) -> Result<Vec<u8>, CookError> {
    let leaf_count = count_u16(leaf_sections.len(), "render-section leaf")?;
    let section_count = count_u16(sections.len(), "render section")?;
    for &section in leaf_sections {
        if section != RENDER_SECTION_NONE && section >= section_count {
            return Err(CookError::new(
                "render-section leaf references a missing section",
            ));
        }
    }

    let mut neighbors = Vec::with_capacity(sections.len());
    let mut edge_count = 0usize;
    let mut payload_bytes = 0usize;
    let mut activation_bytes = Vec::with_capacity(sections.len());
    for (section_index, section) in sections.iter().enumerate() {
        let parsed = RenderQuadPayload::parse(&section.payload.bytes).map_err(|error| {
            CookError::new(format!(
                "render section {section_index} has invalid QRP3: {error:?}"
            ))
        })?;
        if parsed.packet_pool_bytes() != section.payload.packet_pool_bytes
            || parsed.projection_bytes() != section.payload.projection_bytes
            || parsed.runtime_metadata_bytes() != section.payload.runtime_metadata_bytes
        {
            return Err(CookError::new(
                "render-section derived memory accounting drifted",
            ));
        }
        let active = section
            .payload
            .runtime_metadata_bytes
            .checked_add(section.payload.projection_bytes)
            .ok_or_else(|| CookError::new("render-section activation size overflow"))?;
        if section
            .payload
            .packet_pool_bytes
            .checked_add(section.fallback_bytes)
            .is_none_or(|bytes| bytes > gpu_arena_budget_bytes)
        {
            return Err(CookError::new(
                "render section exceeds the existing GPU packet arena",
            ));
        }
        activation_bytes.push(active);
        let mut list = section.neighbors.clone();
        list.sort_unstable();
        list.dedup();
        if list.iter().any(|neighbor| {
            *neighbor as usize >= sections.len() || *neighbor as usize == section_index
        }) {
            return Err(CookError::new(
                "render-section neighbor references a missing or identical section",
            ));
        }
        edge_count = edge_count
            .checked_add(list.len())
            .ok_or_else(|| CookError::new("render-section edge count overflow"))?;
        payload_bytes = payload_bytes
            .checked_add(section.payload.bytes.len())
            .ok_or_else(|| CookError::new("render-section payload size overflow"))?;
        neighbors.push(list);
    }
    let edge_count_u16 = count_u16(edge_count, "render-section edge")?;

    let leaf_offset = RENDER_SECTION_HEADER_BYTES;
    let leaf_end = leaf_offset + leaf_sections.len() * 2;
    let section_offset = align_up_4(leaf_end);
    let section_end = section_offset + sections.len() * RENDER_SECTION_RECORD_BYTES;
    let edge_offset = section_end;
    let edge_end = edge_offset + edge_count * RENDER_SECTION_EDGE_BYTES;
    let payload_offset = align_up_4(edge_end);
    let file_bytes = payload_offset
        .checked_add(payload_bytes)
        .ok_or_else(|| CookError::new("render-section file size overflow"))?;
    let file_bytes_u32 =
        u32::try_from(file_bytes).map_err(|_| CookError::new("render sections exceed u32"))?;
    let mut output = vec![0u8; file_bytes];
    output[0..4].copy_from_slice(&RENDER_SECTION_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_SECTION_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_SECTION_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&leaf_count.to_le_bytes());
    output[10..12].copy_from_slice(&section_count.to_le_bytes());
    output[12..14].copy_from_slice(&edge_count_u16.to_le_bytes());
    output[14..16].copy_from_slice(&(RENDER_SECTION_RECORD_BYTES as u16).to_le_bytes());
    for (offset, value) in [
        (16, leaf_offset),
        (20, section_offset),
        (24, edge_offset),
        (28, payload_offset),
    ] {
        output[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    output[32..36].copy_from_slice(&file_bytes_u32.to_le_bytes());
    output[36..40].copy_from_slice(&resident_core_bytes.to_le_bytes());
    output[40..44].copy_from_slice(&streaming_budget_bytes.to_le_bytes());
    output[44..48].copy_from_slice(&gpu_arena_budget_bytes.to_le_bytes());
    for (leaf, section) in leaf_sections.iter().enumerate() {
        let start = leaf_offset + leaf * 2;
        output[start..start + 2].copy_from_slice(&section.to_le_bytes());
    }

    let mut first_edge = 0usize;
    let mut section_payload = payload_offset;
    for (section_index, section) in sections.iter().enumerate() {
        let start = section_offset + section_index * RENDER_SECTION_RECORD_BYTES;
        output[start..start + 2].copy_from_slice(&(first_edge as u16).to_le_bytes());
        output[start + 2..start + 4]
            .copy_from_slice(&(neighbors[section_index].len() as u16).to_le_bytes());
        output[start + 4..start + 8].copy_from_slice(&(section_payload as u32).to_le_bytes());
        output[start + 8..start + 12]
            .copy_from_slice(&(section.payload.bytes.len() as u32).to_le_bytes());
        output[start + 12..start + 16]
            .copy_from_slice(&activation_bytes[section_index].to_le_bytes());
        output[start + 16..start + 20]
            .copy_from_slice(&section.payload.packet_pool_bytes.to_le_bytes());
        output[start + 20..start + 24]
            .copy_from_slice(&section.payload.projection_bytes.to_le_bytes());
        output[start + 24..start + 28].copy_from_slice(&section.fallback_bytes.to_le_bytes());
        output[start + 28..start + 30].copy_from_slice(&section.flags.to_le_bytes());
        for (neighbor_index, neighbor) in neighbors[section_index].iter().enumerate() {
            let edge = edge_offset + (first_edge + neighbor_index) * RENDER_SECTION_EDGE_BYTES;
            output[edge..edge + 2].copy_from_slice(&neighbor.to_le_bytes());
        }
        first_edge += neighbors[section_index].len();
        section_payload += section.payload.bytes.len();
    }
    let mut destination = payload_offset;
    for section in sections {
        let end = destination + section.payload.bytes.len();
        output[destination..end].copy_from_slice(&section.payload.bytes);
        destination = end;
    }
    debug_assert_eq!(destination, output.len());
    RenderSectionDirectory::parse(&output).map_err(|error| {
        CookError::new(format!(
            "encoded render-section directory is invalid: {error:?}"
        ))
    })?;
    Ok(output)
}

fn count_u16(count: usize, name: &str) -> Result<u16, CookError> {
    u16::try_from(count).map_err(|_| CookError::new(format!("{name} count exceeds u16")))
}

fn byte_count_u32(count: usize, bytes: usize, name: &str) -> Result<u32, CookError> {
    count
        .checked_mul(bytes)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| CookError::new(format!("{name} size exceeds u32")))
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(seed: u32) -> EncodedRenderQuadPayload {
        encode_render_quad_payload(&RenderQuadPayloadInput {
            objects: vec![RenderQuadObject {
                first_face: 0,
                face_count: 1,
                first_quad: 0,
                quad_count: 1,
                first_position: 0,
                position_count: 4,
                first_run: 0,
                run_count: 1,
                mins: [-8, -4, 0],
                maxs: [8, 4, 0],
                flags: 0,
            }],
            faces: vec![RenderQuadFace {
                source_face: 3,
                first_quad: 0,
                quad_count: 1,
                plane: 5,
                material: 7,
                flags: quake_formats::RENDER_QUAD_FACE_BACKSIDE,
            }],
            quads: vec![RenderQuad {
                positions: [0, 1, 2, 3],
                invariant_words: core::array::from_fn(|index| seed + index as u32),
            }],
            positions: vec![[-8, -4, 0], [8, -4, 0], [-8, 4, 0], [8, 4, 0]],
            runs: vec![RenderQuadRun {
                first_quad: 0,
                quad_count: 1,
                material: 7,
                flags: 1,
            }],
            cells: vec![RenderQuadCellInput {
                leaf: 1,
                flags: 0,
                commands: vec![RenderQuadCommand {
                    object: 0,
                    flags: 0,
                    visible_faces: 1,
                    dynamic_faces: 1,
                }],
            }],
        })
        .unwrap()
    }

    #[test]
    fn quad_payload_roundtrips_exact_packet_and_projection_accounting() {
        let encoded = payload(0x1000);
        let parsed = RenderQuadPayload::parse(&encoded.bytes).unwrap();
        assert_eq!(parsed.object_count(), 1);
        assert_eq!(parsed.face_count(), 1);
        assert_eq!(parsed.quad_count(), 1);
        assert_eq!(parsed.position_count(), 4);
        assert_eq!(parsed.packet_pool_bytes(), 52);
        assert_eq!(parsed.projection_bytes(), 32);
        assert_eq!(parsed.runtime_metadata_bytes(), 96);
        assert_eq!(parsed.face(0).unwrap().plane, 5);
        assert_eq!(parsed.quad(0).unwrap().invariant_words[7], 0x1007);
        assert_eq!(
            parsed.command(parsed.cell(0).unwrap(), 0).unwrap().object,
            0
        );
    }

    #[test]
    fn quad_payload_rejects_dynamic_faces_outside_the_visible_mask() {
        let mut encoded = payload(0x1800);
        let stream = RenderQuadPayload::parse(&encoded.bytes)
            .unwrap()
            .streams_offset();
        encoded.bytes[stream + 8..stream + 12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            RenderQuadPayload::parse(&encoded.bytes),
            Err(quake_formats::RenderQuadPayloadError::BadFaceMask)
        );
    }

    #[test]
    fn directory_proves_active_plus_neighbor_preload_fits() {
        let sections = [
            RenderSectionInput {
                neighbors: vec![1],
                payload: payload(0x2000),
                fallback_bytes: 64,
                flags: 0,
            },
            RenderSectionInput {
                neighbors: vec![0],
                payload: payload(0x3000),
                fallback_bytes: 64,
                flags: 0,
            },
        ];
        let active = sections[0].payload.runtime_metadata_bytes as usize
            + sections[0].payload.projection_bytes as usize;
        let budget = active + sections[1].payload.bytes.len();
        let bytes = encode_render_sections(
            &[RENDER_SECTION_NONE, 0, 1],
            &sections,
            335_000,
            budget as u32,
            120 * 1024,
        )
        .unwrap();
        let directory = RenderSectionDirectory::parse(&bytes).unwrap();
        assert_eq!(directory.resident_core_bytes(), 335_000);
        assert_eq!(directory.streaming_budget_bytes(), budget as u32);
        assert_eq!(directory.gpu_arena_budget_bytes(), 120 * 1024);
        assert_eq!(directory.leaf_section(0), None);
        assert_eq!(directory.leaf_section(2), Some(1));
        assert_eq!(directory.edge(0).unwrap().neighbor, 1);
        let header = quake_formats::RenderSectionHeader::parse(
            &bytes[..RENDER_SECTION_HEADER_BYTES],
            bytes.len(),
        )
        .unwrap();
        let index = quake_formats::RenderSectionIndex::parse_prefix(
            &bytes[..header.directory_bytes()],
            header.file_bytes(),
        )
        .unwrap();
        assert_eq!(index.header().resident_core_bytes(), 335_000);
        assert_eq!(index.leaf_section(0), None);
        assert_eq!(index.leaf_section(2), Some(1));
        assert_eq!(index.section(1).unwrap(), directory.section(1).unwrap());
        assert_eq!(index.edge(0).unwrap().neighbor, 1);
        assert!(RenderQuadPayload::parse(
            directory.payload(directory.section(0).unwrap()).unwrap()
        )
        .is_ok());
    }

    #[test]
    fn directory_rejects_a_neighbor_preload_over_budget() {
        let sections = [
            RenderSectionInput {
                neighbors: vec![1],
                payload: payload(0x4000),
                ..RenderSectionInput::default()
            },
            RenderSectionInput {
                neighbors: vec![0],
                payload: payload(0x5000),
                ..RenderSectionInput::default()
            },
        ];
        assert!(encode_render_sections(&[0, 1], &sections, 1, 1, 1).is_err());
    }

    #[test]
    fn directory_rejects_an_installed_pool_and_fallback_over_gpu_budget() {
        let sections = [RenderSectionInput {
            payload: payload(0x6000),
            fallback_bytes: 32,
            ..RenderSectionInput::default()
        }];
        assert!(encode_render_sections(&[0], &sections, 1, 1024, 64).is_err());
    }
}
