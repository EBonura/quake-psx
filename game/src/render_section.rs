//! Resident QRP5 dictionary plus cached QRC2 camera-cell activation.

use psx_bsp::resident::ResidentMap as SharedResidentMap;
use quake_formats::{RenderCellDirectory, RenderCellHeader, RenderQuadPayload};

use crate::asset::MapLoadError;
use crate::platform;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GatheredRenderSection {
    pub section: u16,
    pub leaf: u16,
    pub payload_bytes: u32,
}

/// Read the compact world dictionary once as part of cold map loading.
pub fn load_render_dictionary(
    chunk_id: u32,
    header: RenderCellHeader,
    plane_count: usize,
    material_count: usize,
    arena: &mut SharedResidentMap,
) -> Result<(), MapLoadError> {
    if !arena.resize_arena_tail(header.dictionary_bytes()) {
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::debug_log("quake-psx: QRC dictionary exceeds arena tail");
        return Err(MapLoadError::TooLarge);
    }
    {
        let mut stream = platform::ChunkStream::open_at(chunk_id, header.dictionary_offset() as u32)
            .map_err(MapLoadError::Storage)?;
        stream
            .read_exact_at(header.dictionary_offset() as u32, arena.arena_tail_mut())
            .map_err(MapLoadError::Storage)?;
    }
    let payload = RenderQuadPayload::parse(arena.arena_tail()).map_err(|_| MapLoadError::Format)?;
    if payload.cell_count() != 0 || payload.visibility_row_bytes() != 0 {
        return Err(MapLoadError::Format);
    }
    validate_dictionary(payload, plane_count, material_count)
}

/// Append one exact leaf block behind the resident dictionary. This is the
/// only gameplay read: commands, base PVS and portal PVS are contiguous and
/// normally occupy one ISO sector.
pub fn load_render_cell(
    chunk_id: u32,
    index: RenderCellDirectory<'_>,
    leaf: usize,
    cached_section: Option<usize>,
    plane_count: usize,
    material_count: usize,
    arena: &mut SharedResidentMap,
) -> Result<GatheredRenderSection, MapLoadError> {
    let header = index.header();
    let (section_index, cell_offset) = index
        .cell_location(leaf)
        .ok_or(MapLoadError::Format)?;
    let (source_offset, section_bytes) = index
        .section_range(section_index)
        .ok_or(MapLoadError::Format)?;
    let cache_start = header
        .dictionary_bytes()
        .checked_add(header.max_cell_bytes())
        .ok_or(MapLoadError::TooLarge)?;
    let total_bytes = cache_start
        .checked_add(section_bytes)
        .ok_or(MapLoadError::TooLarge)?;
    if section_bytes > header.max_section_bytes() || !arena.resize_arena_tail(total_bytes) {
        return Err(MapLoadError::TooLarge);
    }
    if cached_section != Some(section_index) {
        let destination = arena
            .arena_tail_mut()
            .get_mut(cache_start..total_bytes)
            .ok_or(MapLoadError::TooLarge)?;
        let mut stream = platform::ChunkStream::open_at(chunk_id, source_offset as u32)
            .map_err(MapLoadError::Storage)?;
        stream
            .read_exact_at(source_offset as u32, destination)
            .map_err(MapLoadError::Storage)?;
    }
    let source_start = cache_start
        .checked_add(cell_offset)
        .ok_or(MapLoadError::Format)?;
    let minimum_cell_bytes = quake_formats::RENDER_QUAD_CELL_BYTES
        .checked_add(header.visibility_row_bytes() * 2)
        .ok_or(MapLoadError::Format)?;
    let cell_header = arena
        .arena_tail()
        .get(source_start..source_start + quake_formats::RENDER_QUAD_CELL_BYTES)
        .ok_or(MapLoadError::Format)?;
    let encoded_leaf = u16::from_le_bytes([cell_header[0], cell_header[1]]) as usize;
    let command_count = u16::from_le_bytes([cell_header[2], cell_header[3]]) as usize;
    let cell_bytes = minimum_cell_bytes
        .checked_add(
            command_count
                .checked_mul(quake_formats::RENDER_QUAD_COMMAND_BYTES)
                .ok_or(MapLoadError::Format)?,
        )
        .ok_or(MapLoadError::Format)?;
    if encoded_leaf != leaf
        || cell_bytes > header.max_cell_bytes()
        || cell_offset
            .checked_add(cell_bytes)
            .is_none_or(|end| end > section_bytes)
    {
        return Err(MapLoadError::Format);
    }
    let payload_bytes = header
        .dictionary_bytes()
        .checked_add(cell_bytes)
        .ok_or(MapLoadError::TooLarge)?;
    arena
        .arena_tail_mut()
        .copy_within(source_start..source_start + cell_bytes, header.dictionary_bytes());
    let _payload = RenderQuadPayload::bind_single_cell(
        &mut arena.arena_tail_mut()[..payload_bytes],
        header.dictionary_bytes(),
        header.visibility_row_bytes(),
        0,
    )
    .map_err(|_| MapLoadError::Format)?;
    let cell = payload.cell(0).ok_or(MapLoadError::Format)?;
    if cell.leaf as usize != leaf {
        return Err(MapLoadError::Format);
    }
    let mut packet_pool_bytes = 0usize;
    for command_index in 0..cell.command_count as usize {
        let command = payload.command(cell, command_index).ok_or(MapLoadError::Format)?;
        let object = payload
            .object(command.object as usize)
            .ok_or(MapLoadError::Format)?;
        for local_face in 0..object.face_count as usize {
            if command.template_faces & (1 << local_face) != 0 {
                packet_pool_bytes = packet_pool_bytes
                    .checked_add(
                        payload
                            .face(object.first_face as usize + local_face)
                            .ok_or(MapLoadError::Format)?
                            .quad_count as usize
                            * quake_formats::RENDER_QUAD_PACKET_BYTES,
                    )
                    .ok_or(MapLoadError::TooLarge)?;
            }
        }
    }
    if packet_pool_bytes > header.packet_pool_budget_bytes() as usize
        || packet_pool_bytes > header.max_packet_pool_bytes() as usize
    {
        return Err(MapLoadError::TooLarge);
    }
    let payload = RenderQuadPayload::bind_single_cell(
        &mut arena.arena_tail_mut()[..payload_bytes],
        header.dictionary_bytes(),
        header.visibility_row_bytes(),
        packet_pool_bytes as u32,
    )
    .map_err(|_| MapLoadError::Format)?;
    // The immutable dictionary was semantically validated once during cold
    // map loading. The newly read cell and all of its object/face references
    // have been checked above; rescanning every dictionary face here was a
    // measurable gameplay hot spot.
    let _ = (plane_count, material_count);
    Ok(GatheredRenderSection {
        section: section_index as u16,
        leaf: leaf as u16,
        payload_bytes: payload_bytes as u32,
    })
}

fn validate_dictionary(
    payload: RenderQuadPayload<'_>,
    plane_count: usize,
    material_count: usize,
) -> Result<(), MapLoadError> {
    for face_index in 0..payload.face_count() {
        let face = payload.face(face_index).ok_or(MapLoadError::Format)?;
        if face.plane as usize >= plane_count || face.material as usize >= material_count {
            return Err(MapLoadError::Format);
        }
    }
    Ok(())
}
