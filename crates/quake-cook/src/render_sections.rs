//! Host encoder for the checked `QRS1` streamed-render sidecar.

use quake_formats::{
    render_section_template_bytes, RenderSectionDirectory, RenderSectionPayload,
    RENDER_SECTION_EDGE_BYTES, RENDER_SECTION_HEADER_BYTES, RENDER_SECTION_MAGIC,
    RENDER_SECTION_NONE, RENDER_SECTION_PAYLOAD_CELL_BYTES, RENDER_SECTION_PAYLOAD_CORNER_BYTES,
    RENDER_SECTION_PAYLOAD_FACE_BYTES, RENDER_SECTION_PAYLOAD_HEADER_BYTES,
    RENDER_SECTION_PAYLOAD_MAGIC, RENDER_SECTION_PAYLOAD_POSITION_BYTES,
    RENDER_SECTION_PAYLOAD_VERSION, RENDER_SECTION_RECORD_BYTES, RENDER_SECTION_TEMPLATE_NONE,
    RENDER_SECTION_VERSION,
};

use super::CookError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionInput {
    /// Sorted neighboring section IDs. The encoder sorts and deduplicates a
    /// caller's input so the wire directory is canonical.
    pub neighbors: Vec<u16>,
    /// Opaque compact geometry/cell/packet-descriptor payload.
    pub payload: Vec<u8>,
    pub active_bytes: u32,
    pub compact_bytes: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionFaceInput {
    pub plane: u16,
    pub material: u16,
    pub first_corner: u16,
    pub corner_count: u8,
    pub flags: u8,
    pub light_styles: [u8; 2],
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub template_eligible: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionCornerInput {
    pub position: u16,
    pub texture: [u8; 2],
    pub light: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionCellInput {
    pub leaf: u16,
    pub stream: Vec<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionPayloadInput {
    pub faces: Vec<RenderSectionFaceInput>,
    pub corners: Vec<RenderSectionCornerInput>,
    pub positions: Vec<[i16; 3]>,
    pub cells: Vec<RenderSectionCellInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EncodedRenderSectionPayload {
    pub bytes: Vec<u8>,
    pub template_bytes: usize,
    pub active_bytes: usize,
}

/// Encode the compact geometry and exact camera-cell streams for one section.
/// Packet templates are not stored on CD: activation constructs them once
/// from these baked corners and installs identical invariant fields in both
/// GPU pools. `active_bytes` includes both template copies and the eight-byte
/// projected-position cache.
pub fn encode_render_section_payload(
    input: &RenderSectionPayloadInput,
) -> Result<EncodedRenderSectionPayload, CookError> {
    let face_count = u16::try_from(input.faces.len())
        .map_err(|_| CookError::new("render-section face count exceeds u16"))?;
    let corner_count = u16::try_from(input.corners.len())
        .map_err(|_| CookError::new("render-section corner count exceeds u16"))?;
    let position_count = u16::try_from(input.positions.len())
        .map_err(|_| CookError::new("render-section position count exceeds u16"))?;
    let cell_count = u16::try_from(input.cells.len())
        .map_err(|_| CookError::new("render-section cell count exceeds u16"))?;
    let mut expected_corner = 0usize;
    let mut template_offsets = Vec::with_capacity(input.faces.len());
    let mut template_bytes = 0usize;
    for face in &input.faces {
        if face.corner_count < 3 || face.first_corner as usize != expected_corner {
            return Err(CookError::new(
                "render-section faces do not own a canonical corner stream",
            ));
        }
        expected_corner = expected_corner
            .checked_add(face.corner_count as usize)
            .ok_or_else(|| CookError::new("render-section corner range overflow"))?;
        let template_offset = if face.template_eligible {
            let offset = u32::try_from(template_bytes)
                .map_err(|_| CookError::new("render-section template offset exceeds u32"))?;
            template_bytes = template_bytes
                .checked_add(render_section_template_bytes(face.corner_count as usize))
                .ok_or_else(|| CookError::new("render-section template size overflow"))?;
            offset
        } else {
            RENDER_SECTION_TEMPLATE_NONE
        };
        template_offsets.push(template_offset);
    }
    if expected_corner != input.corners.len()
        || input
            .corners
            .iter()
            .any(|corner| corner.position as usize >= input.positions.len())
    {
        return Err(CookError::new(
            "render-section corner references a missing position",
        ));
    }
    let mut previous_leaf = None;
    let mut stream_bytes = 0usize;
    for cell in &input.cells {
        if previous_leaf.is_some_and(|leaf| leaf >= cell.leaf) {
            return Err(CookError::new(
                "render-section cells are not in strict source-leaf order",
            ));
        }
        u16::try_from(cell.stream.len())
            .map_err(|_| CookError::new("render-section cell stream exceeds u16"))?;
        stream_bytes = stream_bytes
            .checked_add(cell.stream.len())
            .ok_or_else(|| CookError::new("render-section cell streams overflow"))?;
        previous_leaf = Some(cell.leaf);
    }

    let faces_offset = RENDER_SECTION_PAYLOAD_HEADER_BYTES;
    let faces_end = faces_offset + input.faces.len() * RENDER_SECTION_PAYLOAD_FACE_BYTES;
    let corners_offset = align_up_4(faces_end);
    let corners_end = corners_offset + input.corners.len() * RENDER_SECTION_PAYLOAD_CORNER_BYTES;
    let positions_offset = corners_end;
    let positions_end =
        positions_offset + input.positions.len() * RENDER_SECTION_PAYLOAD_POSITION_BYTES;
    let cells_offset = align_up_4(positions_end);
    let cells_end = cells_offset + input.cells.len() * RENDER_SECTION_PAYLOAD_CELL_BYTES;
    let streams_offset = cells_end;
    let file_bytes = streams_offset
        .checked_add(stream_bytes)
        .ok_or_else(|| CookError::new("render-section payload exceeds address space"))?;
    let mut output = vec![0u8; streams_offset];
    output[0..4].copy_from_slice(&RENDER_SECTION_PAYLOAD_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_SECTION_PAYLOAD_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_SECTION_PAYLOAD_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&face_count.to_le_bytes());
    output[10..12].copy_from_slice(&corner_count.to_le_bytes());
    output[12..14].copy_from_slice(&position_count.to_le_bytes());
    output[14..16].copy_from_slice(&cell_count.to_le_bytes());
    for (offset, value) in [
        (16, faces_offset),
        (20, corners_offset),
        (24, positions_offset),
        (28, cells_offset),
        (32, streams_offset),
    ] {
        output[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    output[40..44].copy_from_slice(&(template_bytes as u32).to_le_bytes());
    output[44..48].copy_from_slice(&(file_bytes as u32).to_le_bytes());
    for (index, face) in input.faces.iter().enumerate() {
        let start = faces_offset + index * RENDER_SECTION_PAYLOAD_FACE_BYTES;
        output[start..start + 2].copy_from_slice(&face.plane.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&face.material.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&face.first_corner.to_le_bytes());
        output[start + 6] = face.corner_count;
        output[start + 7] = face.flags;
        output[start + 8..start + 10].copy_from_slice(&face.light_styles);
        for axis in 0..3 {
            output[start + 12 + axis * 2..start + 14 + axis * 2]
                .copy_from_slice(&face.mins[axis].to_le_bytes());
            output[start + 18 + axis * 2..start + 20 + axis * 2]
                .copy_from_slice(&face.maxs[axis].to_le_bytes());
        }
        output[start + 24..start + 28].copy_from_slice(&template_offsets[index].to_le_bytes());
    }
    for (index, corner) in input.corners.iter().enumerate() {
        let start = corners_offset + index * RENDER_SECTION_PAYLOAD_CORNER_BYTES;
        output[start..start + 2].copy_from_slice(&corner.position.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&corner.texture);
        output[start + 4..start + 8].copy_from_slice(&corner.light.to_le_bytes());
    }
    for (index, position) in input.positions.iter().enumerate() {
        let start = positions_offset + index * RENDER_SECTION_PAYLOAD_POSITION_BYTES;
        for axis in 0..3 {
            output[start + axis * 2..start + axis * 2 + 2]
                .copy_from_slice(&position[axis].to_le_bytes());
        }
    }
    let mut stream_offset = streams_offset;
    for (index, cell) in input.cells.iter().enumerate() {
        let start = cells_offset + index * RENDER_SECTION_PAYLOAD_CELL_BYTES;
        output[start..start + 2].copy_from_slice(&cell.leaf.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&(cell.stream.len() as u16).to_le_bytes());
        output[start + 4..start + 8].copy_from_slice(&(stream_offset as u32).to_le_bytes());
        stream_offset += cell.stream.len();
    }
    for cell in &input.cells {
        output.extend_from_slice(&cell.stream);
    }
    let active_bytes = output
        .len()
        .checked_add(input.positions.len() * 8)
        .and_then(|bytes| bytes.checked_add(template_bytes * 2))
        .ok_or_else(|| CookError::new("render-section active size overflow"))?;
    RenderSectionPayload::parse(&output).map_err(|error| {
        CookError::new(format!(
            "encoded render-section payload is invalid: {error:?}"
        ))
    })?;
    Ok(EncodedRenderSectionPayload {
        bytes: output,
        template_bytes,
        active_bytes,
    })
}

/// Encode one canonical sidecar and immediately validate it through the guest
/// reader. Solid/unowned BSP leaves use [`RENDER_SECTION_NONE`].
pub fn encode_render_sections(
    leaf_sections: &[u16],
    section_inputs: &[RenderSectionInput],
) -> Result<Vec<u8>, CookError> {
    let leaf_count = u16::try_from(leaf_sections.len())
        .map_err(|_| CookError::new("render-section leaf count exceeds u16"))?;
    let section_count = u16::try_from(section_inputs.len())
        .map_err(|_| CookError::new("render-section count exceeds u16"))?;
    for &section in leaf_sections {
        if section != RENDER_SECTION_NONE && section >= section_count {
            return Err(CookError::new(
                "render-section leaf references a missing section",
            ));
        }
    }

    let mut neighbor_lists = Vec::with_capacity(section_inputs.len());
    let mut edge_count = 0usize;
    let mut payload_bytes = 0usize;
    for (section_index, section) in section_inputs.iter().enumerate() {
        if section.compact_bytes > section.active_bytes {
            return Err(CookError::new(
                "render-section compact bytes exceed active bytes",
            ));
        }
        let mut neighbors = section.neighbors.clone();
        neighbors.sort_unstable();
        neighbors.dedup();
        if neighbors.iter().any(|&neighbor| {
            neighbor as usize >= section_inputs.len() || neighbor as usize == section_index
        }) {
            return Err(CookError::new(
                "render-section neighbor references a missing or identical section",
            ));
        }
        edge_count = edge_count
            .checked_add(neighbors.len())
            .ok_or_else(|| CookError::new("render-section edge count overflow"))?;
        payload_bytes = payload_bytes
            .checked_add(section.payload.len())
            .ok_or_else(|| CookError::new("render-section payload size overflow"))?;
        neighbor_lists.push(neighbors);
    }
    let edge_count_u16 = u16::try_from(edge_count)
        .map_err(|_| CookError::new("render-section edge count exceeds u16"))?;

    let leaf_offset = RENDER_SECTION_HEADER_BYTES;
    let leaf_end = leaf_offset
        .checked_add(leaf_sections.len() * 2)
        .ok_or_else(|| CookError::new("render-section leaf table overflow"))?;
    let section_offset = align_up_4(leaf_end);
    let section_end = section_offset
        .checked_add(section_inputs.len() * RENDER_SECTION_RECORD_BYTES)
        .ok_or_else(|| CookError::new("render-section table overflow"))?;
    let edge_offset = section_end;
    let edge_end = edge_offset
        .checked_add(edge_count * RENDER_SECTION_EDGE_BYTES)
        .ok_or_else(|| CookError::new("render-section edge table overflow"))?;
    let payload_offset = align_up_4(edge_end);
    let file_bytes = payload_offset
        .checked_add(payload_bytes)
        .ok_or_else(|| CookError::new("render-section file size overflow"))?;
    let file_bytes_u32 =
        u32::try_from(file_bytes).map_err(|_| CookError::new("render-section file exceeds u32"))?;
    let mut output = vec![0u8; payload_offset];
    output[0..4].copy_from_slice(&RENDER_SECTION_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_SECTION_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_SECTION_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&leaf_count.to_le_bytes());
    output[10..12].copy_from_slice(&section_count.to_le_bytes());
    output[12..14].copy_from_slice(&edge_count_u16.to_le_bytes());
    output[14..16].copy_from_slice(&(RENDER_SECTION_RECORD_BYTES as u16).to_le_bytes());
    output[16..20].copy_from_slice(&(leaf_offset as u32).to_le_bytes());
    output[20..24].copy_from_slice(&(section_offset as u32).to_le_bytes());
    output[24..28].copy_from_slice(&(edge_offset as u32).to_le_bytes());
    output[28..32].copy_from_slice(&(payload_offset as u32).to_le_bytes());
    output[32..36].copy_from_slice(&file_bytes_u32.to_le_bytes());

    for (leaf, &section) in leaf_sections.iter().enumerate() {
        let start = leaf_offset + leaf * 2;
        output[start..start + 2].copy_from_slice(&section.to_le_bytes());
    }

    let mut first_edge = 0usize;
    let mut section_payload_offset = payload_offset;
    for (section_index, section) in section_inputs.iter().enumerate() {
        let start = section_offset + section_index * RENDER_SECTION_RECORD_BYTES;
        let neighbors = &neighbor_lists[section_index];
        output[start..start + 2].copy_from_slice(&(first_edge as u16).to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&(neighbors.len() as u16).to_le_bytes());
        output[start + 4..start + 8]
            .copy_from_slice(&(section_payload_offset as u32).to_le_bytes());
        output[start + 8..start + 12]
            .copy_from_slice(&(section.payload.len() as u32).to_le_bytes());
        output[start + 12..start + 16].copy_from_slice(&section.active_bytes.to_le_bytes());
        output[start + 16..start + 20].copy_from_slice(&section.compact_bytes.to_le_bytes());
        output[start + 20..start + 22].copy_from_slice(&section.flags.to_le_bytes());
        for (neighbor_index, &neighbor) in neighbors.iter().enumerate() {
            let edge = edge_offset + (first_edge + neighbor_index) * RENDER_SECTION_EDGE_BYTES;
            output[edge..edge + 2].copy_from_slice(&neighbor.to_le_bytes());
        }
        first_edge += neighbors.len();
        section_payload_offset += section.payload.len();
    }
    for section in section_inputs {
        output.extend_from_slice(&section.payload);
    }
    debug_assert_eq!(output.len(), file_bytes);
    RenderSectionDirectory::parse(&output).map_err(|error| {
        CookError::new(format!("encoded render sections are invalid: {error:?}"))
    })?;
    Ok(output)
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_roundtrips_canonical_directory() {
        let sections = [
            RenderSectionInput {
                neighbors: vec![1, 1],
                payload: vec![1, 2, 3],
                active_bytes: 192_000,
                compact_bytes: 80_000,
                flags: 7,
            },
            RenderSectionInput {
                neighbors: vec![0],
                payload: vec![4, 5],
                active_bytes: 180_000,
                compact_bytes: 70_000,
                flags: 0,
            },
        ];
        let bytes = encode_render_sections(&[RENDER_SECTION_NONE, 0, 1], &sections).unwrap();
        let directory = RenderSectionDirectory::parse(&bytes).unwrap();
        assert_eq!(directory.section_count(), 2);
        assert_eq!(directory.edge_count(), 2);
        assert_eq!(directory.section(0).unwrap().flags, 7);
        assert_eq!(
            directory.payload(directory.section(1).unwrap()),
            Some(&[4, 5][..])
        );
    }

    #[test]
    fn payload_encoder_derives_exact_compact_and_active_sizes() {
        let input = RenderSectionPayloadInput {
            faces: vec![RenderSectionFaceInput {
                plane: 3,
                material: 5,
                first_corner: 0,
                corner_count: 3,
                flags: 7,
                light_styles: [0, 64],
                mins: [-1, -2, -3],
                maxs: [4, 5, 6],
                template_eligible: true,
            }],
            corners: vec![
                RenderSectionCornerInput {
                    position: 0,
                    ..RenderSectionCornerInput::default()
                },
                RenderSectionCornerInput {
                    position: 1,
                    ..RenderSectionCornerInput::default()
                },
                RenderSectionCornerInput {
                    position: 2,
                    ..RenderSectionCornerInput::default()
                },
            ],
            positions: vec![[0, 0, 0], [1, 0, 0], [0, 1, 0]],
            cells: vec![RenderSectionCellInput {
                leaf: 9,
                stream: vec![1, 2, 3],
            }],
        };
        let encoded = encode_render_section_payload(&input).unwrap();
        let payload = RenderSectionPayload::parse(&encoded.bytes).unwrap();
        assert_eq!(payload.template_bytes(), 40);
        assert_eq!(payload.face(0).unwrap().material, 5);
        assert_eq!(
            payload.cell_stream(payload.cell(0).unwrap()),
            Some(&[1, 2, 3][..])
        );
        assert_eq!(encoded.active_bytes, encoded.bytes.len() + 3 * 8 + 2 * 40);
    }

    #[test]
    fn encoder_rejects_invalid_ownership_and_budget() {
        let section = RenderSectionInput {
            active_bytes: 4,
            compact_bytes: 5,
            ..RenderSectionInput::default()
        };
        assert!(encode_render_sections(&[0], &[section]).is_err());
        assert!(encode_render_sections(&[1], &[RenderSectionInput::default()]).is_err());
    }
}
