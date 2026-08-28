//! Checked compact payload carried by one `QRS1` render section.

use core::convert::TryInto;

pub const RENDER_SECTION_PAYLOAD_MAGIC: u32 = u32::from_le_bytes(*b"QRP1");
pub const RENDER_SECTION_PAYLOAD_VERSION: u16 = 1;
pub const RENDER_SECTION_PAYLOAD_HEADER_BYTES: usize = 48;
pub const RENDER_SECTION_PAYLOAD_FACE_BYTES: usize = 28;
pub const RENDER_SECTION_PAYLOAD_CORNER_BYTES: usize = 8;
pub const RENDER_SECTION_PAYLOAD_POSITION_BYTES: usize = 6;
pub const RENDER_SECTION_PAYLOAD_CELL_BYTES: usize = 8;
pub const RENDER_SECTION_TEMPLATE_NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderSectionPayloadError {
    TooSmall,
    BadMagic,
    BadVersion,
    BadHeaderSize,
    BadFileSize,
    NonCanonicalLayout,
    BadFaceRange,
    BadCornerPosition,
    BadCellOrder,
    BadCellStream,
    BadTemplateRange,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionPayloadFace {
    pub plane: u16,
    pub material: u16,
    pub first_corner: u16,
    pub corner_count: u8,
    pub flags: u8,
    pub light_styles: [u8; 2],
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    /// Byte offset in one activated GPU template pool, or `u32::MAX` for a
    /// dynamic-only face.
    pub template_offset: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionPayloadCorner {
    pub position: u16,
    pub texture: [u8; 2],
    pub light: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderSectionPayloadCell {
    pub leaf: u16,
    pub stream_len: u16,
    pub stream_offset: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSectionPayload<'a> {
    bytes: &'a [u8],
    faces: &'a [u8],
    corners: &'a [u8],
    positions: &'a [u8],
    cells: &'a [u8],
    streams_offset: usize,
    template_bytes: usize,
}

impl<'a> RenderSectionPayload<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderSectionPayloadError> {
        let header = bytes
            .get(..RENDER_SECTION_PAYLOAD_HEADER_BYTES)
            .ok_or(RenderSectionPayloadError::TooSmall)?;
        if u32_at(header, 0) != RENDER_SECTION_PAYLOAD_MAGIC {
            return Err(RenderSectionPayloadError::BadMagic);
        }
        if u16_at(header, 4) != RENDER_SECTION_PAYLOAD_VERSION {
            return Err(RenderSectionPayloadError::BadVersion);
        }
        if u16_at(header, 6) as usize != RENDER_SECTION_PAYLOAD_HEADER_BYTES {
            return Err(RenderSectionPayloadError::BadHeaderSize);
        }
        if u32_at(header, 44) as usize != bytes.len() {
            return Err(RenderSectionPayloadError::BadFileSize);
        }
        let face_count = u16_at(header, 8) as usize;
        let corner_count = u16_at(header, 10) as usize;
        let position_count = u16_at(header, 12) as usize;
        let cell_count = u16_at(header, 14) as usize;
        let faces_offset = u32_at(header, 16) as usize;
        let corners_offset = u32_at(header, 20) as usize;
        let positions_offset = u32_at(header, 24) as usize;
        let cells_offset = u32_at(header, 28) as usize;
        let streams_offset = u32_at(header, 32) as usize;
        if u32_at(header, 36) != 0 {
            return Err(RenderSectionPayloadError::NonCanonicalLayout);
        }
        let template_bytes = u32_at(header, 40) as usize;
        let faces_end =
            checked_table_end(faces_offset, face_count, RENDER_SECTION_PAYLOAD_FACE_BYTES)?;
        let corners_end = checked_table_end(
            corners_offset,
            corner_count,
            RENDER_SECTION_PAYLOAD_CORNER_BYTES,
        )?;
        let positions_end = checked_table_end(
            positions_offset,
            position_count,
            RENDER_SECTION_PAYLOAD_POSITION_BYTES,
        )?;
        let cells_end =
            checked_table_end(cells_offset, cell_count, RENDER_SECTION_PAYLOAD_CELL_BYTES)?;
        if faces_offset != RENDER_SECTION_PAYLOAD_HEADER_BYTES
            || corners_offset != align_up_4(faces_end)
            || positions_offset != corners_end
            || cells_offset != align_up_4(positions_end)
            || streams_offset != cells_end
            || streams_offset > bytes.len()
        {
            return Err(RenderSectionPayloadError::NonCanonicalLayout);
        }
        let payload = Self {
            bytes,
            faces: bytes
                .get(faces_offset..faces_end)
                .ok_or(RenderSectionPayloadError::BadFileSize)?,
            corners: bytes
                .get(corners_offset..corners_end)
                .ok_or(RenderSectionPayloadError::BadFileSize)?,
            positions: bytes
                .get(positions_offset..positions_end)
                .ok_or(RenderSectionPayloadError::BadFileSize)?,
            cells: bytes
                .get(cells_offset..cells_end)
                .ok_or(RenderSectionPayloadError::BadFileSize)?,
            streams_offset,
            template_bytes,
        };

        let mut expected_corner = 0usize;
        let mut expected_template = 0usize;
        for face_index in 0..face_count {
            let face = payload
                .face(face_index)
                .ok_or(RenderSectionPayloadError::BadFaceRange)?;
            if face.corner_count < 3 || face.first_corner as usize != expected_corner {
                return Err(RenderSectionPayloadError::BadFaceRange);
            }
            expected_corner = expected_corner
                .checked_add(face.corner_count as usize)
                .ok_or(RenderSectionPayloadError::BadFaceRange)?;
            if expected_corner > corner_count {
                return Err(RenderSectionPayloadError::BadFaceRange);
            }
            if face.template_offset != RENDER_SECTION_TEMPLATE_NONE {
                if face.template_offset as usize != expected_template {
                    return Err(RenderSectionPayloadError::BadTemplateRange);
                }
                expected_template = expected_template
                    .checked_add(render_section_template_bytes(face.corner_count as usize))
                    .ok_or(RenderSectionPayloadError::BadTemplateRange)?;
                if expected_template > template_bytes {
                    return Err(RenderSectionPayloadError::BadTemplateRange);
                }
            }
        }
        if expected_corner != corner_count || expected_template != template_bytes {
            return Err(RenderSectionPayloadError::BadTemplateRange);
        }
        for corner_index in 0..corner_count {
            if payload
                .corner(corner_index)
                .ok_or(RenderSectionPayloadError::BadCornerPosition)?
                .position as usize
                >= position_count
            {
                return Err(RenderSectionPayloadError::BadCornerPosition);
            }
        }
        let mut previous_leaf = None;
        let mut expected_stream = streams_offset;
        for cell_index in 0..cell_count {
            let cell = payload
                .cell(cell_index)
                .ok_or(RenderSectionPayloadError::BadCellStream)?;
            if previous_leaf.is_some_and(|leaf| leaf >= cell.leaf) {
                return Err(RenderSectionPayloadError::BadCellOrder);
            }
            if cell.stream_offset as usize != expected_stream {
                return Err(RenderSectionPayloadError::BadCellStream);
            }
            expected_stream = expected_stream
                .checked_add(cell.stream_len as usize)
                .ok_or(RenderSectionPayloadError::BadCellStream)?;
            if expected_stream > bytes.len() {
                return Err(RenderSectionPayloadError::BadCellStream);
            }
            previous_leaf = Some(cell.leaf);
        }
        if expected_stream != bytes.len() {
            return Err(RenderSectionPayloadError::BadCellStream);
        }
        Ok(payload)
    }

    pub const fn face_count(self) -> usize {
        self.faces.len() / RENDER_SECTION_PAYLOAD_FACE_BYTES
    }

    pub const fn corner_count(self) -> usize {
        self.corners.len() / RENDER_SECTION_PAYLOAD_CORNER_BYTES
    }

    pub const fn position_count(self) -> usize {
        self.positions.len() / RENDER_SECTION_PAYLOAD_POSITION_BYTES
    }

    pub const fn cell_count(self) -> usize {
        self.cells.len() / RENDER_SECTION_PAYLOAD_CELL_BYTES
    }

    pub const fn template_bytes(self) -> usize {
        self.template_bytes
    }

    pub fn face(self, index: usize) -> Option<RenderSectionPayloadFace> {
        let start = index.checked_mul(RENDER_SECTION_PAYLOAD_FACE_BYTES)?;
        let bytes = self
            .faces
            .get(start..start + RENDER_SECTION_PAYLOAD_FACE_BYTES)?;
        Some(RenderSectionPayloadFace {
            plane: u16_at(bytes, 0),
            material: u16_at(bytes, 2),
            first_corner: u16_at(bytes, 4),
            corner_count: bytes[6],
            flags: bytes[7],
            light_styles: [bytes[8], bytes[9]],
            mins: [i16_at(bytes, 12), i16_at(bytes, 14), i16_at(bytes, 16)],
            maxs: [i16_at(bytes, 18), i16_at(bytes, 20), i16_at(bytes, 22)],
            template_offset: u32_at(bytes, 24),
        })
    }

    pub fn corner(self, index: usize) -> Option<RenderSectionPayloadCorner> {
        let start = index.checked_mul(RENDER_SECTION_PAYLOAD_CORNER_BYTES)?;
        let bytes = self
            .corners
            .get(start..start + RENDER_SECTION_PAYLOAD_CORNER_BYTES)?;
        Some(RenderSectionPayloadCorner {
            position: u16_at(bytes, 0),
            texture: [bytes[2], bytes[3]],
            light: u32_at(bytes, 4),
        })
    }

    pub fn position(self, index: usize) -> Option<[i16; 3]> {
        let start = index.checked_mul(RENDER_SECTION_PAYLOAD_POSITION_BYTES)?;
        let bytes = self
            .positions
            .get(start..start + RENDER_SECTION_PAYLOAD_POSITION_BYTES)?;
        Some([i16_at(bytes, 0), i16_at(bytes, 2), i16_at(bytes, 4)])
    }

    pub fn cell(self, index: usize) -> Option<RenderSectionPayloadCell> {
        let start = index.checked_mul(RENDER_SECTION_PAYLOAD_CELL_BYTES)?;
        let bytes = self
            .cells
            .get(start..start + RENDER_SECTION_PAYLOAD_CELL_BYTES)?;
        Some(RenderSectionPayloadCell {
            leaf: u16_at(bytes, 0),
            stream_len: u16_at(bytes, 2),
            stream_offset: u32_at(bytes, 4),
        })
    }

    pub fn cell_stream(self, cell: RenderSectionPayloadCell) -> Option<&'a [u8]> {
        let start = cell.stream_offset as usize;
        let end = start.checked_add(cell.stream_len as usize)?;
        self.bytes.get(start..end)
    }
}

/// Root fan packet bytes in one display pool: paired triangles become one
/// 52-byte GT4 and an odd remainder becomes one 40-byte GT3.
pub const fn render_section_template_bytes(corner_count: usize) -> usize {
    let triangles = corner_count.saturating_sub(2);
    (triangles / 2) * 52 + (triangles & 1) * 40
}

fn checked_table_end(
    offset: usize,
    count: usize,
    record_bytes: usize,
) -> Result<usize, RenderSectionPayloadError> {
    offset
        .checked_add(
            count
                .checked_mul(record_bytes)
                .ok_or(RenderSectionPayloadError::BadFileSize)?,
        )
        .ok_or(RenderSectionPayloadError::BadFileSize)
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn fixture() -> std::vec::Vec<u8> {
        let faces_offset = RENDER_SECTION_PAYLOAD_HEADER_BYTES;
        let corners_offset = align_up_4(faces_offset + RENDER_SECTION_PAYLOAD_FACE_BYTES);
        let positions_offset = corners_offset + 3 * RENDER_SECTION_PAYLOAD_CORNER_BYTES;
        let cells_offset = align_up_4(positions_offset + 3 * RENDER_SECTION_PAYLOAD_POSITION_BYTES);
        let streams_offset = cells_offset + RENDER_SECTION_PAYLOAD_CELL_BYTES;
        let mut bytes = vec![0; streams_offset + 3];
        bytes[0..4].copy_from_slice(&RENDER_SECTION_PAYLOAD_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&RENDER_SECTION_PAYLOAD_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(RENDER_SECTION_PAYLOAD_HEADER_BYTES as u16).to_le_bytes());
        bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&3u16.to_le_bytes());
        bytes[12..14].copy_from_slice(&3u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
        for (offset, value) in [
            (16, faces_offset),
            (20, corners_offset),
            (24, positions_offset),
            (28, cells_offset),
            (32, streams_offset),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        bytes[40..44].copy_from_slice(&40u32.to_le_bytes());
        let file_len = bytes.len() as u32;
        bytes[44..48].copy_from_slice(&file_len.to_le_bytes());
        bytes[faces_offset + 6] = 3;
        bytes[faces_offset + 12..faces_offset + 18].copy_from_slice(&[0, 0, 1, 0, 2, 0]);
        bytes[faces_offset + 18..faces_offset + 24].copy_from_slice(&[3, 0, 4, 0, 5, 0]);
        for corner in 0..3 {
            let start = corners_offset + corner * RENDER_SECTION_PAYLOAD_CORNER_BYTES;
            bytes[start..start + 2].copy_from_slice(&(corner as u16).to_le_bytes());
        }
        bytes[cells_offset..cells_offset + 2].copy_from_slice(&7u16.to_le_bytes());
        bytes[cells_offset + 2..cells_offset + 4].copy_from_slice(&3u16.to_le_bytes());
        bytes[cells_offset + 4..cells_offset + 8]
            .copy_from_slice(&(streams_offset as u32).to_le_bytes());
        bytes[streams_offset..].copy_from_slice(&[9, 8, 7]);
        bytes
    }

    #[test]
    fn checked_payload_exposes_compact_geometry_and_cell_stream() {
        let bytes = fixture();
        let payload = RenderSectionPayload::parse(&bytes).unwrap();
        assert_eq!(payload.face_count(), 1);
        assert_eq!(payload.corner_count(), 3);
        assert_eq!(payload.position_count(), 3);
        assert_eq!(payload.template_bytes(), 40);
        assert_eq!(payload.face(0).unwrap().mins, [0, 1, 2]);
        assert_eq!(payload.face(0).unwrap().maxs, [3, 4, 5]);
        assert_eq!(payload.corner(2).unwrap().position, 2);
        assert_eq!(
            payload.cell_stream(payload.cell(0).unwrap()),
            Some(&[9, 8, 7][..])
        );
    }

    #[test]
    fn checked_payload_rejects_bad_local_indices_and_template_offsets() {
        let mut bytes = fixture();
        let corners_offset = u32_at(&bytes, 20) as usize;
        bytes[corners_offset..corners_offset + 2].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(
            RenderSectionPayload::parse(&bytes),
            Err(RenderSectionPayloadError::BadCornerPosition)
        );

        let mut bytes = fixture();
        let faces_offset = u32_at(&bytes, 16) as usize;
        bytes[faces_offset + 24..faces_offset + 28].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(
            RenderSectionPayload::parse(&bytes),
            Err(RenderSectionPayloadError::BadTemplateRange)
        );
    }
}
