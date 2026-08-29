//! Resident world dictionary plus bounded spatial camera-cell sections.
//!
//! `QRC2` is the renderer-owned residency boundary inferred from Quake II PSX:
//! compact object/face/corner/position topology is read once with the map,
//! while spatially adjacent camera leaves share one bounded command section.
//! Moving among leaves in the active section performs no storage I/O.

use core::convert::TryInto;

pub const RENDER_CELL_MAGIC: u32 = u32::from_le_bytes(*b"QRC2");
pub const RENDER_CELL_VERSION: u16 = 2;
pub const RENDER_CELL_HEADER_BYTES: usize = 64;
pub const RENDER_CELL_OFFSET_BYTES: usize = 4;
pub const RENDER_CELL_NONE: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderCellError {
    TooSmall,
    BadMagic,
    BadVersion,
    BadHeaderSize,
    BadOffsetSize,
    BadFileSize,
    NonCanonicalLayout,
    BadCellRange,
    BadMemoryBudget,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderCellHeader {
    leaf_count: u16,
    section_count: u16,
    visibility_row_bytes: u16,
    leaf_records_offset: u32,
    section_offsets_offset: u32,
    dictionary_offset: u32,
    dictionary_bytes: u32,
    cells_offset: u32,
    file_bytes: u32,
    resident_core_bytes: u32,
    resident_high_water_bytes: u32,
    packet_pool_budget_bytes: u32,
    max_section_bytes: u32,
    max_cell_bytes: u32,
    max_packet_pool_bytes: u32,
}

impl RenderCellHeader {
    pub fn parse(bytes: &[u8], file_bytes: usize) -> Result<Self, RenderCellError> {
        let header = bytes
            .get(..RENDER_CELL_HEADER_BYTES)
            .ok_or(RenderCellError::TooSmall)?;
        if u32_at(header, 0) != RENDER_CELL_MAGIC {
            return Err(RenderCellError::BadMagic);
        }
        if u16_at(header, 4) != RENDER_CELL_VERSION {
            return Err(RenderCellError::BadVersion);
        }
        if u16_at(header, 6) as usize != RENDER_CELL_HEADER_BYTES {
            return Err(RenderCellError::BadHeaderSize);
        }
        if u16_at(header, 14) as usize != RENDER_CELL_OFFSET_BYTES {
            return Err(RenderCellError::BadOffsetSize);
        }
        let encoded_file_bytes = u32_at(header, 36);
        if encoded_file_bytes as usize != file_bytes {
            return Err(RenderCellError::BadFileSize);
        }
        let decoded = Self {
            leaf_count: u16_at(header, 8),
            section_count: u16_at(header, 10),
            visibility_row_bytes: u16_at(header, 12),
            leaf_records_offset: u32_at(header, 16),
            section_offsets_offset: u32_at(header, 20),
            dictionary_offset: u32_at(header, 24),
            dictionary_bytes: u32_at(header, 28),
            cells_offset: u32_at(header, 32),
            file_bytes: encoded_file_bytes,
            resident_core_bytes: u32_at(header, 40),
            resident_high_water_bytes: u32_at(header, 44),
            packet_pool_budget_bytes: u32_at(header, 48),
            max_section_bytes: u32_at(header, 52),
            max_cell_bytes: u32_at(header, 56),
            max_packet_pool_bytes: u32_at(header, 60),
        };
        let leaf_end = decoded
            .leaf_records_offset()
            .checked_add(
                decoded
                    .leaf_count()
                    .checked_mul(RENDER_CELL_OFFSET_BYTES)
                    .ok_or(RenderCellError::NonCanonicalLayout)?,
            )
            .ok_or(RenderCellError::NonCanonicalLayout)?;
        let section_end = decoded
            .section_offsets_offset()
            .checked_add(
                decoded
                    .section_count()
                    .checked_add(1)
                    .and_then(|count| count.checked_mul(RENDER_CELL_OFFSET_BYTES))
                    .ok_or(RenderCellError::NonCanonicalLayout)?,
            )
            .ok_or(RenderCellError::NonCanonicalLayout)?;
        let minimum_high_water = decoded
            .resident_core_bytes()
            .checked_add(decoded.dictionary_bytes() as u32)
            .and_then(|bytes| bytes.checked_add(decoded.max_cell_bytes() as u32))
            .and_then(|bytes| bytes.checked_add(decoded.max_section_bytes() as u32))
            .ok_or(RenderCellError::BadMemoryBudget)?;
        if decoded.leaf_count() < 2
            || decoded.section_count() == 0
            || decoded.visibility_row_bytes() == 0
            || decoded.leaf_records_offset() != RENDER_CELL_HEADER_BYTES
            || decoded.section_offsets_offset() != align_up_4(leaf_end)
            || decoded.dictionary_offset() != align_up_4(section_end)
            || decoded.dictionary_bytes() == 0
            || decoded.cells_offset()
                != decoded
                    .dictionary_offset()
                    .checked_add(decoded.dictionary_bytes())
                    .ok_or(RenderCellError::NonCanonicalLayout)?
            || decoded.cells_offset() >= file_bytes
            || decoded.max_section_bytes() == 0
            || decoded.max_cell_bytes() == 0
            || decoded.max_cell_bytes() > decoded.max_section_bytes()
            || decoded.max_packet_pool_bytes() > decoded.packet_pool_budget_bytes()
            || decoded.resident_high_water_bytes() != minimum_high_water
        {
            return Err(RenderCellError::NonCanonicalLayout);
        }
        Ok(decoded)
    }

    pub const fn leaf_count(self) -> usize {
        self.leaf_count as usize
    }

    pub const fn section_count(self) -> usize {
        self.section_count as usize
    }

    pub const fn visibility_row_bytes(self) -> usize {
        self.visibility_row_bytes as usize
    }

    pub const fn leaf_records_offset(self) -> usize {
        self.leaf_records_offset as usize
    }

    pub const fn section_offsets_offset(self) -> usize {
        self.section_offsets_offset as usize
    }

    pub const fn dictionary_offset(self) -> usize {
        self.dictionary_offset as usize
    }

    pub const fn dictionary_bytes(self) -> usize {
        self.dictionary_bytes as usize
    }

    pub const fn cells_offset(self) -> usize {
        self.cells_offset as usize
    }

    pub const fn file_bytes(self) -> usize {
        self.file_bytes as usize
    }

    pub const fn resident_core_bytes(self) -> u32 {
        self.resident_core_bytes
    }

    pub const fn resident_high_water_bytes(self) -> u32 {
        self.resident_high_water_bytes
    }

    pub const fn packet_pool_budget_bytes(self) -> u32 {
        self.packet_pool_budget_bytes
    }

    pub const fn max_section_bytes(self) -> usize {
        self.max_section_bytes as usize
    }

    pub const fn max_cell_bytes(self) -> usize {
        self.max_cell_bytes as usize
    }

    pub const fn max_packet_pool_bytes(self) -> u32 {
        self.max_packet_pool_bytes
    }

    pub const fn directory_bytes(self) -> usize {
        self.dictionary_offset as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderCellDirectory<'a> {
    header: RenderCellHeader,
    leaf_records: &'a [u8],
    section_offsets: &'a [u8],
}

impl<'a> RenderCellDirectory<'a> {
    pub fn parse_prefix(prefix: &'a [u8], file_bytes: usize) -> Result<Self, RenderCellError> {
        let header = RenderCellHeader::parse(prefix, file_bytes)?;
        if prefix.len() != header.directory_bytes() {
            return Err(RenderCellError::NonCanonicalLayout);
        }
        let leaf_records = prefix
            .get(header.leaf_records_offset()..header.section_offsets_offset())
            .ok_or(RenderCellError::BadCellRange)?;
        let section_offsets = prefix
            .get(header.section_offsets_offset()..header.directory_bytes())
            .ok_or(RenderCellError::BadCellRange)?;
        let first = offset_at(section_offsets, 0).ok_or(RenderCellError::BadCellRange)? as usize;
        let last = offset_at(section_offsets, header.section_count())
            .ok_or(RenderCellError::BadCellRange)? as usize;
        if first != header.cells_offset() || last != file_bytes {
            return Err(RenderCellError::BadCellRange);
        }
        let mut previous = first;
        for section in 0..header.section_count() {
            let start =
                offset_at(section_offsets, section).ok_or(RenderCellError::BadCellRange)? as usize;
            let end = offset_at(section_offsets, section + 1)
                .ok_or(RenderCellError::BadCellRange)? as usize;
            if start != previous
                || end <= start
                || end > file_bytes
                || end - start > header.max_section_bytes()
            {
                return Err(RenderCellError::BadCellRange);
            }
            previous = end;
        }
        for leaf in 0..header.leaf_count() {
            let (section, cell_offset) =
                leaf_record_at(leaf_records, leaf).ok_or(RenderCellError::BadCellRange)?;
            if leaf == 0 {
                if section != RENDER_CELL_NONE || cell_offset != 0 {
                    return Err(RenderCellError::BadCellRange);
                }
                continue;
            }
            if section == RENDER_CELL_NONE || section as usize >= header.section_count() {
                return Err(RenderCellError::BadCellRange);
            }
            let (_, section_bytes) = section_range(header, section_offsets, section as usize)
                .ok_or(RenderCellError::BadCellRange)?;
            let minimum_cell_bytes = 16usize
                .checked_add(header.visibility_row_bytes() * 2)
                .ok_or(RenderCellError::BadCellRange)?;
            if cell_offset as usize + minimum_cell_bytes > section_bytes {
                return Err(RenderCellError::BadCellRange);
            }
        }
        Ok(Self {
            header,
            leaf_records,
            section_offsets,
        })
    }

    /// Rebind a directory which was already fully validated at map load.
    ///
    /// The prefix remains owned by the resident map and is never mutated.
    /// Camera movement therefore only needs these two checked slices; walking
    /// every leaf and section again would turn a constant-time lookup into a
    /// major gameplay cost.
    pub fn bind_validated_prefix(
        prefix: &'a [u8],
        header: RenderCellHeader,
    ) -> Result<Self, RenderCellError> {
        if prefix.len() != header.directory_bytes() {
            return Err(RenderCellError::NonCanonicalLayout);
        }
        let leaf_records = prefix
            .get(header.leaf_records_offset()..header.section_offsets_offset())
            .ok_or(RenderCellError::BadCellRange)?;
        let section_offsets = prefix
            .get(header.section_offsets_offset()..header.directory_bytes())
            .ok_or(RenderCellError::BadCellRange)?;
        Ok(Self {
            header,
            leaf_records,
            section_offsets,
        })
    }

    pub const fn header(self) -> RenderCellHeader {
        self.header
    }

    pub const fn leaf_count(self) -> usize {
        self.header.leaf_count()
    }

    pub const fn section_count(self) -> usize {
        self.header.section_count()
    }

    pub fn cell_location(self, leaf: usize) -> Option<(usize, usize)> {
        if leaf == 0 || leaf >= self.header.leaf_count() {
            return None;
        }
        let (section, offset) = leaf_record_at(self.leaf_records, leaf)?;
        (section != RENDER_CELL_NONE && (section as usize) < self.header.section_count())
            .then_some((section as usize, offset as usize))
    }

    pub fn section_range(self, section: usize) -> Option<(usize, usize)> {
        section_range(self.header, self.section_offsets, section)
    }
}

fn section_range(
    header: RenderCellHeader,
    offsets: &[u8],
    section: usize,
) -> Option<(usize, usize)> {
    if section >= header.section_count() {
        return None;
    }
    let start = offset_at(offsets, section)? as usize;
    let end = offset_at(offsets, section + 1)? as usize;
    (end > start && end <= header.file_bytes()).then_some((start, end - start))
}

fn leaf_record_at(bytes: &[u8], index: usize) -> Option<(u16, u16)> {
    let start = index.checked_mul(RENDER_CELL_OFFSET_BYTES)?;
    let bytes = bytes.get(start..start + RENDER_CELL_OFFSET_BYTES)?;
    Some((u16_at(bytes, 0), u16_at(bytes, 2)))
}

fn offset_at(bytes: &[u8], index: usize) -> Option<u32> {
    let start = index.checked_mul(RENDER_CELL_OFFSET_BYTES)?;
    let bytes = bytes.get(start..start + RENDER_CELL_OFFSET_BYTES)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
