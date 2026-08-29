//! Checked wire format for Quake-PSX streamed render sections.
//!
//! `QRS1` is intentionally a sidecar rather than another resident PSB lump.
//! Collision/gameplay data can keep the existing whole-map lifetime while the
//! world renderer loads one RAM-budgeted section and stages one compact
//! neighbor. Payload bytes are opaque to this directory layer; later format
//! versions can change packet descriptors without changing CD ownership.

use core::convert::TryInto;

pub const RENDER_SECTION_MAGIC: u32 = u32::from_le_bytes(*b"QRS1");
pub const RENDER_SECTION_VERSION: u16 = 1;
pub const RENDER_SECTION_HEADER_BYTES: usize = 40;
pub const RENDER_SECTION_RECORD_BYTES: usize = 24;
pub const RENDER_SECTION_EDGE_BYTES: usize = 4;
pub const RENDER_SECTION_NONE: u16 = u16::MAX;
/// Census/partition manifest: ownership and budgets are authoritative, but
/// compact renderer payloads have not been emitted yet.
pub const RENDER_SECTION_FLAG_MANIFEST_ONLY: u16 = 1 << 0;

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
    /// Expanded active bytes: geometry, projection cache, cell streams, and
    /// invariant packet templates installed in both GPU pools.
    pub active_bytes: u32,
    /// CD/preload bytes before projection caches and dual packet templates
    /// are installed. This must not exceed `active_bytes`.
    pub compact_bytes: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionEdge {
    pub neighbor: u16,
    pub flags: u16,
}

/// Validated fixed header for a streamed `QRS1` sidecar.
///
/// The guest reads this record first, allocates only [`Self::directory_bytes`],
/// and then validates the leaf/section/edge prefix with
/// [`RenderSectionIndex::parse_prefix`]. No render payload needs to be resident
/// merely to resolve the camera leaf's active section.
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
        if u32_at(header, 36) != 0 {
            return Err(RenderSectionError::NonCanonicalLayout);
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
        };
        let leaf_end = decoded
            .leaf_offset()
            .checked_add(
                decoded
                    .leaf_count()
                    .checked_mul(2)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        let section_end = decoded
            .section_offset()
            .checked_add(
                decoded
                    .section_count()
                    .checked_mul(RENDER_SECTION_RECORD_BYTES)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        let edge_end = decoded
            .edge_offset()
            .checked_add(
                decoded
                    .edge_count()
                    .checked_mul(RENDER_SECTION_EDGE_BYTES)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        if decoded.leaf_offset() != RENDER_SECTION_HEADER_BYTES
            || decoded.section_offset() != align_up_4(leaf_end)
            || decoded.edge_offset() != section_end
            || decoded.directory_bytes() != align_up_4(edge_end)
            || decoded.directory_bytes() > file_bytes
        {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        Ok(decoded)
    }

    pub const fn leaf_count(self) -> usize {
        self.leaf_count as usize
    }

    pub const fn section_count(self) -> usize {
        self.section_count as usize
    }

    pub const fn edge_count(self) -> usize {
        self.edge_count as usize
    }

    pub const fn leaf_offset(self) -> usize {
        self.leaf_offset as usize
    }

    pub const fn section_offset(self) -> usize {
        self.section_offset as usize
    }

    pub const fn edge_offset(self) -> usize {
        self.edge_offset as usize
    }

    pub const fn directory_bytes(self) -> usize {
        self.payload_offset as usize
    }

    pub const fn file_bytes(self) -> usize {
        self.file_bytes as usize
    }
}

/// Borrowed, checked leaf/section/edge prefix for streaming guests.
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
        let leaf_end = header.section_offset();
        let section_end = header.edge_offset();
        let edge_end = header.directory_bytes();
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
        for leaf in 0..index.leaf_count() {
            let section = u16_at(index.leaves, leaf * 2);
            if section != RENDER_SECTION_NONE && section as usize >= index.section_count() {
                return Err(RenderSectionError::BadLeafSection);
            }
        }
        let mut previous_payload_end = header.directory_bytes();
        for section_index in 0..index.section_count() {
            let section = index
                .section(section_index)
                .ok_or(RenderSectionError::BadRecordSize)?;
            let edge_end = section
                .first_edge
                .checked_add(section.edge_count)
                .ok_or(RenderSectionError::BadEdgeRange)? as usize;
            if edge_end > index.edge_count() {
                return Err(RenderSectionError::BadEdgeRange);
            }
            let payload_start = section.payload_offset as usize;
            let payload_end = payload_start
                .checked_add(section.payload_len as usize)
                .ok_or(RenderSectionError::BadPayloadRange)?;
            if payload_start < header.directory_bytes()
                || payload_start < previous_payload_end
                || payload_end > file_bytes
            {
                return Err(RenderSectionError::BadPayloadRange);
            }
            if section.compact_bytes > section.active_bytes
                || (section.flags & RENDER_SECTION_FLAG_MANIFEST_ONLY == 0
                    && section.compact_bytes != section.payload_len)
            {
                return Err(RenderSectionError::BadMemoryBudget);
            }
            previous_payload_end = payload_end;
            for edge_index in section.first_edge as usize..edge_end {
                let edge = index
                    .edge(edge_index)
                    .ok_or(RenderSectionError::BadEdgeRange)?;
                if edge.neighbor as usize >= index.section_count()
                    || edge.neighbor as usize == section_index
                {
                    return Err(RenderSectionError::BadNeighbor);
                }
            }
        }
        if previous_payload_end != file_bytes {
            return Err(RenderSectionError::BadPayloadRange);
        }
        Ok(index)
    }

    pub const fn header(self) -> RenderSectionHeader {
        self.header
    }

    pub const fn leaf_count(self) -> usize {
        self.header.leaf_count()
    }

    pub const fn section_count(self) -> usize {
        self.header.section_count()
    }

    pub const fn edge_count(self) -> usize {
        self.header.edge_count()
    }

    pub fn leaf_section(self, leaf: usize) -> Option<usize> {
        let start = leaf.checked_mul(2)?;
        let section = u16::from_le_bytes(self.leaves.get(start..start + 2)?.try_into().ok()?);
        (section != RENDER_SECTION_NONE).then_some(section as usize)
    }

    pub fn section(self, index: usize) -> Option<RenderSectionRecord> {
        let start = index.checked_mul(RENDER_SECTION_RECORD_BYTES)?;
        let bytes = self
            .sections
            .get(start..start + RENDER_SECTION_RECORD_BYTES)?;
        Some(RenderSectionRecord {
            first_edge: u16_at(bytes, 0),
            edge_count: u16_at(bytes, 2),
            payload_offset: u32_at(bytes, 4),
            payload_len: u32_at(bytes, 8),
            active_bytes: u32_at(bytes, 12),
            compact_bytes: u32_at(bytes, 16),
            flags: u16_at(bytes, 20),
        })
    }

    pub fn edge(self, index: usize) -> Option<RenderSectionEdge> {
        let start = index.checked_mul(RENDER_SECTION_EDGE_BYTES)?;
        let bytes = self.edges.get(start..start + RENDER_SECTION_EDGE_BYTES)?;
        Some(RenderSectionEdge {
            neighbor: u16_at(bytes, 0),
            flags: u16_at(bytes, 2),
        })
    }
}

/// Validated borrowed view of one `QRS1` sidecar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSectionDirectory<'a> {
    bytes: &'a [u8],
    leaves: &'a [u8],
    sections: &'a [u8],
    edges: &'a [u8],
    payload_offset: usize,
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
        if u32_at(header, 36) != 0 {
            return Err(RenderSectionError::NonCanonicalLayout);
        }

        let leaf_count = u16_at(header, 8) as usize;
        let section_count = u16_at(header, 10) as usize;
        let edge_count = u16_at(header, 12) as usize;
        let leaf_offset = u32_at(header, 16) as usize;
        let section_offset = u32_at(header, 20) as usize;
        let edge_offset = u32_at(header, 24) as usize;
        let payload_offset = u32_at(header, 28) as usize;
        let leaf_end = leaf_offset
            .checked_add(
                leaf_count
                    .checked_mul(2)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        let section_end = section_offset
            .checked_add(
                section_count
                    .checked_mul(RENDER_SECTION_RECORD_BYTES)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        let edge_end = edge_offset
            .checked_add(
                edge_count
                    .checked_mul(RENDER_SECTION_EDGE_BYTES)
                    .ok_or(RenderSectionError::BadFileSize)?,
            )
            .ok_or(RenderSectionError::BadFileSize)?;
        if leaf_offset != RENDER_SECTION_HEADER_BYTES
            || section_offset != align_up_4(leaf_end)
            || edge_offset != section_end
            || payload_offset != align_up_4(edge_end)
            || payload_offset > bytes.len()
        {
            return Err(RenderSectionError::NonCanonicalLayout);
        }
        let leaves = bytes
            .get(leaf_offset..leaf_end)
            .ok_or(RenderSectionError::BadFileSize)?;
        let sections = bytes
            .get(section_offset..section_end)
            .ok_or(RenderSectionError::BadFileSize)?;
        let edges = bytes
            .get(edge_offset..edge_end)
            .ok_or(RenderSectionError::BadFileSize)?;
        let directory = Self {
            bytes,
            leaves,
            sections,
            edges,
            payload_offset,
        };

        for leaf in 0..leaf_count {
            let section = u16_at(leaves, leaf * 2);
            if section != RENDER_SECTION_NONE && section as usize >= section_count {
                return Err(RenderSectionError::BadLeafSection);
            }
        }
        let mut previous_payload_end = payload_offset;
        for section_index in 0..section_count {
            let section = directory
                .section(section_index)
                .ok_or(RenderSectionError::BadRecordSize)?;
            let edge_end = section
                .first_edge
                .checked_add(section.edge_count)
                .ok_or(RenderSectionError::BadEdgeRange)? as usize;
            if edge_end > edge_count {
                return Err(RenderSectionError::BadEdgeRange);
            }
            let payload_start = section.payload_offset as usize;
            let payload_end = payload_start
                .checked_add(section.payload_len as usize)
                .ok_or(RenderSectionError::BadPayloadRange)?;
            if payload_start < payload_offset
                || payload_start < previous_payload_end
                || payload_end > bytes.len()
            {
                return Err(RenderSectionError::BadPayloadRange);
            }
            if section.compact_bytes > section.active_bytes {
                return Err(RenderSectionError::BadMemoryBudget);
            }
            previous_payload_end = payload_end;
            for edge_index in section.first_edge as usize..edge_end {
                let edge = directory
                    .edge(edge_index)
                    .ok_or(RenderSectionError::BadEdgeRange)?;
                if edge.neighbor as usize >= section_count
                    || edge.neighbor as usize == section_index
                {
                    return Err(RenderSectionError::BadNeighbor);
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
    pub const fn payload_offset(self) -> usize {
        self.payload_offset
    }

    #[inline]
    pub fn leaf_section(self, leaf: usize) -> Option<usize> {
        let start = leaf.checked_mul(2)?;
        let section = u16::from_le_bytes(self.leaves.get(start..start + 2)?.try_into().ok()?);
        (section != RENDER_SECTION_NONE).then_some(section as usize)
    }

    #[inline]
    pub fn section(self, index: usize) -> Option<RenderSectionRecord> {
        let start = index.checked_mul(RENDER_SECTION_RECORD_BYTES)?;
        let bytes = self
            .sections
            .get(start..start + RENDER_SECTION_RECORD_BYTES)?;
        Some(RenderSectionRecord {
            first_edge: u16_at(bytes, 0),
            edge_count: u16_at(bytes, 2),
            payload_offset: u32_at(bytes, 4),
            payload_len: u32_at(bytes, 8),
            active_bytes: u32_at(bytes, 12),
            compact_bytes: u32_at(bytes, 16),
            flags: u16_at(bytes, 20),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn fixture() -> Vec<u8> {
        let leaf_count = 4usize;
        let section_count = 2usize;
        let edge_count = 2usize;
        let leaf_offset = RENDER_SECTION_HEADER_BYTES;
        let section_offset = align_up_4(leaf_offset + leaf_count * 2);
        let edge_offset = section_offset + section_count * RENDER_SECTION_RECORD_BYTES;
        let payload_offset = align_up_4(edge_offset + edge_count * RENDER_SECTION_EDGE_BYTES);
        let mut bytes = vec![0; payload_offset + 7];
        bytes[0..4].copy_from_slice(&RENDER_SECTION_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&RENDER_SECTION_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(RENDER_SECTION_HEADER_BYTES as u16).to_le_bytes());
        bytes[8..10].copy_from_slice(&(leaf_count as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&(section_count as u16).to_le_bytes());
        bytes[12..14].copy_from_slice(&(edge_count as u16).to_le_bytes());
        bytes[14..16].copy_from_slice(&(RENDER_SECTION_RECORD_BYTES as u16).to_le_bytes());
        bytes[16..20].copy_from_slice(&(leaf_offset as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&(section_offset as u32).to_le_bytes());
        bytes[24..28].copy_from_slice(&(edge_offset as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&(payload_offset as u32).to_le_bytes());
        let file_len = bytes.len() as u32;
        bytes[32..36].copy_from_slice(&file_len.to_le_bytes());
        for (leaf, section) in [RENDER_SECTION_NONE, 0, 0, 1].into_iter().enumerate() {
            let start = leaf_offset + leaf * 2;
            bytes[start..start + 2].copy_from_slice(&section.to_le_bytes());
        }
        let first = section_offset;
        bytes[first..first + 2].copy_from_slice(&0u16.to_le_bytes());
        bytes[first + 2..first + 4].copy_from_slice(&1u16.to_le_bytes());
        bytes[first + 4..first + 8].copy_from_slice(&(payload_offset as u32).to_le_bytes());
        bytes[first + 8..first + 12].copy_from_slice(&3u32.to_le_bytes());
        bytes[first + 12..first + 16].copy_from_slice(&192_000u32.to_le_bytes());
        bytes[first + 16..first + 20].copy_from_slice(&3u32.to_le_bytes());
        let second = first + RENDER_SECTION_RECORD_BYTES;
        bytes[second..second + 2].copy_from_slice(&1u16.to_le_bytes());
        bytes[second + 2..second + 4].copy_from_slice(&1u16.to_le_bytes());
        bytes[second + 4..second + 8].copy_from_slice(&((payload_offset + 3) as u32).to_le_bytes());
        bytes[second + 8..second + 12].copy_from_slice(&4u32.to_le_bytes());
        bytes[second + 12..second + 16].copy_from_slice(&180_000u32.to_le_bytes());
        bytes[second + 16..second + 20].copy_from_slice(&4u32.to_le_bytes());
        bytes[edge_offset..edge_offset + 2].copy_from_slice(&1u16.to_le_bytes());
        bytes[edge_offset + 4..edge_offset + 6].copy_from_slice(&0u16.to_le_bytes());
        bytes[payload_offset..].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        bytes
    }

    #[test]
    fn checked_directory_exposes_leaf_edges_and_payloads() {
        let bytes = fixture();
        let directory = RenderSectionDirectory::parse(&bytes).unwrap();
        assert_eq!(directory.leaf_count(), 4);
        assert_eq!(directory.section_count(), 2);
        assert_eq!(directory.leaf_section(0), None);
        assert_eq!(directory.leaf_section(2), Some(0));
        assert_eq!(directory.leaf_section(3), Some(1));
        assert_eq!(directory.edge(0).unwrap().neighbor, 1);
        assert_eq!(
            directory.payload(directory.section(0).unwrap()),
            Some(&[1, 2, 3][..])
        );
        assert_eq!(
            directory.payload(directory.section(1).unwrap()),
            Some(&[4, 5, 6, 7][..])
        );
    }

    #[test]
    fn streamed_prefix_exposes_the_same_checked_index() {
        let bytes = fixture();
        let header =
            RenderSectionHeader::parse(&bytes[..RENDER_SECTION_HEADER_BYTES], bytes.len()).unwrap();
        let index = RenderSectionIndex::parse_prefix(
            &bytes[..header.directory_bytes()],
            header.file_bytes(),
        )
        .unwrap();
        assert_eq!(index.leaf_count(), 4);
        assert_eq!(index.section_count(), 2);
        assert_eq!(index.leaf_section(0), None);
        assert_eq!(index.leaf_section(3), Some(1));
        assert_eq!(index.section(1).unwrap().payload_len, 4);
        assert_eq!(index.edge(1).unwrap().neighbor, 0);
    }

    #[test]
    fn checked_directory_rejects_cross_section_corruption() {
        let mut bytes = fixture();
        let leaf_offset = RENDER_SECTION_HEADER_BYTES;
        bytes[leaf_offset + 2..leaf_offset + 4].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            RenderSectionDirectory::parse(&bytes),
            Err(RenderSectionError::BadLeafSection)
        );

        let mut bytes = fixture();
        let section_offset = align_up_4(RENDER_SECTION_HEADER_BYTES + 4 * 2);
        bytes[section_offset + 16..section_offset + 20].copy_from_slice(&200_000u32.to_le_bytes());
        assert_eq!(
            RenderSectionDirectory::parse(&bytes),
            Err(RenderSectionError::BadMemoryBudget)
        );
    }
}
