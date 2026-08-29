//! Canonical host encoders for `QRS5` and its complete `QRP5` render objects.

use std::collections::BTreeMap;

use quake_formats::{
    RenderCellDirectory, RenderQuad, RenderQuadCommand, RenderQuadCorner, RenderQuadFace,
    RenderQuadObject, RenderQuadPayload, RenderQuadRun, RenderSectionDirectory,
    RENDER_CELL_HEADER_BYTES, RENDER_CELL_MAGIC, RENDER_CELL_NONE, RENDER_CELL_OFFSET_BYTES,
    RENDER_CELL_VERSION,
    RENDER_QUAD_CELL_BYTES,
    RENDER_QUAD_COMMAND_BYTES, RENDER_QUAD_CORNER_BYTES, RENDER_QUAD_FACE_BYTES,
    RENDER_QUAD_HEADER_BYTES, RENDER_QUAD_OBJECT_BYTES, RENDER_QUAD_OBJECT_SUBMODEL,
    RENDER_QUAD_PACKET_BYTES, RENDER_QUAD_PAYLOAD_MAGIC, RENDER_QUAD_PAYLOAD_VERSION,
    RENDER_QUAD_POSITION_BYTES, RENDER_QUAD_PROJECTED_POSITION_BYTES, RENDER_QUAD_RECORD_BYTES,
    RENDER_QUAD_REFERENCE_BYTES, RENDER_QUAD_RUNTIME_FACE_BYTES, RENDER_QUAD_RUN_BYTES,
    RENDER_SECTION_EDGE_BYTES, RENDER_SECTION_HEADER_BYTES, RENDER_SECTION_MAGIC,
    RENDER_SECTION_NONE, RENDER_SECTION_RECORD_BYTES, RENDER_SECTION_VERSION,
};

use super::CookError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderQuadCellInput {
    pub leaf: u16,
    pub flags: u16,
    pub portal_leaf: u16,
    pub portal_plane: i16,
    pub visibility: Vec<u8>,
    pub portal_visibility: Vec<u8>,
    pub commands: Vec<RenderQuadCommand>,
}

impl Default for RenderQuadCellInput {
    fn default() -> Self {
        Self {
            leaf: 0,
            flags: 0,
            portal_leaf: u16::MAX,
            portal_plane: -1,
            visibility: Vec::new(),
            portal_visibility: Vec::new(),
            commands: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadPayloadInput {
    pub objects: Vec<RenderQuadObject>,
    pub faces: Vec<RenderQuadFace>,
    pub corners: Vec<RenderQuadCorner>,
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
    pub first_cell: u16,
    pub cell_count: u16,
    pub fallback_bytes: u32,
    pub flags: u16,
}

pub fn encode_render_quad_payload(
    input: &RenderQuadPayloadInput,
) -> Result<EncodedRenderQuadPayload, CookError> {
    let object_count = count_u16(input.objects.len(), "render object")?;
    let face_count = count_u16(input.faces.len(), "render face")?;
    let corner_count = count_u16(input.corners.len(), "render corner")?;
    let quad_count = count_u16(input.quads.len(), "render quad")?;
    let position_count = count_u16(input.positions.len(), "render position")?;
    let run_count = count_u16(input.runs.len(), "render material run")?;
    let cell_count = count_u16(input.cells.len(), "render cell")?;
    let visibility_row_bytes = input.cells.first().map_or(0, |cell| cell.visibility.len());
    let visibility_row_bytes_u16 = count_u16(visibility_row_bytes, "render visibility row byte")?;
    if input.cells.iter().any(|cell| {
        cell.visibility.len() != visibility_row_bytes
            || cell.portal_visibility.len() != visibility_row_bytes
    }) {
        return Err(CookError::new(
            "render cells have inconsistent visibility row sizes",
        ));
    }
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
    let corners_offset = faces_end;
    let corners_end = corners_offset + input.corners.len() * RENDER_QUAD_CORNER_BYTES;
    let quads_offset = corners_end;
    let quads_end = quads_offset + input.quads.len() * RENDER_QUAD_RECORD_BYTES;
    let positions_offset = quads_end;
    let positions_end = positions_offset + input.positions.len() * RENDER_QUAD_POSITION_BYTES;
    let runs_offset = align_up_4(positions_end);
    let runs_end = runs_offset + input.runs.len() * RENDER_QUAD_RUN_BYTES;
    let cells_offset = runs_end;
    let cells_end = cells_offset + input.cells.len() * RENDER_QUAD_CELL_BYTES;
    let visibility_bytes = input
        .cells
        .len()
        .checked_mul(visibility_row_bytes)
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or_else(|| CookError::new("render visibility rows exceed address space"))?;
    let streams_offset = cells_end
        .checked_add(visibility_bytes)
        .ok_or_else(|| CookError::new("render visibility rows exceed address space"))?;
    let file_bytes = streams_offset
        .checked_add(command_count * RENDER_QUAD_COMMAND_BYTES)
        .ok_or_else(|| CookError::new("quad-native payload exceeds address space"))?;
    let file_bytes_u32 =
        u32::try_from(file_bytes).map_err(|_| CookError::new("quad-native payload exceeds u32"))?;
    let runtime_metadata_bytes = input
        .objects
        .len()
        .checked_mul(RENDER_QUAD_OBJECT_BYTES)
        .and_then(|bytes| bytes.checked_add(input.faces.len() * RENDER_QUAD_RUNTIME_FACE_BYTES))
        .and_then(|bytes| bytes.checked_add(input.corners.len() * RENDER_QUAD_CORNER_BYTES))
        .and_then(|bytes| bytes.checked_add(input.quads.len() * RENDER_QUAD_REFERENCE_BYTES))
        .and_then(|bytes| bytes.checked_add(input.positions.len() * RENDER_QUAD_POSITION_BYTES))
        .and_then(|bytes| bytes.checked_add(input.cells.len() * RENDER_QUAD_CELL_BYTES))
        .and_then(|bytes| bytes.checked_add(visibility_bytes))
        .and_then(|bytes| bytes.checked_add(command_count * RENDER_QUAD_COMMAND_BYTES))
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| CookError::new("quad-native runtime metadata exceeds u32"))?;
    let mut output = vec![0u8; file_bytes];
    output[0..4].copy_from_slice(&RENDER_QUAD_PAYLOAD_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_QUAD_PAYLOAD_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_QUAD_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&object_count.to_le_bytes());
    output[10..12].copy_from_slice(&face_count.to_le_bytes());
    output[12..14].copy_from_slice(&corner_count.to_le_bytes());
    output[14..16].copy_from_slice(&quad_count.to_le_bytes());
    output[16..18].copy_from_slice(&position_count.to_le_bytes());
    output[18..20].copy_from_slice(&run_count.to_le_bytes());
    output[20..22].copy_from_slice(&cell_count.to_le_bytes());
    output[22..24].copy_from_slice(&visibility_row_bytes_u16.to_le_bytes());
    for (offset, value) in [
        (24, objects_offset),
        (28, faces_offset),
        (32, corners_offset),
        (36, quads_offset),
        (40, positions_offset),
        (44, runs_offset),
        (48, cells_offset),
        (52, streams_offset),
    ] {
        output[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    output[56..60].copy_from_slice(&file_bytes_u32.to_le_bytes());
    output[60..64].copy_from_slice(&packet_pool_bytes.to_le_bytes());
    output[64..68].copy_from_slice(&projection_bytes.to_le_bytes());
    output[68..72].copy_from_slice(&runtime_metadata_bytes.to_le_bytes());

    for (index, object) in input.objects.iter().enumerate() {
        let start = objects_offset + index * RENDER_QUAD_OBJECT_BYTES;
        output[start..start + 2].copy_from_slice(&object.first_face.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&object.face_count.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&object.first_corner.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&object.corner_count.to_le_bytes());
        output[start + 8..start + 10].copy_from_slice(&object.first_quad.to_le_bytes());
        output[start + 10..start + 12].copy_from_slice(&object.quad_count.to_le_bytes());
        output[start + 12..start + 14].copy_from_slice(&object.first_position.to_le_bytes());
        output[start + 14..start + 16].copy_from_slice(&object.position_count.to_le_bytes());
        output[start + 16..start + 18].copy_from_slice(&object.first_run.to_le_bytes());
        output[start + 18..start + 20].copy_from_slice(&object.run_count.to_le_bytes());
        for axis in 0..3 {
            output[start + 20 + axis * 2..start + 22 + axis * 2]
                .copy_from_slice(&object.mins[axis].to_le_bytes());
            output[start + 26 + axis * 2..start + 28 + axis * 2]
                .copy_from_slice(&object.maxs[axis].to_le_bytes());
        }
        output[start + 32..start + 34].copy_from_slice(&object.flags.to_le_bytes());
    }
    for (index, face) in input.faces.iter().enumerate() {
        let start = faces_offset + index * RENDER_QUAD_FACE_BYTES;
        output[start..start + 2].copy_from_slice(&face.source_face.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&face.first_corner.to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&face.first_quad.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&face.quad_count.to_le_bytes());
        output[start + 8..start + 10].copy_from_slice(&face.plane.to_le_bytes());
        output[start + 10..start + 12].copy_from_slice(&face.material.to_le_bytes());
        output[start + 12] = face.flags;
        output[start + 13] = face.corner_count;
        output[start + 14..start + 16].copy_from_slice(&face.light_styles);
    }
    for (index, corner) in input.corners.iter().enumerate() {
        let start = corners_offset + index * RENDER_QUAD_CORNER_BYTES;
        output[start] = corner.position;
        output[start + 1..start + 3].copy_from_slice(&corner.texture);
        output[start + 4..start + 8].copy_from_slice(&corner.light.to_le_bytes());
    }
    for (index, quad) in input.quads.iter().enumerate() {
        let start = quads_offset + index * RENDER_QUAD_RECORD_BYTES;
        output[start..start + 4].copy_from_slice(&quad.positions);
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
        output[start + 10..start + 12].copy_from_slice(&cell.portal_leaf.to_le_bytes());
        output[start + 12..start + 14].copy_from_slice(&cell.portal_plane.to_le_bytes());
        let visibility_start = cells_end + index * visibility_row_bytes * 2;
        output[visibility_start..visibility_start + visibility_row_bytes]
            .copy_from_slice(&cell.visibility);
        output
            [visibility_start + visibility_row_bytes..visibility_start + visibility_row_bytes * 2]
            .copy_from_slice(&cell.portal_visibility);
        for command in &cell.commands {
            output[stream_offset..stream_offset + 2].copy_from_slice(&command.object.to_le_bytes());
            output[stream_offset + 2..stream_offset + 4]
                .copy_from_slice(&command.flags.to_le_bytes());
            output[stream_offset + 4..stream_offset + 8]
                .copy_from_slice(&command.visible_faces.to_le_bytes());
            output[stream_offset + 8..stream_offset + 12]
                .copy_from_slice(&command.portal_faces.to_le_bytes());
            output[stream_offset + 12..stream_offset + 16]
                .copy_from_slice(&command.dynamic_faces.to_le_bytes());
            output[stream_offset + 16..stream_offset + 20]
                .copy_from_slice(&command.template_faces.to_le_bytes());
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

/// Encode one resident compact QRP5 dictionary followed by independently
/// addressable camera-cell blocks. The dictionary is read once during map
/// loading; a leaf transition reads only its exact cell block and patches the
/// in-RAM QRP5 header to bind that block as cell zero.
pub fn encode_resident_render_cells(
    leaf_count: usize,
    payload: &EncodedRenderQuadPayload,
    resident_core_bytes: u32,
    arena_budget_bytes: u32,
    packet_pool_budget_bytes: u32,
) -> Result<Vec<u8>, CookError> {
    let leaf_count_u16 = count_u16(leaf_count, "resident render-cell leaf")?;
    if leaf_count < 2 {
        return Err(CookError::new("resident render-cell map has too few leaves"));
    }
    let source = RenderQuadPayload::parse(&payload.bytes)
        .map_err(|error| CookError::new(format!("shared QRP5 payload is invalid: {error:?}")))?;
    if source.cell_count() != leaf_count - 1 || source.visibility_row_bytes() == 0 {
        return Err(CookError::new(
            "resident render cells do not cover every non-solid leaf",
        ));
    }

    let mut dictionary_input = RenderQuadPayloadInput::default();
    for index in 0..source.object_count() {
        dictionary_input.objects.push(
            source
                .object(index)
                .ok_or_else(|| CookError::new("resident dictionary object is missing"))?,
        );
    }
    for index in 0..source.face_count() {
        dictionary_input.faces.push(
            source
                .face(index)
                .ok_or_else(|| CookError::new("resident dictionary face is missing"))?,
        );
    }
    for index in 0..source.corner_count() {
        dictionary_input.corners.push(
            source
                .corner(index)
                .ok_or_else(|| CookError::new("resident dictionary corner is missing"))?,
        );
    }
    for index in 0..source.quad_count() {
        dictionary_input.quads.push(
            source
                .quad(index)
                .ok_or_else(|| CookError::new("resident dictionary quad is missing"))?,
        );
    }
    for index in 0..source.position_count() {
        dictionary_input.positions.push(
            source
                .position(index)
                .ok_or_else(|| CookError::new("resident dictionary position is missing"))?,
        );
    }
    if source.run_count() != 0 {
        return Err(CookError::new(
            "resident QRP5 dictionary unexpectedly contains activation runs",
        ));
    }
    let dictionary = encode_render_quad_payload(&dictionary_input)?;
    let dictionary_view = RenderQuadPayload::parse(&dictionary.bytes)
        .map_err(|_| CookError::new("resident QRP5 dictionary failed validation"))?;
    if dictionary_view.cell_count() != 0 || dictionary_view.visibility_row_bytes() != 0 {
        return Err(CookError::new(
            "resident QRP5 dictionary contains camera-cell state",
        ));
    }

    let mut cells = vec![None; leaf_count];
    for cell_index in 0..source.cell_count() {
        let cell = source
            .cell(cell_index)
            .ok_or_else(|| CookError::new("resident render cell is missing"))?;
        let leaf = cell.leaf as usize;
        if leaf == 0 || leaf >= leaf_count || cells[leaf].replace(cell_index).is_some() {
            return Err(CookError::new(
                "resident render cells have a missing or duplicate leaf",
            ));
        }
    }
    if cells.iter().skip(1).any(Option::is_none) {
        return Err(CookError::new(
            "resident render cells do not cover every non-solid leaf",
        ));
    }

    let mut blocks = Vec::with_capacity(leaf_count - 1);
    let mut block_leaves = Vec::with_capacity(leaf_count - 1);
    let mut max_cell_bytes = 0usize;
    let mut max_packet_pool_bytes = 0usize;
    // The source payload has already been sorted by spatial Morton key. Keep
    // that order on disc so one bounded section remains useful across many
    // adjacent BSP leaf transitions.
    for cell_index in 0..source.cell_count() {
        let cell = source.cell(cell_index).unwrap();
        let leaf = cell.leaf as usize;
        let row_bytes = source.visibility_row_bytes();
        let stream_bytes = (cell.command_count as usize)
            .checked_mul(RENDER_QUAD_COMMAND_BYTES)
            .ok_or_else(|| CookError::new("resident cell command stream exceeds address space"))?;
        let block_bytes = RENDER_QUAD_CELL_BYTES
            .checked_add(row_bytes * 2)
            .and_then(|bytes| bytes.checked_add(stream_bytes))
            .ok_or_else(|| CookError::new("resident cell block exceeds address space"))?;
        let mut block = vec![0u8; block_bytes];
        block[0..2].copy_from_slice(&cell.leaf.to_le_bytes());
        block[2..4].copy_from_slice(&cell.command_count.to_le_bytes());
        // Runtime stream offset is relative to the assembled QRP5 image and
        // is patched after this block is read behind the resident dictionary.
        block[4..8].fill(0);
        block[8..10].copy_from_slice(&cell.flags.to_le_bytes());
        block[10..12].copy_from_slice(&cell.portal_leaf.to_le_bytes());
        block[12..14].copy_from_slice(&cell.portal_plane.to_le_bytes());
        let visibility = source
            .visibility(cell_index)
            .ok_or_else(|| CookError::new("resident cell visibility row is missing"))?;
        let portal_visibility = source
            .portal_visibility(cell_index)
            .ok_or_else(|| CookError::new("resident portal visibility row is missing"))?;
        block[RENDER_QUAD_CELL_BYTES..RENDER_QUAD_CELL_BYTES + row_bytes]
            .copy_from_slice(visibility);
        block[RENDER_QUAD_CELL_BYTES + row_bytes..RENDER_QUAD_CELL_BYTES + row_bytes * 2]
            .copy_from_slice(portal_visibility);
        let mut destination = RENDER_QUAD_CELL_BYTES + row_bytes * 2;
        let mut packet_bytes = 0usize;
        for command_index in 0..cell.command_count as usize {
            let command = source
                .command(cell, command_index)
                .ok_or_else(|| CookError::new("resident cell command is missing"))?;
            block[destination..destination + 2].copy_from_slice(&command.object.to_le_bytes());
            block[destination + 2..destination + 4]
                .copy_from_slice(&command.flags.to_le_bytes());
            block[destination + 4..destination + 8]
                .copy_from_slice(&command.visible_faces.to_le_bytes());
            block[destination + 8..destination + 12]
                .copy_from_slice(&command.portal_faces.to_le_bytes());
            block[destination + 12..destination + 16]
                .copy_from_slice(&command.dynamic_faces.to_le_bytes());
            block[destination + 16..destination + 20]
                .copy_from_slice(&command.template_faces.to_le_bytes());
            let object = source
                .object(command.object as usize)
                .ok_or_else(|| CookError::new("resident cell command object is missing"))?;
            for local_face in 0..object.face_count as usize {
                if command.template_faces & (1 << local_face) != 0 {
                    packet_bytes = packet_bytes
                        .checked_add(
                            source
                                .face(object.first_face as usize + local_face)
                                .ok_or_else(|| CookError::new("resident template face is missing"))?
                                .quad_count as usize
                                * RENDER_QUAD_PACKET_BYTES,
                        )
                        .ok_or_else(|| CookError::new("resident packet pool exceeds address space"))?;
                }
            }
            destination += RENDER_QUAD_COMMAND_BYTES;
        }
        if packet_bytes > packet_pool_budget_bytes as usize {
            return Err(CookError::new(format!(
                "resident cell leaf {leaf} needs {packet_bytes} fixed-packet bytes, budget is {packet_pool_budget_bytes}"
            )));
        }
        max_cell_bytes = max_cell_bytes.max(block.len());
        max_packet_pool_bytes = max_packet_pool_bytes.max(packet_bytes);
        block_leaves.push(leaf);
        blocks.push(block);
    }

    // A large region amortises seek/setup latency but also transfers many
    // camera cells the route may never enter. Keep the shipping default here
    // while allowing deterministic PSoXide A/Bs to locate that balance.
    const DEFAULT_SECTION_TARGET_KIB: usize = 8;
    let section_target_bytes = std::env::var("QUAKE_PSX_RENDER_CELL_SECTION_KIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|kib| (4..=64).contains(kib))
        .unwrap_or(DEFAULT_SECTION_TARGET_KIB)
        * 1024;
    const SECTION_SAFETY_BYTES: usize = 4 * 1024;
    let fixed_resident_bytes = resident_core_bytes as usize
        + dictionary.bytes.len()
        + max_cell_bytes;
    let available_section_bytes = (arena_budget_bytes as usize)
        .checked_sub(fixed_resident_bytes)
        .ok_or_else(|| CookError::new("resident dictionary leaves no camera-section arena"))?;
    let section_budget = section_target_bytes.min(
        available_section_bytes
            .checked_sub(SECTION_SAFETY_BYTES)
            .ok_or_else(|| CookError::new("resident camera section has no safety margin"))?,
    );
    if section_budget < max_cell_bytes || section_budget > u16::MAX as usize {
        return Err(CookError::new(format!(
            "resident camera-section budget {section_budget} cannot hold {max_cell_bytes}-byte cell"
        )));
    }
    let mut section_ranges = Vec::new();
    let mut first_block = 0usize;
    while first_block < blocks.len() {
        let mut end_block = first_block;
        let mut section_bytes = 0usize;
        while end_block < blocks.len()
            && section_bytes + blocks[end_block].len() <= section_budget
        {
            section_bytes += blocks[end_block].len();
            end_block += 1;
        }
        if end_block == first_block {
            return Err(CookError::new("resident camera cell exceeds section budget"));
        }
        section_ranges.push((first_block, end_block, section_bytes));
        first_block = end_block;
    }
    let section_count_u16 = count_u16(section_ranges.len(), "resident render-cell section")?;
    let leaf_records_offset = RENDER_CELL_HEADER_BYTES;
    let section_offsets_offset = align_up_4(
        leaf_records_offset
            .checked_add(
                leaf_count
                    .checked_mul(RENDER_CELL_OFFSET_BYTES)
                    .ok_or_else(|| CookError::new("render-cell directory exceeds address space"))?,
            )
            .ok_or_else(|| CookError::new("render-cell directory exceeds address space"))?,
    );
    let dictionary_offset = align_up_4(
        section_offsets_offset
            .checked_add(
                (section_ranges.len() + 1)
                    .checked_mul(RENDER_CELL_OFFSET_BYTES)
                    .ok_or_else(|| CookError::new("render-cell directory exceeds address space"))?,
            )
            .ok_or_else(|| CookError::new("render-cell directory exceeds address space"))?,
    );
    let cells_offset = dictionary_offset
        .checked_add(dictionary.bytes.len())
        .ok_or_else(|| CookError::new("resident dictionary exceeds address space"))?;
    let file_bytes = blocks
        .iter()
        .try_fold(cells_offset, |bytes, block| bytes.checked_add(block.len()))
        .ok_or_else(|| CookError::new("resident render-cell sidecar exceeds address space"))?;
    let max_section_bytes = section_ranges
        .iter()
        .map(|&(_, _, bytes)| bytes)
        .max()
        .unwrap_or(0);
    let resident_high_water = resident_core_bytes as usize
        + dictionary.bytes.len()
        + max_cell_bytes
        + max_section_bytes;
    if resident_high_water > arena_budget_bytes as usize {
        return Err(CookError::new(format!(
            "resident render cells need {resident_high_water} arena bytes, budget is {arena_budget_bytes}"
        )));
    }
    let mut output = vec![0u8; file_bytes];
    output[0..4].copy_from_slice(&RENDER_CELL_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&RENDER_CELL_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(RENDER_CELL_HEADER_BYTES as u16).to_le_bytes());
    output[8..10].copy_from_slice(&leaf_count_u16.to_le_bytes());
    output[10..12].copy_from_slice(&section_count_u16.to_le_bytes());
    output[12..14].copy_from_slice(&(source.visibility_row_bytes() as u16).to_le_bytes());
    output[14..16].copy_from_slice(&(RENDER_CELL_OFFSET_BYTES as u16).to_le_bytes());
    for (offset, value) in [
        (16, leaf_records_offset),
        (20, section_offsets_offset),
        (24, dictionary_offset),
        (28, dictionary.bytes.len()),
        (32, cells_offset),
        (36, file_bytes),
    ] {
        output[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    output[40..44].copy_from_slice(&resident_core_bytes.to_le_bytes());
    output[44..48].copy_from_slice(&(resident_high_water as u32).to_le_bytes());
    output[48..52].copy_from_slice(&packet_pool_budget_bytes.to_le_bytes());
    output[52..56].copy_from_slice(&(max_section_bytes as u32).to_le_bytes());
    output[56..60].copy_from_slice(&(max_cell_bytes as u32).to_le_bytes());
    output[60..64].copy_from_slice(&(max_packet_pool_bytes as u32).to_le_bytes());
    output[dictionary_offset..cells_offset].copy_from_slice(&dictionary.bytes);

    for leaf in 0..leaf_count {
        let offset = leaf_records_offset + leaf * RENDER_CELL_OFFSET_BYTES;
        output[offset..offset + 2].copy_from_slice(&RENDER_CELL_NONE.to_le_bytes());
    }
    let mut destination = cells_offset;
    for (section_index, &(first, end, section_bytes)) in section_ranges.iter().enumerate() {
        let section_offset = section_offsets_offset + section_index * RENDER_CELL_OFFSET_BYTES;
        output[section_offset..section_offset + 4]
            .copy_from_slice(&(destination as u32).to_le_bytes());
        let section_start = destination;
        for block_index in first..end {
            let block = &blocks[block_index];
            let cell_offset = destination - section_start;
            let leaf = block_leaves[block_index];
            let leaf_offset = leaf_records_offset + leaf * RENDER_CELL_OFFSET_BYTES;
            output[leaf_offset..leaf_offset + 2]
                .copy_from_slice(&(section_index as u16).to_le_bytes());
            output[leaf_offset + 2..leaf_offset + 4]
                .copy_from_slice(&(cell_offset as u16).to_le_bytes());
            let block_end = destination + block.len();
            output[destination..block_end].copy_from_slice(block);
            destination = block_end;
        }
        debug_assert_eq!(destination - section_start, section_bytes);
    }
    let final_section_offset =
        section_offsets_offset + section_ranges.len() * RENDER_CELL_OFFSET_BYTES;
    output[final_section_offset..final_section_offset + 4]
        .copy_from_slice(&(destination as u32).to_le_bytes());
    debug_assert_eq!(destination, output.len());
    RenderCellDirectory::parse_prefix(&output[..dictionary_offset], output.len()).map_err(
        |error| CookError::new(format!("encoded resident render cells are invalid: {error:?}")),
    )?;
    Ok(output)
}

pub fn encode_render_sections(
    leaf_sections: &[u16],
    payload: &EncodedRenderQuadPayload,
    sections: &[RenderSectionInput],
    resident_core_bytes: u32,
    streaming_budget_bytes: u32,
    packet_pool_budget_bytes: u32,
) -> Result<Vec<u8>, CookError> {
    let leaf_count = count_u16(leaf_sections.len(), "render-section leaf")?;
    let section_count = count_u16(sections.len(), "render section")?;
    let parsed = RenderQuadPayload::parse(&payload.bytes)
        .map_err(|error| CookError::new(format!("shared QRP4 payload is invalid: {error:?}")))?;
    if parsed.packet_pool_bytes() != payload.packet_pool_bytes
        || parsed.projection_bytes() != payload.projection_bytes
        || parsed.runtime_metadata_bytes() != payload.runtime_metadata_bytes
    {
        return Err(CookError::new(
            "shared QRP4 derived memory accounting drifted",
        ));
    }
    for &section in leaf_sections {
        if section != RENDER_SECTION_NONE && section >= section_count {
            return Err(CookError::new(
                "render-section leaf references a missing section",
            ));
        }
    }

    let mut neighbors = Vec::with_capacity(sections.len());
    let mut memories = Vec::with_capacity(sections.len());
    let mut section_payloads = Vec::with_capacity(sections.len());
    let mut edge_count = 0usize;
    let mut expected_cell = 0usize;
    for (section_index, section) in sections.iter().enumerate() {
        if section.cell_count == 0 || section.first_cell as usize != expected_cell {
            return Err(CookError::new(
                "render sections do not canonically partition QRP4 cells",
            ));
        }
        let encoded_section = encode_render_quad_payload(&extract_section_payload(
            parsed,
            section.first_cell as usize,
            section.cell_count as usize,
        )?)?;
        let memory = quake_formats::RenderQuadSectionMemory {
            staging_bytes: u32::try_from(encoded_section.bytes.len())
                .map_err(|_| CookError::new("render section payload exceeds u32"))?,
            activation_bytes: encoded_section
                .runtime_metadata_bytes
                .checked_add(encoded_section.projection_bytes)
                .ok_or_else(|| CookError::new("render section activation bytes overflow"))?,
            packet_pool_bytes: encoded_section.packet_pool_bytes,
            projection_bytes: encoded_section.projection_bytes,
        };
        expected_cell = expected_cell
            .checked_add(section.cell_count as usize)
            .ok_or_else(|| CookError::new("render-section cell range overflow"))?;
        if memory.activation_bytes > streaming_budget_bytes
            || memory.packet_pool_bytes > packet_pool_budget_bytes
        {
            return Err(CookError::new(format!(
                "render section {section_index} exceeds its CPU or fixed-packet arena: activation={} budget={}, packets={} packet_budget={}, fallback_candidates={}",
                memory.activation_bytes,
                streaming_budget_bytes,
                memory.packet_pool_bytes,
                packet_pool_budget_bytes,
                section.fallback_bytes,
            )));
        }
        memories.push(memory);
        section_payloads.push(encoded_section);
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
        neighbors.push(list);
    }
    if expected_cell != parsed.cell_count() {
        return Err(CookError::new(
            "render sections do not cover every QRP4 cell",
        ));
    }
    let edge_count_u16 = count_u16(edge_count, "render-section edge")?;

    let leaf_offset = RENDER_SECTION_HEADER_BYTES;
    let leaf_end = leaf_offset + leaf_sections.len() * 2;
    let section_offset = align_up_4(leaf_end);
    let section_end = section_offset + sections.len() * RENDER_SECTION_RECORD_BYTES;
    let edge_offset = section_end;
    let edge_end = edge_offset + edge_count * RENDER_SECTION_EDGE_BYTES;
    let payload_offset = align_up_4(edge_end);
    let file_bytes = section_payloads
        .iter()
        .try_fold(payload_offset, |bytes, payload| {
            bytes
                .checked_add(payload.bytes.len())
                .ok_or_else(|| CookError::new("render-section file size overflow"))
        })?;
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
    output[44..48].copy_from_slice(&packet_pool_budget_bytes.to_le_bytes());
    for (leaf, section) in leaf_sections.iter().enumerate() {
        let start = leaf_offset + leaf * 2;
        output[start..start + 2].copy_from_slice(&section.to_le_bytes());
    }

    let mut first_edge = 0usize;
    let mut section_payload_offset = payload_offset;
    for (section_index, section) in sections.iter().enumerate() {
        let memory = memories[section_index];
        let start = section_offset + section_index * RENDER_SECTION_RECORD_BYTES;
        output[start..start + 2].copy_from_slice(&(first_edge as u16).to_le_bytes());
        output[start + 2..start + 4]
            .copy_from_slice(&(neighbors[section_index].len() as u16).to_le_bytes());
        output[start + 4..start + 6].copy_from_slice(&section.first_cell.to_le_bytes());
        output[start + 6..start + 8].copy_from_slice(&section.cell_count.to_le_bytes());
        output[start + 8..start + 12].copy_from_slice(&memory.staging_bytes.to_le_bytes());
        output[start + 12..start + 16].copy_from_slice(&memory.activation_bytes.to_le_bytes());
        output[start + 16..start + 20].copy_from_slice(&memory.packet_pool_bytes.to_le_bytes());
        output[start + 20..start + 24].copy_from_slice(&memory.projection_bytes.to_le_bytes());
        output[start + 24..start + 28].copy_from_slice(&section.fallback_bytes.to_le_bytes());
        output[start + 28..start + 32]
            .copy_from_slice(&(section_payload_offset as u32).to_le_bytes());
        output[start + 32..start + 36]
            .copy_from_slice(&memories[section_index].staging_bytes.to_le_bytes());
        output[start + 36..start + 38].copy_from_slice(&section.flags.to_le_bytes());
        for (neighbor_index, neighbor) in neighbors[section_index].iter().enumerate() {
            let edge = edge_offset + (first_edge + neighbor_index) * RENDER_SECTION_EDGE_BYTES;
            output[edge..edge + 2].copy_from_slice(&neighbor.to_le_bytes());
        }
        first_edge += neighbors[section_index].len();
        section_payload_offset += section_payloads[section_index].bytes.len();
    }
    let mut destination = payload_offset;
    for payload in &section_payloads {
        let end = destination + payload.bytes.len();
        output[destination..end].copy_from_slice(&payload.bytes);
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

fn extract_section_payload(
    source: RenderQuadPayload<'_>,
    first_cell: usize,
    cell_count: usize,
) -> Result<RenderQuadPayloadInput, CookError> {
    let end_cell = first_cell
        .checked_add(cell_count)
        .filter(|end| *end <= source.cell_count())
        .ok_or_else(|| CookError::new("render section cell range is invalid"))?;
    let mut templates = BTreeMap::<u16, u32>::new();
    for cell_index in first_cell..end_cell {
        let cell = source
            .cell(cell_index)
            .ok_or_else(|| CookError::new("render section cell is missing"))?;
        for command_index in 0..cell.command_count as usize {
            let command = source
                .command(cell, command_index)
                .ok_or_else(|| CookError::new("render section command is missing"))?;
            *templates.entry(command.object).or_default() |= command.template_faces;
        }
    }
    // Inline brush objects are deliberately absent from camera-cell command
    // streams. They are still part of the renderer-owned topology and must be
    // available in every active section for doors, lifts, and other moving BSP
    // entities. Keeping them template-free preserves the exact dynamic path.
    for object_index in 0..source.object_count() {
        let object = source
            .object(object_index)
            .ok_or_else(|| CookError::new("render section object is missing"))?;
        if object.flags & RENDER_QUAD_OBJECT_SUBMODEL != 0 {
            templates.entry(object_index as u16).or_default();
        }
    }

    let mut output = RenderQuadPayloadInput::default();
    let mut object_remap = BTreeMap::<u16, u16>::new();
    for (&source_object_index, &template_faces) in &templates {
        let object = source
            .object(source_object_index as usize)
            .ok_or_else(|| CookError::new("render section object is missing"))?;
        let target_object_index = count_u16(output.objects.len(), "section render object")?;
        object_remap.insert(source_object_index, target_object_index);
        let first_face = count_u16(output.faces.len(), "section render face")?;
        let first_corner = count_u16(output.corners.len(), "section render corner")?;
        let first_quad = count_u16(output.quads.len(), "section render quad")?;
        let first_position = count_u16(output.positions.len(), "section render position")?;
        let first_run = 0;

        for corner_index in object.first_corner as usize
            ..object.first_corner as usize + object.corner_count as usize
        {
            output.corners.push(
                source
                    .corner(corner_index)
                    .ok_or_else(|| CookError::new("render section corner is missing"))?,
            );
        }
        for position_index in object.first_position as usize
            ..object.first_position as usize + object.position_count as usize
        {
            output.positions.push(
                source
                    .position(position_index)
                    .ok_or_else(|| CookError::new("render section position is missing"))?,
            );
        }

        for local_face in 0..object.face_count as usize {
            let mut face = source
                .face(object.first_face as usize + local_face)
                .ok_or_else(|| CookError::new("render section face is missing"))?;
            face.first_corner = first_corner
                .checked_add(face.first_corner - object.first_corner)
                .ok_or_else(|| CookError::new("render section corner rebase overflow"))?;
            face.first_quad = count_u16(output.quads.len(), "section render quad")?;
            if template_faces & (1 << local_face) != 0 {
                for quad_index in 0..face.quad_count as usize {
                    output.quads.push(
                        source
                            .quad(
                                source
                                    .face(object.first_face as usize + local_face)
                                    .unwrap()
                                    .first_quad as usize
                                    + quad_index,
                            )
                            .ok_or_else(|| CookError::new("render section quad is missing"))?,
                    );
                }
            } else {
                face.quad_count = 0;
            }
            output.faces.push(face);
        }
        output.objects.push(RenderQuadObject {
            first_face,
            face_count: object.face_count,
            first_corner,
            corner_count: object.corner_count,
            first_quad,
            quad_count: count_u16(
                output.quads.len() - first_quad as usize,
                "section object quad",
            )?,
            first_position,
            position_count: object.position_count,
            first_run,
            run_count: 0,
            mins: object.mins,
            maxs: object.maxs,
            flags: object.flags,
        });
    }

    for cell_index in first_cell..end_cell {
        let cell = source
            .cell(cell_index)
            .ok_or_else(|| CookError::new("render section cell is missing"))?;
        let mut commands = Vec::with_capacity(cell.command_count as usize);
        for command_index in 0..cell.command_count as usize {
            let mut command = source
                .command(cell, command_index)
                .ok_or_else(|| CookError::new("render section command is missing"))?;
            command.object = *object_remap
                .get(&command.object)
                .ok_or_else(|| CookError::new("render section object remap is missing"))?;
            commands.push(command);
        }
        output.cells.push(RenderQuadCellInput {
            leaf: cell.leaf,
            flags: cell.flags,
            portal_leaf: cell.portal_leaf,
            portal_plane: cell.portal_plane,
            visibility: source
                .visibility(cell_index)
                .ok_or_else(|| CookError::new("render section visibility is missing"))?
                .to_vec(),
            portal_visibility: source
                .portal_visibility(cell_index)
                .ok_or_else(|| CookError::new("render section portal visibility is missing"))?
                .to_vec(),
            commands,
        });
    }
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

    fn payload_for_leaves(seed: u32, leaves: &[u16]) -> EncodedRenderQuadPayload {
        encode_render_quad_payload(&RenderQuadPayloadInput {
            objects: vec![RenderQuadObject {
                first_face: 0,
                face_count: 1,
                first_corner: 0,
                corner_count: 4,
                first_quad: 0,
                quad_count: 1,
                first_position: 0,
                position_count: 4,
                first_run: 0,
                run_count: 0,
                mins: [-8, -4, 0],
                maxs: [8, 4, 0],
                flags: 0,
            }],
            faces: vec![RenderQuadFace {
                source_face: 3,
                first_corner: 0,
                first_quad: 0,
                quad_count: 1,
                plane: 5,
                material: 7,
                flags: quake_formats::RENDER_QUAD_FACE_BACKSIDE
                    | quake_formats::RENDER_QUAD_FACE_BAKED_UV
                    | quake_formats::RENDER_QUAD_FACE_BAKED_LIGHT,
                corner_count: 4,
                light_styles: [0, 255],
            }],
            corners: (0..4)
                .map(|position| RenderQuadCorner {
                    position,
                    texture: [position, position + 8],
                    light: seed + u32::from(position),
                })
                .collect(),
            quads: vec![RenderQuad {
                positions: [0, 1, 2, 3],
            }],
            positions: vec![[-8, -4, 0], [8, -4, 0], [-8, 4, 0], [8, 4, 0]],
            runs: vec![],
            cells: leaves
                .iter()
                .map(|leaf| RenderQuadCellInput {
                    leaf: *leaf,
                    flags: 0,
                    portal_leaf: u16::MAX,
                    portal_plane: -1,
                    visibility: vec![1],
                    portal_visibility: vec![0],
                    commands: vec![RenderQuadCommand {
                        object: 0,
                        flags: 0,
                        visible_faces: 1,
                        portal_faces: 0,
                        dynamic_faces: 1,
                        template_faces: 1,
                    }],
                })
                .collect(),
        })
        .unwrap()
    }

    fn payload(seed: u32) -> EncodedRenderQuadPayload {
        payload_for_leaves(seed, &[1])
    }

    #[test]
    fn resident_render_cells_keep_one_dictionary_and_bind_one_leaf() {
        let shared = payload_for_leaves(0x1600, &[1, 2]);
        let sidecar = encode_resident_render_cells(3, &shared, 1_000, 100_000, 64 * 1024)
            .unwrap();
        let header = quake_formats::RenderCellHeader::parse(&sidecar, sidecar.len()).unwrap();
        let directory = quake_formats::RenderCellDirectory::parse_prefix(
            &sidecar[..header.directory_bytes()],
            sidecar.len(),
        )
        .unwrap();
        assert_eq!(directory.leaf_count(), 3);
        assert!(directory.cell_location(0).is_none());
        let dictionary_start = header.dictionary_offset();
        let dictionary_end = dictionary_start + header.dictionary_bytes();
        let dictionary = RenderQuadPayload::parse(&sidecar[dictionary_start..dictionary_end])
            .unwrap();
        assert_eq!(dictionary.object_count(), 1);
        assert_eq!(dictionary.cell_count(), 0);
        let (section, cell_offset) = directory.cell_location(2).unwrap();
        let (section_offset, section_bytes) = directory.section_range(section).unwrap();
        let cell_source = &sidecar[section_offset + cell_offset..section_offset + section_bytes];
        let command_count = u16::from_le_bytes(cell_source[2..4].try_into().unwrap()) as usize;
        let cell_bytes = RENDER_QUAD_CELL_BYTES
            + header.visibility_row_bytes() * 2
            + command_count * RENDER_QUAD_COMMAND_BYTES;
        let mut active = sidecar[dictionary_start..dictionary_end].to_vec();
        active.extend_from_slice(&cell_source[..cell_bytes]);
        let active_bytes = header.dictionary_bytes();
        let payload = RenderQuadPayload::bind_single_cell(
            &mut active,
            active_bytes,
            header.visibility_row_bytes(),
            RENDER_QUAD_PACKET_BYTES as u32,
        )
        .unwrap();
        assert_eq!(payload.cell_count(), 1);
        assert_eq!(payload.cell(0).unwrap().leaf, 2);
        assert_eq!(payload.command(payload.cell(0).unwrap(), 0).unwrap().object, 0);
    }

    fn submodel_payload() -> EncodedRenderQuadPayload {
        encode_render_quad_payload(&RenderQuadPayloadInput {
            objects: vec![RenderQuadObject {
                first_face: 0,
                face_count: 1,
                first_corner: 0,
                corner_count: 4,
                first_quad: 0,
                quad_count: 0,
                first_position: 0,
                position_count: 4,
                first_run: 0,
                run_count: 0,
                mins: [-8, -4, 0],
                maxs: [8, 4, 0],
                flags: quake_formats::RENDER_QUAD_OBJECT_SUBMODEL,
            }],
            faces: vec![RenderQuadFace {
                source_face: 9,
                first_corner: 0,
                first_quad: 0,
                quad_count: 0,
                plane: 5,
                material: 7,
                flags: quake_formats::RENDER_QUAD_FACE_BAKED_UV
                    | quake_formats::RENDER_QUAD_FACE_BAKED_LIGHT,
                corner_count: 4,
                light_styles: [0, 255],
            }],
            corners: (0..4)
                .map(|position| RenderQuadCorner {
                    position,
                    texture: [position, position + 8],
                    light: 0x404040,
                })
                .collect(),
            quads: Vec::new(),
            positions: vec![[-8, -4, 0], [8, -4, 0], [-8, 4, 0], [8, 4, 0]],
            runs: Vec::new(),
            cells: vec![RenderQuadCellInput {
                leaf: 1,
                flags: 0,
                portal_leaf: u16::MAX,
                portal_plane: -1,
                visibility: vec![1],
                portal_visibility: vec![0],
                commands: Vec::new(),
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
        assert_eq!(parsed.corner_count(), 4);
        assert_eq!(parsed.quad_count(), 1);
        assert_eq!(parsed.position_count(), 4);
        assert_eq!(parsed.packet_pool_bytes(), 52);
        assert_eq!(parsed.projection_bytes(), 32);
        assert_eq!(parsed.runtime_metadata_bytes(), 158);
        assert_eq!(parsed.visibility(0).unwrap(), &[1]);
        assert_eq!(parsed.portal_visibility(0).unwrap(), &[0]);
        assert_eq!(parsed.face(0).unwrap().plane, 5);
        assert_eq!(parsed.corner(3).unwrap().texture, [3, 11]);
        assert_eq!(parsed.quad(0).unwrap().positions, [0, 1, 2, 3]);
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
    fn quad_payload_roundtrips_exact_water_portal_rows_and_masks() {
        let mut encoded = payload(0x1810);
        let parsed = RenderQuadPayload::parse(&encoded.bytes).unwrap();
        let stream = parsed.streams_offset();
        let cells = u32::from_le_bytes(encoded.bytes[48..52].try_into().unwrap()) as usize;
        encoded.bytes[cells + 8..cells + 10]
            .copy_from_slice(&quake_formats::RENDER_QUAD_CELL_WATER_PORTAL.to_le_bytes());
        encoded.bytes[cells + 10..cells + 12].copy_from_slice(&2u16.to_le_bytes());
        encoded.bytes[cells + 12..cells + 14].copy_from_slice(&5i16.to_le_bytes());
        encoded.bytes[cells + RENDER_QUAD_CELL_BYTES + 1] = 2;
        encoded.bytes[stream + 4..stream + 8].copy_from_slice(&0u32.to_le_bytes());
        encoded.bytes[stream + 8..stream + 12].copy_from_slice(&1u32.to_le_bytes());

        let parsed = RenderQuadPayload::parse(&encoded.bytes).unwrap();
        let cell = parsed.cell(0).unwrap();
        let command = parsed.command(cell, 0).unwrap();
        assert_eq!(cell.portal_leaf, 2);
        assert_eq!(cell.portal_plane, 5);
        assert_eq!(parsed.visibility(0).unwrap(), &[1]);
        assert_eq!(parsed.portal_visibility(0).unwrap(), &[2]);
        assert_eq!(command.visible_faces, 0);
        assert_eq!(command.portal_faces, 1);
    }

    #[test]
    fn quad_payload_rejects_portal_faces_without_a_portal_cell() {
        let mut encoded = payload(0x1820);
        let stream = RenderQuadPayload::parse(&encoded.bytes)
            .unwrap()
            .streams_offset();
        encoded.bytes[stream + 4..stream + 8].copy_from_slice(&0u32.to_le_bytes());
        encoded.bytes[stream + 8..stream + 12].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            RenderQuadPayload::parse(&encoded.bytes),
            Err(quake_formats::RenderQuadPayloadError::BadFaceMask)
        );
    }

    #[test]
    fn quad_payload_rejects_a_duplicate_spatial_cell() {
        let mut encoded = payload_for_leaves(0x1830, &[1, 2]);
        let cells = u32::from_le_bytes(encoded.bytes[48..52].try_into().unwrap()) as usize;
        encoded.bytes[cells + RENDER_QUAD_CELL_BYTES..cells + RENDER_QUAD_CELL_BYTES + 2]
            .copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            RenderQuadPayload::parse(&encoded.bytes),
            Err(quake_formats::RenderQuadPayloadError::BadCellOrder)
        );
    }

    #[test]
    fn fallback_only_section_retains_topology_without_installing_templates() {
        let mut encoded = payload(0x1900);
        let stream = RenderQuadPayload::parse(&encoded.bytes)
            .unwrap()
            .streams_offset();
        encoded.bytes[stream + 16..stream + 20].fill(0);
        let parsed = RenderQuadPayload::parse(&encoded.bytes).unwrap();
        assert_eq!(parsed.face(0).unwrap().corner_count, 4);
        assert_eq!(
            parsed.face(0).unwrap().flags,
            quake_formats::RENDER_QUAD_FACE_BACKSIDE
                | quake_formats::RENDER_QUAD_FACE_BAKED_UV
                | quake_formats::RENDER_QUAD_FACE_BAKED_LIGHT
        );
        assert_eq!(parsed.corner(2).unwrap().position, 2);
        assert_eq!(
            parsed.section_memory(0, 1).unwrap(),
            quake_formats::RenderQuadSectionMemory {
                staging_bytes: 218,
                activation_bytes: 186,
                packet_pool_bytes: 0,
                projection_bytes: 32,
            }
        );
    }

    #[test]
    fn submodel_fallback_is_resident_but_never_cell_referenced() {
        let encoded = submodel_payload();
        let parsed = RenderQuadPayload::parse(&encoded.bytes).unwrap();
        assert_eq!(parsed.resident_object_bytes(), Some(116));
        assert_eq!(parsed.object(0).unwrap().quad_count, 0);
        assert_eq!(parsed.cell(0).unwrap().command_count, 0);
        let memory = parsed.section_memory(0, 1).unwrap();
        let sections = [RenderSectionInput {
            first_cell: 0,
            cell_count: 1,
            fallback_bytes: 116,
            ..RenderSectionInput::default()
        }];
        let section_bytes = encode_render_sections(
            &[RENDER_SECTION_NONE, 0],
            &encoded,
            &sections,
            1,
            memory.activation_bytes,
            0,
        )
        .unwrap();
        let directory = RenderSectionDirectory::parse(&section_bytes).unwrap();
        let section_payload = directory.payload(0).unwrap();
        assert_eq!(section_payload.object_count(), 1);
        assert_eq!(
            section_payload.object(0).unwrap().flags,
            RENDER_QUAD_OBJECT_SUBMODEL
        );
        assert_eq!(section_payload.cell(0).unwrap().command_count, 0);

        let mut invalid = encoded;
        let stream = RenderQuadPayload::parse(&invalid.bytes)
            .unwrap()
            .streams_offset();
        let cells_offset = u32::from_le_bytes(invalid.bytes[48..52].try_into().unwrap()) as usize;
        invalid.bytes[cells_offset + 2..cells_offset + 4].copy_from_slice(&1u16.to_le_bytes());
        invalid
            .bytes
            .extend_from_slice(&[0; quake_formats::RENDER_QUAD_COMMAND_BYTES]);
        invalid.bytes[stream..stream + 2].copy_from_slice(&0u16.to_le_bytes());
        let file_bytes = invalid.bytes.len() as u32;
        invalid.bytes[56..60].copy_from_slice(&file_bytes.to_le_bytes());
        let runtime_bytes =
            invalid.runtime_metadata_bytes + quake_formats::RENDER_QUAD_COMMAND_BYTES as u32;
        invalid.bytes[68..72].copy_from_slice(&runtime_bytes.to_le_bytes());
        assert_eq!(
            RenderQuadPayload::parse(&invalid.bytes),
            Err(quake_formats::RenderQuadPayloadError::BadCellStream)
        );
    }

    #[test]
    fn directory_proves_windowed_staging_and_activation_budgets() {
        let payload = payload_for_leaves(0x2000, &[1, 2]);
        let sections = [
            RenderSectionInput {
                neighbors: vec![1],
                first_cell: 0,
                cell_count: 1,
                fallback_bytes: 64,
                flags: 0,
            },
            RenderSectionInput {
                neighbors: vec![0],
                first_cell: 1,
                cell_count: 1,
                fallback_bytes: 64,
                flags: 0,
            },
        ];
        let parsed = RenderQuadPayload::parse(&payload.bytes).unwrap();
        let memory = parsed.section_memory(0, 1).unwrap();
        let budget = memory.activation_bytes;
        let bytes = encode_render_sections(
            &[RENDER_SECTION_NONE, 0, 1],
            &payload,
            &sections,
            335_000,
            budget,
            120 * 1024,
        )
        .unwrap();
        let directory = RenderSectionDirectory::parse(&bytes).unwrap();
        assert_eq!(directory.resident_core_bytes(), 335_000);
        assert_eq!(directory.streaming_budget_bytes(), budget as u32);
        assert_eq!(directory.packet_pool_budget_bytes(), 120 * 1024);
        assert_eq!(directory.leaf_section(0), None);
        assert_eq!(directory.leaf_section(2), Some(1));
        assert_eq!(directory.edge(0).unwrap().neighbor, 1);
        let header = quake_formats::RenderSectionHeader::parse(
            &bytes[..RENDER_SECTION_HEADER_BYTES],
            bytes.len(),
        )
        .unwrap();
        let section_bytes = (0..directory.section_count())
            .map(|section| directory.section(section).unwrap().payload_bytes as usize)
            .sum::<usize>();
        assert_eq!(bytes.len(), header.directory_bytes() + section_bytes);
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
        assert_eq!(directory.payload(0).unwrap().cell_count(), 1);
        assert_eq!(directory.payload(1).unwrap().cell_count(), 1);
    }

    #[test]
    fn directory_rejects_an_activation_over_budget() {
        let payload = payload_for_leaves(0x4000, &[0, 1]);
        let sections = [
            RenderSectionInput {
                neighbors: vec![1],
                first_cell: 0,
                cell_count: 1,
                ..RenderSectionInput::default()
            },
            RenderSectionInput {
                neighbors: vec![0],
                first_cell: 1,
                cell_count: 1,
                ..RenderSectionInput::default()
            },
        ];
        assert!(encode_render_sections(&[0, 1], &payload, &sections, 1, 1, 1).is_err());
    }

    #[test]
    fn directory_caps_the_installed_pool_without_summing_fallback_candidates() {
        let payload = payload(0x6000);
        let sections = [RenderSectionInput {
            first_cell: 0,
            cell_count: 1,
            fallback_bytes: 32,
            ..RenderSectionInput::default()
        }];
        assert!(encode_render_sections(
            &[RENDER_SECTION_NONE, 0],
            &payload,
            &sections,
            1,
            1024,
            52
        )
        .is_ok());
        assert!(encode_render_sections(
            &[RENDER_SECTION_NONE, 0],
            &payload,
            &sections,
            1,
            1024,
            51
        )
        .is_err());
    }
}
