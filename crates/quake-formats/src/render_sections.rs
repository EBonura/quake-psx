//! Checked directory for Quake II-style streamed world-render sections.
//!
//! `QRS2` separates the always-resident collision/gameplay core from one
//! activated world-render section and one compact neighbor preload. The
//! directory carries enough memory accounting for both the host cooker and
//! the guest loader to reject a map which cannot fit the reserved streaming
//! tail before any packet pool is installed.

use core::convert::TryInto;

pub const RENDER_SECTION_MAGIC: u32 = u32::from_le_bytes(*b"QRS2");
pub const RENDER_SECTION_VERSION: u16 = 2;
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
    pub payload_offset: u32,
    pub payload_len: u32,
    /// Total streaming-tail bytes after activation.
    pub activation_bytes: u32,
    /// Bytes occupied by one resident GT4 packet pool.
    pub packet_pool_bytes: u32,
    /// Shared projected-position cache installed beside the packet pools.
    pub projection_bytes: u32,
    /// Bounded dynamic-fallback workspace owned by this section.
    pub fallback_bytes: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionEdge {
    pub neighbor: u16,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSectionDirectory<'a> {
    bytes: &'a [u8],
    leaves: &'a [u8],
    sections: &'a [u8],
    edges: &'a [u8],
    payload_offset: usize,
    resident_core_bytes: u32,
    streaming_budget_bytes: u32,
}

impl<'a> RenderSectionDirectory<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderSectionError> {
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
        if u32_at(header, 32) as usize != bytes.len() {
            return Err(RenderSectionError::BadFileSize);
        }
        if u32_at(header, 44) != 0 {
            return Err(RenderSectionError::NonCanonicalLayout);
        }

        let leaf_count = u16_at(header, 8) as usize;
        let section_count = u16_at(header, 10) as usize;
        let edge_count = u16_at(header, 12) as usize;
        let leaf_offset = u32_at(header, 16) as usize;
        let section_offset = u32_at(header, 20) as usize;
        let edge_offset = u32_at(header, 24) as usize;
        let payload_offset = u32_at(header, 28) as usize;
        let resident_core_bytes = u32_at(header, 36);
        let streaming_budget_bytes = u32_at(header, 40);
        let leaf_end = checked_table_end(leaf_offset, leaf_count, 2)?;
        let section_end =
            checked_table_end(section_offset, section_count, RENDER_SECTION_RECORD_BYTES)?;
        let edge_end = checked_table_end(edge_offset, edge_count, RENDER_SECTION_EDGE_BYTES)?;
        if leaf_offset != RENDER_SECTION_HEADER_BYTES
            || section_offset != align_up_4(leaf_end)
            || edge_offset != section_end
            || payload_offset != align_up_4(edge_end)
            || payload_offset > bytes.len()
        {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        let directory = Self {
            bytes,
            leaves: bytes
                .get(leaf_offset..leaf_end)
                .ok_or(RenderSectionError::BadFileSize)?,
            sections: bytes
                .get(section_offset..section_end)
                .ok_or(RenderSectionError::BadFileSize)?,
            edges: bytes
                .get(edge_offset..edge_end)
                .ok_or(RenderSectionError::BadFileSize)?,
            payload_offset,
            resident_core_bytes,
            streaming_budget_bytes,
        };

        for leaf in 0..leaf_count {
            let section = u16_at(directory.leaves, leaf * 2);
            if section != RENDER_SECTION_NONE && section as usize >= section_count {
                return Err(RenderSectionError::BadLeafSection);
            }
        }

        let mut expected_edge = 0usize;
        let mut expected_payload = payload_offset;
        for section_index in 0..section_count {
            let section = directory
                .section(section_index)
                .ok_or(RenderSectionError::BadRecordSize)?;
            if section.first_edge as usize != expected_edge {
                return Err(RenderSectionError::BadEdgeRange);
            }
            let edge_end = expected_edge
                .checked_add(section.edge_count as usize)
                .ok_or(RenderSectionError::BadEdgeRange)?;
            if edge_end > edge_count {
                return Err(RenderSectionError::BadEdgeRange);
            }
            if section.payload_offset as usize != expected_payload {
                return Err(RenderSectionError::BadPayloadRange);
            }
            expected_payload = expected_payload
                .checked_add(section.payload_len as usize)
                .ok_or(RenderSectionError::BadPayloadRange)?;
            if expected_payload > bytes.len() {
                return Err(RenderSectionError::BadPayloadRange);
            }
            let dual_packet_pool_bytes = section
                .packet_pool_bytes
                .checked_mul(2)
                .ok_or(RenderSectionError::BadMemoryBudget)?;
            let expected_activation = section
                .payload_len
                .checked_add(dual_packet_pool_bytes)
                .and_then(|bytes| bytes.checked_add(section.projection_bytes))
                .and_then(|bytes| bytes.checked_add(section.fallback_bytes))
                .ok_or(RenderSectionError::BadMemoryBudget)?;
            if expected_activation != section.activation_bytes
                || section.activation_bytes > streaming_budget_bytes
            {
                return Err(RenderSectionError::BadMemoryBudget);
            }

            let mut previous_neighbor = None;
            for edge_index in expected_edge..edge_end {
                let edge = directory
                    .edge(edge_index)
                    .ok_or(RenderSectionError::BadEdgeRange)?;
                if edge.neighbor as usize >= section_count
                    || edge.neighbor as usize == section_index
                    || previous_neighbor.is_some_and(|previous| previous >= edge.neighbor)
                {
                    return Err(RenderSectionError::BadNeighbor);
                }
                previous_neighbor = Some(edge.neighbor);
            }
            expected_edge = edge_end;
        }
        if expected_edge != edge_count || expected_payload != bytes.len() {
            return Err(RenderSectionError::NonCanonicalLayout);
        }

        // While the current section is active, the largest adjacent compact
        // payload must still fit in the same streaming tail as a preload.
        for section_index in 0..section_count {
            let section = directory.section(section_index).unwrap();
            for edge_index in section.first_edge as usize
                ..section.first_edge as usize + section.edge_count as usize
            {
                let neighbor = directory.edge(edge_index).unwrap().neighbor as usize;
                let preload = directory.section(neighbor).unwrap().payload_len;
                if section
                    .activation_bytes
                    .checked_add(preload)
                    .is_none_or(|bytes| bytes > streaming_budget_bytes)
                {
                    return Err(RenderSectionError::BadMemoryBudget);
                }
            }
        }
        Ok(directory)
    }

    #[inline]
    pub const fn leaf_count(self) -> usize {
        self.leaves.len() / 2
    }

    #[inline]
    pub const fn section_count(self) -> usize {
        self.sections.len() / RENDER_SECTION_RECORD_BYTES
    }

    #[inline]
    pub const fn edge_count(self) -> usize {
        self.edges.len() / RENDER_SECTION_EDGE_BYTES
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
    pub fn leaf_section(self, leaf: usize) -> Option<usize> {
        let start = leaf.checked_mul(2)?;
        let section = u16_at(self.leaves.get(start..start + 2)?, 0);
        (section != RENDER_SECTION_NONE).then_some(section as usize)
    }

    #[inline]
    pub fn section(self, index: usize) -> Option<RenderSectionRecord> {
        let start = index.checked_mul(RENDER_SECTION_RECORD_BYTES)?;
        let bytes = self
            .sections
            .get(start..start + RENDER_SECTION_RECORD_BYTES)?;
        if u16_at(bytes, 30) != 0 {
            return None;
        }
        Some(RenderSectionRecord {
            first_edge: u16_at(bytes, 0),
            edge_count: u16_at(bytes, 2),
            payload_offset: u32_at(bytes, 4),
            payload_len: u32_at(bytes, 8),
            activation_bytes: u32_at(bytes, 12),
            packet_pool_bytes: u32_at(bytes, 16),
            projection_bytes: u32_at(bytes, 20),
            fallback_bytes: u32_at(bytes, 24),
            flags: u16_at(bytes, 28),
        })
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

    #[inline]
    pub fn payload(self, section: RenderSectionRecord) -> Option<&'a [u8]> {
        let start = section.payload_offset as usize;
        let end = start.checked_add(section.payload_len as usize)?;
        self.bytes.get(start..end)
    }

    #[inline]
    pub const fn payload_offset(self) -> usize {
        self.payload_offset
    }
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
