//! Checked directory for Quake II-style streamed world-render sections.
//!
//! `QRS4` stores one canonical `QRP4` object dictionary and partitions only
//! its cell stream into activation sections. Objects are never duplicated on
//! disc. A section record carries the exact compact staging, retained CPU,
//! projected-position, fixed-packet, and dynamic-fallback candidate counts needed to
//! gather its referenced objects from the shared dictionary. Compact staging
//! is consumed through the guest's existing bounded CD scratch buffer during
//! a quiescent section change, so it is not co-resident with activation data.

use core::convert::TryInto;

use super::render_quad_payload::RenderQuadPayload;

pub const RENDER_SECTION_MAGIC: u32 = u32::from_le_bytes(*b"QRS4");
pub const RENDER_SECTION_VERSION: u16 = 4;
pub const RENDER_SECTION_HEADER_BYTES: usize = 48;
pub const RENDER_SECTION_RECORD_BYTES: usize = 32;
pub const RENDER_SECTION_EDGE_BYTES: usize = 4;
pub const RENDER_SECTION_NONE: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderSectionError {
    TooSmall,
    BadMagic,
    BadVersion,
    BadHeaderSize,
    BadRecordSize,
    BadFileSize,
    NonCanonicalLayout,
    BadLeafSection,
    BadEdgeRange,
    BadNeighbor,
    BadPayloadRange,
    BadMemoryBudget,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionRecord {
    pub first_edge: u16,
    pub edge_count: u16,
    pub first_cell: u16,
    pub cell_count: u16,
    /// Canonical compact bytes gathered from the shared object dictionary.
    pub staging_bytes: u32,
    /// CPU streaming-tail bytes retained after activation.
    pub activation_bytes: u32,
    /// Bytes occupied by one installed fixed-packet pool.
    pub packet_pool_bytes: u32,
    /// Shared projected-position storage inside `activation_bytes`.
    pub projection_bytes: u32,
    /// Conservative pre-cull packet candidates for exact dynamic fallback work.
    ///
    /// This is not added to `packet_pool_bytes`: the active frame rejects
    /// fallback faces before writing its separately bounded dynamic tail.
    pub fallback_bytes: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionEdge {
    pub neighbor: u16,
    pub flags: u16,
}

/// Checked fixed header for a streamed `QRS4` sidecar.
///
/// The guest reads this record first and then allocates only the bounded
/// leaf/section/edge prefix. The single shared QRP4 dictionary follows it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionHeader {
    leaf_count: u16,
    section_count: u16,
    edge_count: u16,
    leaf_offset: u32,
    section_offset: u32,
    edge_offset: u32,
    payload_offset: u32,
    file_bytes: u32,
    resident_core_bytes: u32,
    streaming_budget_bytes: u32,
    packet_pool_budget_bytes: u32,
}

impl RenderSectionHeader {
    pub fn parse(bytes: &[u8], file_bytes: usize) -> Result<Self, RenderSectionError> {
        let header = bytes
            .get(..RENDER_SECTION_HEADER_BYTES)
            .ok_or(RenderSectionError::TooSmall)?;
        if u32_at(header, 0) != RENDER_SECTION_MAGIC {
            return Err(RenderSectionError::BadMagic);
        }
        if u16_at(header, 4) != RENDER_SECTION_VERSION {
            return Err(RenderSectionError::BadVersion);
        }
        if u16_at(header, 6) as usize != RENDER_SECTION_HEADER_BYTES {
            return Err(RenderSectionError::BadHeaderSize);
        }
        if u16_at(header, 14) as usize != RENDER_SECTION_RECORD_BYTES {
            return Err(RenderSectionError::BadRecordSize);
        }
        let encoded_file_bytes = u32_at(header, 32);
        if encoded_file_bytes as usize != file_bytes {
            return Err(RenderSectionError::BadFileSize);
        }
        let decoded = Self {
            leaf_count: u16_at(header, 8),
            section_count: u16_at(header, 10),
            edge_count: u16_at(header, 12),
            leaf_offset: u32_at(header, 16),
            section_offset: u32_at(header, 20),
            edge_offset: u32_at(header, 24),
            payload_offset: u32_at(header, 28),
            file_bytes: encoded_file_bytes,
            resident_core_bytes: u32_at(header, 36),
            streaming_budget_bytes: u32_at(header, 40),
            packet_pool_budget_bytes: u32_at(header, 44),
        };
        let leaf_end = checked_table_end(decoded.leaf_offset(), decoded.leaf_count(), 2)?;
        let section_end = checked_table_end(
            decoded.section_offset(),
            decoded.section_count(),
            RENDER_SECTION_RECORD_BYTES,
        )?;
        let edge_end = checked_table_end(
            decoded.edge_offset(),
            decoded.edge_count(),
            RENDER_SECTION_EDGE_BYTES,
        )?;
        if decoded.leaf_offset() != RENDER_SECTION_HEADER_BYTES
            || decoded.section_offset() != align_up_4(leaf_end)
            || decoded.edge_offset() != section_end
            || decoded.directory_bytes() != align_up_4(edge_end)
            || decoded.directory_bytes() >= file_bytes
        {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        Ok(decoded)
    }

    #[inline]
    pub const fn leaf_count(self) -> usize {
        self.leaf_count as usize
    }

    #[inline]
    pub const fn section_count(self) -> usize {
        self.section_count as usize
    }

    #[inline]
    pub const fn edge_count(self) -> usize {
        self.edge_count as usize
    }

    #[inline]
    pub const fn leaf_offset(self) -> usize {
        self.leaf_offset as usize
    }

    #[inline]
    pub const fn section_offset(self) -> usize {
        self.section_offset as usize
    }

    #[inline]
    pub const fn edge_offset(self) -> usize {
        self.edge_offset as usize
    }

    #[inline]
    pub const fn directory_bytes(self) -> usize {
        self.payload_offset as usize
    }

    #[inline]
    pub const fn file_bytes(self) -> usize {
        self.file_bytes as usize
    }

    #[inline]
    pub const fn resident_core_bytes(self) -> u32 {
        self.resident_core_bytes
    }

    #[inline]
    pub const fn streaming_budget_bytes(self) -> u32 {
        self.streaming_budget_bytes
    }

    #[inline]
    pub const fn packet_pool_budget_bytes(self) -> u32 {
        self.packet_pool_budget_bytes
    }
}

/// Borrowed, checked leaf/section/edge prefix for a streaming guest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSectionIndex<'a> {
    header: RenderSectionHeader,
    leaves: &'a [u8],
    sections: &'a [u8],
    edges: &'a [u8],
}

impl<'a> RenderSectionIndex<'a> {
    pub fn parse_prefix(prefix: &'a [u8], file_bytes: usize) -> Result<Self, RenderSectionError> {
        let header = RenderSectionHeader::parse(prefix, file_bytes)?;
        if prefix.len() != header.directory_bytes() {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        let leaf_end = checked_table_end(header.leaf_offset(), header.leaf_count(), 2)?;
        let section_end = checked_table_end(
            header.section_offset(),
            header.section_count(),
            RENDER_SECTION_RECORD_BYTES,
        )?;
        let edge_end = checked_table_end(
            header.edge_offset(),
            header.edge_count(),
            RENDER_SECTION_EDGE_BYTES,
        )?;
        let index = Self {
            header,
            leaves: prefix
                .get(header.leaf_offset()..leaf_end)
                .ok_or(RenderSectionError::BadFileSize)?,
            sections: prefix
                .get(header.section_offset()..section_end)
                .ok_or(RenderSectionError::BadFileSize)?,
            edges: prefix
                .get(header.edge_offset()..edge_end)
                .ok_or(RenderSectionError::BadFileSize)?,
        };
        index.validate()?;
        Ok(index)
    }

    fn validate(self) -> Result<(), RenderSectionError> {
        for leaf in 0..self.leaf_count() {
            let section = u16_at(self.leaves, leaf * 2);
            if section != RENDER_SECTION_NONE && section as usize >= self.section_count() {
                return Err(RenderSectionError::BadLeafSection);
            }
        }

        let mut expected_edge = 0usize;
        let mut expected_cell = 0usize;
        for section_index in 0..self.section_count() {
            let section = self
                .section(section_index)
                .ok_or(RenderSectionError::BadRecordSize)?;
            if section.first_edge as usize != expected_edge {
                return Err(RenderSectionError::BadEdgeRange);
            }
            let edge_end = expected_edge
                .checked_add(section.edge_count as usize)
                .ok_or(RenderSectionError::BadEdgeRange)?;
            if edge_end > self.edge_count() {
                return Err(RenderSectionError::BadEdgeRange);
            }
            if section.cell_count == 0 || section.first_cell as usize != expected_cell {
                return Err(RenderSectionError::BadPayloadRange);
            }
            expected_cell = expected_cell
                .checked_add(section.cell_count as usize)
                .ok_or(RenderSectionError::BadPayloadRange)?;
            if section.activation_bytes > self.header.streaming_budget_bytes()
                || section.packet_pool_bytes > self.header.packet_pool_budget_bytes()
            {
                return Err(RenderSectionError::BadMemoryBudget);
            }
            let mut mapped_leaves = 0usize;
            for leaf in 0..self.leaf_count() {
                mapped_leaves +=
                    usize::from(u16_at(self.leaves, leaf * 2) as usize == section_index);
            }
            if mapped_leaves != section.cell_count as usize {
                return Err(RenderSectionError::BadLeafSection);
            }

            let mut previous_neighbor = None;
            for edge_index in expected_edge..edge_end {
                let edge = self
                    .edge(edge_index)
                    .ok_or(RenderSectionError::BadEdgeRange)?;
                if edge.neighbor as usize >= self.section_count()
                    || edge.neighbor as usize == section_index
                    || previous_neighbor.is_some_and(|previous| previous >= edge.neighbor)
                {
                    return Err(RenderSectionError::BadNeighbor);
                }
                previous_neighbor = Some(edge.neighbor);
            }
            expected_edge = edge_end;
        }
        if expected_edge != self.edge_count()
            || expected_cell
                != (0..self.leaf_count())
                    .filter(|leaf| u16_at(self.leaves, leaf * 2) != RENDER_SECTION_NONE)
                    .count()
        {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        Ok(())
    }

    #[inline]
    pub const fn header(self) -> RenderSectionHeader {
        self.header
    }

    #[inline]
    pub const fn leaf_count(self) -> usize {
        self.header.leaf_count()
    }

    #[inline]
    pub const fn section_count(self) -> usize {
        self.header.section_count()
    }

    #[inline]
    pub const fn edge_count(self) -> usize {
        self.header.edge_count()
    }

    #[inline]
    pub fn leaf_section(self, leaf: usize) -> Option<usize> {
        let start = leaf.checked_mul(2)?;
        let section = u16_at(self.leaves.get(start..start + 2)?, 0);
        (section != RENDER_SECTION_NONE).then_some(section as usize)
    }

    #[inline]
    pub fn section(self, index: usize) -> Option<RenderSectionRecord> {
        decode_section_record(self.sections, index)
    }

    #[inline]
    pub fn edge(self, index: usize) -> Option<RenderSectionEdge> {
        let start = index.checked_mul(RENDER_SECTION_EDGE_BYTES)?;
        let bytes = self.edges.get(start..start + RENDER_SECTION_EDGE_BYTES)?;
        Some(RenderSectionEdge {
            neighbor: u16_at(bytes, 0),
            flags: u16_at(bytes, 2),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSectionDirectory<'a> {
    index: RenderSectionIndex<'a>,
    payload: RenderQuadPayload<'a>,
}

impl<'a> RenderSectionDirectory<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderSectionError> {
        let header = RenderSectionHeader::parse(bytes, bytes.len())?;
        let index = RenderSectionIndex::parse_prefix(
            bytes
                .get(..header.directory_bytes())
                .ok_or(RenderSectionError::BadFileSize)?,
            bytes.len(),
        )?;
        let payload = RenderQuadPayload::parse(
            bytes
                .get(header.directory_bytes()..)
                .ok_or(RenderSectionError::BadPayloadRange)?,
        )
        .map_err(|_| RenderSectionError::BadPayloadRange)?;

        let mut expected_cell = 0usize;
        for section_index in 0..index.section_count() {
            let section = index.section(section_index).unwrap();
            let memory = payload
                .section_memory(section.first_cell as usize, section.cell_count as usize)
                .ok_or(RenderSectionError::BadPayloadRange)?;
            if memory.staging_bytes != section.staging_bytes
                || memory.activation_bytes != section.activation_bytes
                || memory.packet_pool_bytes != section.packet_pool_bytes
                || memory.projection_bytes != section.projection_bytes
            {
                return Err(RenderSectionError::BadMemoryBudget);
            }
            for cell_index in section.first_cell as usize
                ..section.first_cell as usize + section.cell_count as usize
            {
                let cell = payload
                    .cell(cell_index)
                    .ok_or(RenderSectionError::BadPayloadRange)?;
                if index.leaf_section(cell.leaf as usize) != Some(section_index) {
                    return Err(RenderSectionError::BadLeafSection);
                }
            }
            expected_cell += section.cell_count as usize;
        }
        if expected_cell != payload.cell_count() {
            return Err(RenderSectionError::BadPayloadRange);
        }
        Ok(Self { index, payload })
    }

    #[inline]
    pub const fn leaf_count(self) -> usize {
        self.index.leaf_count()
    }

    #[inline]
    pub const fn section_count(self) -> usize {
        self.index.section_count()
    }

    #[inline]
    pub const fn edge_count(self) -> usize {
        self.index.edge_count()
    }

    #[inline]
    pub const fn resident_core_bytes(self) -> u32 {
        self.index.header().resident_core_bytes()
    }

    #[inline]
    pub const fn streaming_budget_bytes(self) -> u32 {
        self.index.header().streaming_budget_bytes()
    }

    #[inline]
    pub const fn packet_pool_budget_bytes(self) -> u32 {
        self.index.header().packet_pool_budget_bytes()
    }

    #[inline]
    pub fn leaf_section(self, leaf: usize) -> Option<usize> {
        self.index.leaf_section(leaf)
    }

    #[inline]
    pub fn section(self, index: usize) -> Option<RenderSectionRecord> {
        self.index.section(index)
    }

    #[inline]
    pub fn edge(self, index: usize) -> Option<RenderSectionEdge> {
        self.index.edge(index)
    }

    #[inline]
    pub const fn payload(self) -> RenderQuadPayload<'a> {
        self.payload
    }

    #[inline]
    pub const fn payload_offset(self) -> usize {
        self.index.header().directory_bytes()
    }
}

fn decode_section_record(bytes: &[u8], index: usize) -> Option<RenderSectionRecord> {
    let start = index.checked_mul(RENDER_SECTION_RECORD_BYTES)?;
    let bytes = bytes.get(start..start + RENDER_SECTION_RECORD_BYTES)?;
    if u16_at(bytes, 30) != 0 {
        return None;
    }
    Some(RenderSectionRecord {
        first_edge: u16_at(bytes, 0),
        edge_count: u16_at(bytes, 2),
        first_cell: u16_at(bytes, 4),
        cell_count: u16_at(bytes, 6),
        staging_bytes: u32_at(bytes, 8),
        activation_bytes: u32_at(bytes, 12),
        packet_pool_bytes: u32_at(bytes, 16),
        projection_bytes: u32_at(bytes, 20),
        fallback_bytes: u32_at(bytes, 24),
        flags: u16_at(bytes, 28),
    })
}

fn checked_table_end(
    offset: usize,
    count: usize,
    record_bytes: usize,
) -> Result<usize, RenderSectionError> {
    offset
        .checked_add(
            count
                .checked_mul(record_bytes)
                .ok_or(RenderSectionError::BadFileSize)?,
        )
        .ok_or(RenderSectionError::BadFileSize)
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
