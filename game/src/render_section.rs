//! Resident QRP5 dictionary plus cached QRC2 camera-cell activation.

use psx_bsp::resident::ResidentMap as SharedResidentMap;
use quake_formats::{RenderCellDirectory, RenderCellHeader, RenderQuadPayload};

use crate::asset::MapLoadError;
use crate::platform;

/// Metadata bound for the resident QRC2 section cache. Section bytes live in
/// the already allocated map arena, so this array is only a compact tag set.
pub const RENDER_SECTION_CACHE_MAX_SLOTS: usize = 64;

/// Fully associative FIFO tags for section slots in the resident-map tail.
/// A hit avoids all CD traffic; a miss overwrites one fixed-size arena slot.
pub struct RenderSectionCache {
    tags: [u16; RENDER_SECTION_CACHE_MAX_SLOTS],
    slot_count: u8,
    next_slot: u8,
}

impl RenderSectionCache {
    pub const fn new() -> Self {
        Self {
            tags: [u16::MAX; RENDER_SECTION_CACHE_MAX_SLOTS],
            slot_count: 0,
            next_slot: 0,
        }
    }

    pub fn configure(&mut self, slot_count: usize) -> bool {
        if slot_count == 0 || slot_count > RENDER_SECTION_CACHE_MAX_SLOTS {
            return false;
        }
        self.tags.fill(u16::MAX);
        self.slot_count = slot_count as u8;
        self.next_slot = 0;
        true
    }

    pub fn clear(&mut self) {
        self.tags.fill(u16::MAX);
        self.slot_count = 0;
        self.next_slot = 0;
    }

    /// Return the resident slot when present, or the next replacement slot.
    /// Misses are committed only after the section read and cell validation
    /// succeed, so a failed CD transfer never publishes partial bytes.
    pub fn select(&mut self, section: u16) -> Option<(usize, bool)> {
        let count = self.slot_count as usize;
        if count == 0 {
            return None;
        }
        if let Some(slot) = self.tags[..count]
            .iter()
            .position(|&cached| cached == section)
        {
            return Some((slot, true));
        }
        let slot = self.next_slot as usize;
        self.next_slot = (slot + 1).rem_euclid(count) as u8;
        Some((slot, false))
    }

    pub fn commit(&mut self, slot: usize, section: u16) -> bool {
        let Some(tag) = self.tags.get_mut(slot) else {
            return false;
        };
        if slot >= self.slot_count as usize {
            return false;
        }
        *tag = section;
        true
    }

    pub fn invalidate(&mut self, slot: usize) {
        if slot < self.slot_count as usize {
            self.tags[slot] = u16::MAX;
        }
    }
}

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
) -> Result<usize, MapLoadError> {
    if !arena.resize_arena_tail(header.dictionary_bytes()) {
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::debug_log("quake-psx: QRC dictionary exceeds arena tail");
        return Err(MapLoadError::TooLarge);
    }
    {
        let mut stream =
            platform::ChunkStream::open_at(chunk_id, header.dictionary_offset() as u32)
                .map_err(MapLoadError::Storage)?;
        stream
            .read_exact_at(header.dictionary_offset() as u32, arena.arena_tail_mut())
            .map_err(MapLoadError::Storage)?;
    }
    let payload = RenderQuadPayload::parse(arena.arena_tail()).map_err(|_| MapLoadError::Format)?;
    if payload.cell_count() != 0 || payload.visibility_row_bytes() != 0 {
        return Err(MapLoadError::Format);
    }
    validate_dictionary(payload, plane_count, material_count)?;

    // Use the map arena's already allocated headroom as a multi-section cache.
    // The active cell still occupies its bounded staging range immediately
    // after the dictionary, while fixed-size cached sections fill the rest.
    let cache_start = header
        .dictionary_bytes()
        .checked_add(header.max_cell_bytes())
        .ok_or(MapLoadError::TooLarge)?;
    let tail_capacity = arena
        .arena_capacity()
        .checked_sub(arena.resident_bytes_len())
        .ok_or(MapLoadError::TooLarge)?;
    let cache_bytes = tail_capacity
        .checked_sub(cache_start)
        .ok_or(MapLoadError::TooLarge)?;
    let slot_count = (cache_bytes / header.max_section_bytes())
        .min(header.section_count())
        .min(RENDER_SECTION_CACHE_MAX_SLOTS);
    if slot_count == 0 {
        return Err(MapLoadError::TooLarge);
    }
    let tail_bytes = cache_start
        .checked_add(
            slot_count
                .checked_mul(header.max_section_bytes())
                .ok_or(MapLoadError::TooLarge)?,
        )
        .ok_or(MapLoadError::TooLarge)?;
    if !arena.resize_arena_tail(tail_bytes) {
        return Err(MapLoadError::TooLarge);
    }
    Ok(slot_count)
}

/// Bind one exact leaf block behind the resident dictionary. On a cache miss,
/// commands, base PVS and portal PVS are read together into one arena slot.
/// Cache hits reuse those immutable section bytes without touching the CD.
pub fn load_render_cell(
    chunk_id: u32,
    index: RenderCellDirectory<'_>,
    leaf: usize,
    cache_slot: usize,
    cache_hit: bool,
    plane_count: usize,
    material_count: usize,
    arena: &mut SharedResidentMap,
) -> Result<GatheredRenderSection, MapLoadError> {
    let header = index.header();
    let (section_index, cell_offset) = index.cell_location(leaf).ok_or(MapLoadError::Format)?;
    let (source_offset, section_bytes) = index
        .section_range(section_index)
        .ok_or(MapLoadError::Format)?;
    let cache_start = header
        .dictionary_bytes()
        .checked_add(header.max_cell_bytes())
        .ok_or(MapLoadError::TooLarge)?;
    let slot_start = cache_start
        .checked_add(
            cache_slot
                .checked_mul(header.max_section_bytes())
                .ok_or(MapLoadError::TooLarge)?,
        )
        .ok_or(MapLoadError::TooLarge)?;
    let slot_end = slot_start
        .checked_add(header.max_section_bytes())
        .ok_or(MapLoadError::TooLarge)?;
    if section_bytes > header.max_section_bytes() || slot_end > arena.arena_tail().len() {
        return Err(MapLoadError::TooLarge);
    }
    if !cache_hit {
        let destination = arena
            .arena_tail_mut()
            .get_mut(slot_start..slot_start + section_bytes)
            .ok_or(MapLoadError::TooLarge)?;
        let mut stream = platform::ChunkStream::open_at(chunk_id, source_offset as u32)
            .map_err(MapLoadError::Storage)?;
        stream
            .read_exact_at(source_offset as u32, destination)
            .map_err(MapLoadError::Storage)?;
    }
    let source_start = slot_start
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
    arena.arena_tail_mut().copy_within(
        source_start..source_start + cell_bytes,
        header.dictionary_bytes(),
    );
    let payload = RenderQuadPayload::bind_single_cell(
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
        let command = payload
            .command(cell, command_index)
            .ok_or(MapLoadError::Format)?;
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
    let _payload = RenderQuadPayload::bind_single_cell(
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
