//! Quad-native payload installed by one streamed render section.
//!
//! `QRP2` stores shared 3D positions once, fixed GT4 position references and
//! the eight reusable template words of every packet. Activation expands those
//! words into two 52-byte packet pools and patches settings such as the current
//! brightness CLUT. A frame projects each shared position once, patches only
//! XY/tag fields, then walks the camera cell's ordered object commands and
//! material-run boundaries.

use core::convert::TryInto;

pub const RENDER_QUAD_PAYLOAD_MAGIC: u32 = u32::from_le_bytes(*b"QRP2");
pub const RENDER_QUAD_PAYLOAD_VERSION: u16 = 2;
pub const RENDER_QUAD_HEADER_BYTES: usize = 64;
pub const RENDER_QUAD_OBJECT_BYTES: usize = 24;
pub const RENDER_QUAD_RECORD_BYTES: usize = 40;
pub const RENDER_QUAD_POSITION_BYTES: usize = 6;
pub const RENDER_QUAD_RUN_BYTES: usize = 8;
pub const RENDER_QUAD_CELL_BYTES: usize = 12;
pub const RENDER_QUAD_COMMAND_BYTES: usize = 4;
pub const RENDER_QUAD_PACKET_BYTES: usize = 52;
pub const RENDER_QUAD_PROJECTED_POSITION_BYTES: usize = 8;
pub const RENDER_QUAD_OBJECT_MAX_QUADS: usize = 32;
/// The object's source face uses the back side of its supporting plane.
pub const RENDER_QUAD_OBJECT_BACKSIDE: u16 = 1 << 0;
/// The cell cannot prove the object's facing, so the fixed kernel must test it.
pub const RENDER_QUAD_COMMAND_DYNAMIC_FACING: u16 = 1 << 0;
/// Patch the current shared texture CLUT into word three during activation.
pub const RENDER_QUAD_RUN_PATCH_CLUT: u16 = 1 << 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderQuadPayloadError {
    TooSmall,
    BadMagic,
    BadVersion,
    BadHeaderSize,
    BadFileSize,
    NonCanonicalLayout,
    BadObjectRange,
    BadQuadPosition,
    BadRunRange,
    BadCellOrder,
    BadCellStream,
    BadMemoryAccounting,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadObject {
    pub first_quad: u16,
    pub quad_count: u16,
    pub first_run: u16,
    pub run_count: u16,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub flags: u16,
    /// Supporting source plane for an optional dynamic-facing test.
    pub plane: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuad {
    pub positions: [u16; 4],
    /// GT4 words 1,3,4,6,7,9,10,12. Tag and XY words are installed later;
    /// runs marked [`RENDER_QUAD_RUN_PATCH_CLUT`] store word three's high
    /// half as zero and receive the current CLUT during activation.
    pub invariant_words: [u32; 8],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadRun {
    pub first_quad: u16,
    pub quad_count: u16,
    pub material: u16,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadCell {
    pub leaf: u16,
    pub command_count: u16,
    pub stream_offset: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadCommand {
    pub object: u16,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderQuadPayload<'a> {
    bytes: &'a [u8],
    objects: &'a [u8],
    quads: &'a [u8],
    positions: &'a [u8],
    runs: &'a [u8],
    cells: &'a [u8],
    streams_offset: usize,
    packet_pool_bytes: u32,
    projection_bytes: u32,
}

impl<'a> RenderQuadPayload<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderQuadPayloadError> {
        let header = bytes
            .get(..RENDER_QUAD_HEADER_BYTES)
            .ok_or(RenderQuadPayloadError::TooSmall)?;
        if u32_at(header, 0) != RENDER_QUAD_PAYLOAD_MAGIC {
            return Err(RenderQuadPayloadError::BadMagic);
        }
        if u16_at(header, 4) != RENDER_QUAD_PAYLOAD_VERSION {
            return Err(RenderQuadPayloadError::BadVersion);
        }
        if u16_at(header, 6) as usize != RENDER_QUAD_HEADER_BYTES {
            return Err(RenderQuadPayloadError::BadHeaderSize);
        }
        if u16_at(header, 18) != 0 || u32_at(header, 56) != 0 || u32_at(header, 60) != 0 {
            return Err(RenderQuadPayloadError::NonCanonicalLayout);
        }
        if u32_at(header, 44) as usize != bytes.len() {
            return Err(RenderQuadPayloadError::BadFileSize);
        }

        let object_count = u16_at(header, 8) as usize;
        let quad_count = u16_at(header, 10) as usize;
        let position_count = u16_at(header, 12) as usize;
        let cell_count = u16_at(header, 14) as usize;
        let run_count = u16_at(header, 16) as usize;
        let objects_offset = u32_at(header, 20) as usize;
        let quads_offset = u32_at(header, 24) as usize;
        let positions_offset = u32_at(header, 28) as usize;
        let runs_offset = u32_at(header, 32) as usize;
        let cells_offset = u32_at(header, 36) as usize;
        let streams_offset = u32_at(header, 40) as usize;
        let packet_pool_bytes = u32_at(header, 48);
        let projection_bytes = u32_at(header, 52);
        let objects_end =
            checked_table_end(objects_offset, object_count, RENDER_QUAD_OBJECT_BYTES)?;
        let quads_end = checked_table_end(quads_offset, quad_count, RENDER_QUAD_RECORD_BYTES)?;
        let positions_end =
            checked_table_end(positions_offset, position_count, RENDER_QUAD_POSITION_BYTES)?;
        let runs_end = checked_table_end(runs_offset, run_count, RENDER_QUAD_RUN_BYTES)?;
        let cells_end = checked_table_end(cells_offset, cell_count, RENDER_QUAD_CELL_BYTES)?;
        if objects_offset != RENDER_QUAD_HEADER_BYTES
            || quads_offset != objects_end
            || positions_offset != quads_end
            || runs_offset != align_up_4(positions_end)
            || cells_offset != runs_end
            || streams_offset != cells_end
            || streams_offset > bytes.len()
        {
            return Err(RenderQuadPayloadError::NonCanonicalLayout);
        }
        if packet_pool_bytes as usize
            != quad_count
                .checked_mul(RENDER_QUAD_PACKET_BYTES)
                .ok_or(RenderQuadPayloadError::BadMemoryAccounting)?
            || projection_bytes as usize
                != position_count
                    .checked_mul(RENDER_QUAD_PROJECTED_POSITION_BYTES)
                    .ok_or(RenderQuadPayloadError::BadMemoryAccounting)?
        {
            return Err(RenderQuadPayloadError::BadMemoryAccounting);
        }

        let payload = Self {
            bytes,
            objects: bytes
                .get(objects_offset..objects_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            quads: bytes
                .get(quads_offset..quads_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            positions: bytes
                .get(positions_offset..positions_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            runs: bytes
                .get(runs_offset..runs_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            cells: bytes
                .get(cells_offset..cells_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            streams_offset,
            packet_pool_bytes,
            projection_bytes,
        };

        let mut expected_quad = 0usize;
        let mut expected_run = 0usize;
        for object_index in 0..object_count {
            let object = payload.object(object_index).unwrap();
            if object.quad_count == 0
                || object.quad_count as usize > RENDER_QUAD_OBJECT_MAX_QUADS
                || object.first_quad as usize != expected_quad
                || object.first_run as usize != expected_run
                || (0..3).any(|axis| object.mins[axis] > object.maxs[axis])
            {
                return Err(RenderQuadPayloadError::BadObjectRange);
            }
            let object_quad_end = expected_quad
                .checked_add(object.quad_count as usize)
                .ok_or(RenderQuadPayloadError::BadObjectRange)?;
            let object_run_end = expected_run
                .checked_add(object.run_count as usize)
                .ok_or(RenderQuadPayloadError::BadRunRange)?;
            if object_quad_end > quad_count || object_run_end > run_count {
                return Err(RenderQuadPayloadError::BadObjectRange);
            }
            let mut run_quad = expected_quad;
            for run_index in expected_run..object_run_end {
                let run = payload.run(run_index).unwrap();
                if run.quad_count == 0 || run.first_quad as usize != run_quad {
                    return Err(RenderQuadPayloadError::BadRunRange);
                }
                run_quad = run_quad
                    .checked_add(run.quad_count as usize)
                    .ok_or(RenderQuadPayloadError::BadRunRange)?;
                if run_quad > object_quad_end {
                    return Err(RenderQuadPayloadError::BadRunRange);
                }
            }
            if run_quad != object_quad_end {
                return Err(RenderQuadPayloadError::BadRunRange);
            }
            expected_quad = object_quad_end;
            expected_run = object_run_end;
        }
        if expected_quad != quad_count || expected_run != run_count {
            return Err(RenderQuadPayloadError::BadObjectRange);
        }
        for quad_index in 0..quad_count {
            if payload
                .quad(quad_index)
                .unwrap()
                .positions
                .iter()
                .any(|position| *position as usize >= position_count)
            {
                return Err(RenderQuadPayloadError::BadQuadPosition);
            }
        }

        let mut previous_leaf = None;
        let mut expected_stream = streams_offset;
        for cell_index in 0..cell_count {
            let cell = payload.cell(cell_index).unwrap();
            if previous_leaf.is_some_and(|leaf| leaf >= cell.leaf)
                || cell.stream_offset as usize != expected_stream
            {
                return Err(RenderQuadPayloadError::BadCellOrder);
            }
            let stream_bytes = (cell.command_count as usize)
                .checked_mul(RENDER_QUAD_COMMAND_BYTES)
                .ok_or(RenderQuadPayloadError::BadCellStream)?;
            expected_stream = expected_stream
                .checked_add(stream_bytes)
                .ok_or(RenderQuadPayloadError::BadCellStream)?;
            let commands = bytes
                .get(cell.stream_offset as usize..expected_stream)
                .ok_or(RenderQuadPayloadError::BadCellStream)?;
            let mut previous_object = None;
            for command_index in 0..cell.command_count as usize {
                let command = decode_command(commands, command_index).unwrap();
                if command.object as usize >= object_count
                    || previous_object.is_some_and(|object| object >= command.object)
                {
                    return Err(RenderQuadPayloadError::BadCellStream);
                }
                previous_object = Some(command.object);
            }
            previous_leaf = Some(cell.leaf);
        }
        if expected_stream != bytes.len() {
            return Err(RenderQuadPayloadError::BadCellStream);
        }
        Ok(payload)
    }

    #[inline]
    pub const fn object_count(self) -> usize {
        self.objects.len() / RENDER_QUAD_OBJECT_BYTES
    }

    #[inline]
    pub const fn quad_count(self) -> usize {
        self.quads.len() / RENDER_QUAD_RECORD_BYTES
    }

    #[inline]
    pub const fn position_count(self) -> usize {
        self.positions.len() / RENDER_QUAD_POSITION_BYTES
    }

    #[inline]
    pub const fn run_count(self) -> usize {
        self.runs.len() / RENDER_QUAD_RUN_BYTES
    }

    #[inline]
    pub const fn cell_count(self) -> usize {
        self.cells.len() / RENDER_QUAD_CELL_BYTES
    }

    #[inline]
    pub const fn packet_pool_bytes(self) -> u32 {
        self.packet_pool_bytes
    }

    #[inline]
    pub const fn projection_bytes(self) -> u32 {
        self.projection_bytes
    }

    #[inline]
    pub fn object(self, index: usize) -> Option<RenderQuadObject> {
        let bytes = table_record(self.objects, index, RENDER_QUAD_OBJECT_BYTES)?;
        Some(RenderQuadObject {
            first_quad: u16_at(bytes, 0),
            quad_count: u16_at(bytes, 2),
            first_run: u16_at(bytes, 4),
            run_count: u16_at(bytes, 6),
            mins: [i16_at(bytes, 8), i16_at(bytes, 10), i16_at(bytes, 12)],
            maxs: [i16_at(bytes, 14), i16_at(bytes, 16), i16_at(bytes, 18)],
            flags: u16_at(bytes, 20),
            plane: u16_at(bytes, 22),
        })
    }

    #[inline]
    pub fn quad(self, index: usize) -> Option<RenderQuad> {
        let bytes = table_record(self.quads, index, RENDER_QUAD_RECORD_BYTES)?;
        let mut invariant_words = [0; 8];
        for (index, word) in invariant_words.iter_mut().enumerate() {
            *word = u32_at(bytes, 8 + index * 4);
        }
        Some(RenderQuad {
            positions: [
                u16_at(bytes, 0),
                u16_at(bytes, 2),
                u16_at(bytes, 4),
                u16_at(bytes, 6),
            ],
            invariant_words,
        })
    }

    #[inline]
    pub fn position(self, index: usize) -> Option<[i16; 3]> {
        let bytes = table_record(self.positions, index, RENDER_QUAD_POSITION_BYTES)?;
        Some([i16_at(bytes, 0), i16_at(bytes, 2), i16_at(bytes, 4)])
    }

    #[inline]
    pub fn run(self, index: usize) -> Option<RenderQuadRun> {
        let bytes = table_record(self.runs, index, RENDER_QUAD_RUN_BYTES)?;
        Some(RenderQuadRun {
            first_quad: u16_at(bytes, 0),
            quad_count: u16_at(bytes, 2),
            material: u16_at(bytes, 4),
            flags: u16_at(bytes, 6),
        })
    }

    #[inline]
    pub fn cell(self, index: usize) -> Option<RenderQuadCell> {
        let bytes = table_record(self.cells, index, RENDER_QUAD_CELL_BYTES)?;
        if u16_at(bytes, 10) != 0 {
            return None;
        }
        Some(RenderQuadCell {
            leaf: u16_at(bytes, 0),
            command_count: u16_at(bytes, 2),
            stream_offset: u32_at(bytes, 4),
            flags: u16_at(bytes, 8),
        })
    }

    #[inline]
    pub fn command(self, cell: RenderQuadCell, index: usize) -> Option<RenderQuadCommand> {
        if index >= cell.command_count as usize {
            return None;
        }
        let start = cell.stream_offset as usize;
        decode_command(self.bytes.get(start..)?, index)
    }

    #[inline]
    pub const fn streams_offset(self) -> usize {
        self.streams_offset
    }
}

fn decode_command(bytes: &[u8], index: usize) -> Option<RenderQuadCommand> {
    let bytes = table_record(bytes, index, RENDER_QUAD_COMMAND_BYTES)?;
    Some(RenderQuadCommand {
        object: u16_at(bytes, 0),
        flags: u16_at(bytes, 2),
    })
}

fn table_record(bytes: &[u8], index: usize, record_bytes: usize) -> Option<&[u8]> {
    let start = index.checked_mul(record_bytes)?;
    bytes.get(start..start.checked_add(record_bytes)?)
}

fn checked_table_end(
    offset: usize,
    count: usize,
    record_bytes: usize,
) -> Result<usize, RenderQuadPayloadError> {
    offset
        .checked_add(
            count
                .checked_mul(record_bytes)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
        )
        .ok_or(RenderQuadPayloadError::BadFileSize)
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
