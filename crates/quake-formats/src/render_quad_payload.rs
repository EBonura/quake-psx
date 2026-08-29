//! Quad-native payload installed by one streamed render section.
//!
//! `QRP4` stores complete bounded source-order render objects. Each object owns
//! at most 32 consecutive faces, their fallback corners, 32 optional fixed
//! GT4s, and 255 object-local positions. Cell commands carry exact visible,
//! dynamic-facing, and template-eligible face masks. Activation can expand the
//! invariant packet words into two 52-byte packet pools while fallback paths
//! use the same streamed object instead of resident PSB render lumps.

use core::convert::TryInto;

pub const RENDER_QUAD_PAYLOAD_MAGIC: u32 = u32::from_le_bytes(*b"QRP4");
pub const RENDER_QUAD_PAYLOAD_VERSION: u16 = 4;
pub const RENDER_QUAD_HEADER_BYTES: usize = 72;
pub const RENDER_QUAD_OBJECT_BYTES: usize = 36;
pub const RENDER_QUAD_FACE_BYTES: usize = 16;
/// Activated face plus exact bounds derived once from its owned corners.
pub const RENDER_QUAD_RUNTIME_FACE_BYTES: usize = 24;
pub const RENDER_QUAD_CORNER_BYTES: usize = 8;
pub const RENDER_QUAD_RECORD_BYTES: usize = 36;
pub const RENDER_QUAD_POSITION_BYTES: usize = 6;
pub const RENDER_QUAD_RUN_BYTES: usize = 8;
pub const RENDER_QUAD_CELL_BYTES: usize = 16;
pub const RENDER_QUAD_COMMAND_BYTES: usize = 20;
/// Per-quad object-local position references retained after activation.
pub const RENDER_QUAD_REFERENCE_BYTES: usize = 4;
pub const RENDER_QUAD_PACKET_BYTES: usize = 52;
pub const RENDER_QUAD_PROJECTED_POSITION_BYTES: usize = 8;
pub const RENDER_QUAD_OBJECT_MAX_FACES: usize = 32;
pub const RENDER_QUAD_OBJECT_MAX_QUADS: usize = 32;
pub const RENDER_QUAD_OBJECT_MAX_POSITIONS: usize = 255;
/// Always-resident exact fallback object belonging to an inline brush model.
pub const RENDER_QUAD_OBJECT_SUBMODEL: u16 = 1 << 0;
pub const RENDER_QUAD_OBJECT_FLAGS: u16 = RENDER_QUAD_OBJECT_SUBMODEL;
/// The source face uses the back side of its supporting plane.
pub const RENDER_QUAD_FACE_BACKSIDE: u8 = 1 << 0;
/// Corner UV bytes already contain their final atlas coordinates.
pub const RENDER_QUAD_FACE_BAKED_UV: u8 = 1 << 1;
/// Corner light words already contain their final colour values.
pub const RENDER_QUAD_FACE_BAKED_LIGHT: u8 = 1 << 2;
pub const RENDER_QUAD_FACE_FLAGS: u8 =
    RENDER_QUAD_FACE_BACKSIDE | RENDER_QUAD_FACE_BAKED_UV | RENDER_QUAD_FACE_BAKED_LIGHT;
/// The cell owns one optional empty/water opposite-PVS merge.
pub const RENDER_QUAD_CELL_WATER_PORTAL: u16 = 1 << 0;
pub const RENDER_QUAD_CELL_FLAGS: u16 = RENDER_QUAD_CELL_WATER_PORTAL;
/// Patch the current shared texture CLUT into word three during activation.
pub const RENDER_QUAD_RUN_PATCH_CLUT: u16 = 1 << 0;
pub const RENDER_QUAD_RUN_FLAGS: u16 = RENDER_QUAD_RUN_PATCH_CLUT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderQuadPayloadError {
    TooSmall,
    BadMagic,
    BadVersion,
    BadHeaderSize,
    BadFileSize,
    NonCanonicalLayout,
    BadObjectRange,
    BadFaceRange,
    BadCornerRange,
    BadCornerPosition,
    BadQuadPosition,
    BadRunRange,
    BadCellOrder,
    BadCellStream,
    BadFaceMask,
    BadMemoryAccounting,
}

/// Checked table layout for random-access streaming of one `QRP4` payload.
///
/// The guest can validate only the fixed header, then use these offsets to
/// gather one section without retaining the complete shared dictionary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadPayloadHeader {
    object_count: u16,
    face_count: u16,
    corner_count: u16,
    quad_count: u16,
    position_count: u16,
    run_count: u16,
    cell_count: u16,
    visibility_row_bytes: u16,
    objects_offset: u32,
    faces_offset: u32,
    corners_offset: u32,
    quads_offset: u32,
    positions_offset: u32,
    runs_offset: u32,
    cells_offset: u32,
    streams_offset: u32,
    file_bytes: u32,
    packet_pool_bytes: u32,
    projection_bytes: u32,
    runtime_metadata_bytes: u32,
}

impl RenderQuadPayloadHeader {
    /// Validate the fixed header against the enclosing payload length.
    pub fn parse(bytes: &[u8], file_bytes: usize) -> Result<Self, RenderQuadPayloadError> {
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
        if u32_at(header, 56) as usize != file_bytes {
            return Err(RenderQuadPayloadError::BadFileSize);
        }
        let decoded = Self {
            object_count: u16_at(header, 8),
            face_count: u16_at(header, 10),
            corner_count: u16_at(header, 12),
            quad_count: u16_at(header, 14),
            position_count: u16_at(header, 16),
            run_count: u16_at(header, 18),
            cell_count: u16_at(header, 20),
            visibility_row_bytes: u16_at(header, 22),
            objects_offset: u32_at(header, 24),
            faces_offset: u32_at(header, 28),
            corners_offset: u32_at(header, 32),
            quads_offset: u32_at(header, 36),
            positions_offset: u32_at(header, 40),
            runs_offset: u32_at(header, 44),
            cells_offset: u32_at(header, 48),
            streams_offset: u32_at(header, 52),
            file_bytes: u32_at(header, 56),
            packet_pool_bytes: u32_at(header, 60),
            projection_bytes: u32_at(header, 64),
            runtime_metadata_bytes: u32_at(header, 68),
        };
        decoded.validate_layout()?;
        Ok(decoded)
    }

    fn validate_layout(self) -> Result<(), RenderQuadPayloadError> {
        if self.cell_count() != 0 && self.visibility_row_bytes() == 0 {
            return Err(RenderQuadPayloadError::NonCanonicalLayout);
        }
        let objects_end = self.table_end(
            self.objects_offset(),
            self.object_count(),
            RENDER_QUAD_OBJECT_BYTES,
        )?;
        let faces_end = self.table_end(
            self.faces_offset(),
            self.face_count(),
            RENDER_QUAD_FACE_BYTES,
        )?;
        let corners_end = self.table_end(
            self.corners_offset(),
            self.corner_count(),
            RENDER_QUAD_CORNER_BYTES,
        )?;
        let quads_end = self.table_end(
            self.quads_offset(),
            self.quad_count(),
            RENDER_QUAD_RECORD_BYTES,
        )?;
        let positions_end = self.table_end(
            self.positions_offset(),
            self.position_count(),
            RENDER_QUAD_POSITION_BYTES,
        )?;
        let runs_end =
            self.table_end(self.runs_offset(), self.run_count(), RENDER_QUAD_RUN_BYTES)?;
        let cells_end = self.table_end(
            self.cells_offset(),
            self.cell_count(),
            RENDER_QUAD_CELL_BYTES,
        )?;
        let visibility_end = cells_end
            .checked_add(
                self.cell_count()
                    .checked_mul(self.visibility_row_bytes())
                    .and_then(|bytes| bytes.checked_mul(2))
                    .ok_or(RenderQuadPayloadError::NonCanonicalLayout)?,
            )
            .ok_or(RenderQuadPayloadError::NonCanonicalLayout)?;
        if self.objects_offset() != RENDER_QUAD_HEADER_BYTES
            || self.faces_offset() != objects_end
            || self.corners_offset() != faces_end
            || self.quads_offset() != corners_end
            || self.positions_offset() != quads_end
            || self.runs_offset() != align_up_4(positions_end)
            || self.cells_offset() != runs_end
            || self.streams_offset() != visibility_end
            || self.streams_offset() > self.file_bytes()
        {
            return Err(RenderQuadPayloadError::NonCanonicalLayout);
        }
        if self.packet_pool_bytes() as usize
            != self
                .quad_count()
                .checked_mul(RENDER_QUAD_PACKET_BYTES)
                .ok_or(RenderQuadPayloadError::BadMemoryAccounting)?
            || self.projection_bytes() as usize
                != self
                    .position_count()
                    .checked_mul(RENDER_QUAD_PROJECTED_POSITION_BYTES)
                    .ok_or(RenderQuadPayloadError::BadMemoryAccounting)?
        {
            return Err(RenderQuadPayloadError::BadMemoryAccounting);
        }
        let stream_bytes = self.file_bytes() - self.streams_offset();
        let expected_runtime_metadata = objects_end
            .checked_sub(self.objects_offset())
            .and_then(|bytes| {
                bytes.checked_add(
                    self.face_count()
                        .checked_mul(RENDER_QUAD_RUNTIME_FACE_BYTES)?,
                )
            })
            .and_then(|bytes| bytes.checked_add(corners_end - self.corners_offset()))
            .and_then(|bytes| {
                bytes.checked_add(self.quad_count().checked_mul(RENDER_QUAD_REFERENCE_BYTES)?)
            })
            .and_then(|bytes| bytes.checked_add(positions_end - self.positions_offset()))
            .and_then(|bytes| bytes.checked_add(cells_end - self.cells_offset()))
            .and_then(|bytes| bytes.checked_add(visibility_end - cells_end))
            .and_then(|bytes| bytes.checked_add(stream_bytes))
            .ok_or(RenderQuadPayloadError::BadMemoryAccounting)?;
        if self.runtime_metadata_bytes() as usize != expected_runtime_metadata {
            return Err(RenderQuadPayloadError::BadMemoryAccounting);
        }
        Ok(())
    }

    fn table_end(
        self,
        offset: usize,
        count: usize,
        record_bytes: usize,
    ) -> Result<usize, RenderQuadPayloadError> {
        let end = checked_table_end(offset, count, record_bytes)?;
        if end > self.file_bytes() {
            return Err(RenderQuadPayloadError::BadFileSize);
        }
        Ok(end)
    }

    #[inline]
    pub const fn object_count(self) -> usize {
        self.object_count as usize
    }

    #[inline]
    pub const fn face_count(self) -> usize {
        self.face_count as usize
    }

    #[inline]
    pub const fn corner_count(self) -> usize {
        self.corner_count as usize
    }

    #[inline]
    pub const fn quad_count(self) -> usize {
        self.quad_count as usize
    }

    #[inline]
    pub const fn position_count(self) -> usize {
        self.position_count as usize
    }

    #[inline]
    pub const fn run_count(self) -> usize {
        self.run_count as usize
    }

    #[inline]
    pub const fn cell_count(self) -> usize {
        self.cell_count as usize
    }

    #[inline]
    pub const fn visibility_row_bytes(self) -> usize {
        self.visibility_row_bytes as usize
    }

    #[inline]
    pub const fn objects_offset(self) -> usize {
        self.objects_offset as usize
    }

    #[inline]
    pub const fn faces_offset(self) -> usize {
        self.faces_offset as usize
    }

    #[inline]
    pub const fn corners_offset(self) -> usize {
        self.corners_offset as usize
    }

    #[inline]
    pub const fn quads_offset(self) -> usize {
        self.quads_offset as usize
    }

    #[inline]
    pub const fn positions_offset(self) -> usize {
        self.positions_offset as usize
    }

    #[inline]
    pub const fn runs_offset(self) -> usize {
        self.runs_offset as usize
    }

    #[inline]
    pub const fn cells_offset(self) -> usize {
        self.cells_offset as usize
    }

    #[inline]
    pub const fn streams_offset(self) -> usize {
        self.streams_offset as usize
    }

    #[inline]
    pub const fn file_bytes(self) -> usize {
        self.file_bytes as usize
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
    pub const fn runtime_metadata_bytes(self) -> u32 {
        self.runtime_metadata_bytes
    }

    #[inline]
    pub fn object_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.objects_offset(),
            self.object_count(),
            index,
            RENDER_QUAD_OBJECT_BYTES,
        )
    }

    #[inline]
    pub fn face_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.faces_offset(),
            self.face_count(),
            index,
            RENDER_QUAD_FACE_BYTES,
        )
    }

    #[inline]
    pub fn corner_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.corners_offset(),
            self.corner_count(),
            index,
            RENDER_QUAD_CORNER_BYTES,
        )
    }

    #[inline]
    pub fn quad_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.quads_offset(),
            self.quad_count(),
            index,
            RENDER_QUAD_RECORD_BYTES,
        )
    }

    #[inline]
    pub fn position_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.positions_offset(),
            self.position_count(),
            index,
            RENDER_QUAD_POSITION_BYTES,
        )
    }

    #[inline]
    pub fn run_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.runs_offset(),
            self.run_count(),
            index,
            RENDER_QUAD_RUN_BYTES,
        )
    }

    #[inline]
    pub fn cell_offset(self, index: usize) -> Option<usize> {
        record_offset(
            self.cells_offset(),
            self.cell_count(),
            index,
            RENDER_QUAD_CELL_BYTES,
        )
    }

    #[inline]
    pub fn visibility_offset(self, cell: usize, portal: bool) -> Option<usize> {
        if cell >= self.cell_count() {
            return None;
        }
        let row = cell.checked_mul(2)?.checked_add(usize::from(portal))?;
        self.cells_offset()
            .checked_add(self.cell_count().checked_mul(RENDER_QUAD_CELL_BYTES)?)?
            .checked_add(row.checked_mul(self.visibility_row_bytes())?)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadObject {
    pub first_face: u16,
    pub face_count: u16,
    pub first_corner: u16,
    pub corner_count: u16,
    pub first_quad: u16,
    pub quad_count: u16,
    pub first_position: u16,
    pub position_count: u16,
    pub first_run: u16,
    pub run_count: u16,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadFace {
    /// Original PSB5 face index used to merge accelerated and fallback work.
    pub source_face: u16,
    pub first_corner: u16,
    pub first_quad: u16,
    pub quad_count: u16,
    pub plane: u16,
    pub material: u16,
    pub flags: u8,
    pub corner_count: u8,
    pub light_styles: [u8; 2],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadCorner {
    /// Object-local u8 index into the object's contiguous position range.
    pub position: u8,
    pub texture: [u8; 2],
    pub light: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuad {
    /// Object-local u8 indices into the object's contiguous position range.
    pub positions: [u8; 4],
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
    pub portal_leaf: u16,
    pub portal_plane: i16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadCommand {
    pub object: u16,
    pub flags: u16,
    /// Faces selected by exact PVS and conservative leaf-facing classification.
    pub visible_faces: u32,
    /// Additional faces selected only when the cell's water portal is open.
    pub portal_faces: u32,
    /// Selected faces whose plane still needs a per-frame facing test.
    pub dynamic_faces: u32,
    /// Visible faces which may use their installed fixed GT4 templates.
    pub template_faces: u32,
}

impl RenderQuadObject {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_OBJECT_BYTES)?;
        if u16_at(bytes, 34) != 0 {
            return None;
        }
        Some(Self {
            first_face: u16_at(bytes, 0),
            face_count: u16_at(bytes, 2),
            first_corner: u16_at(bytes, 4),
            corner_count: u16_at(bytes, 6),
            first_quad: u16_at(bytes, 8),
            quad_count: u16_at(bytes, 10),
            first_position: u16_at(bytes, 12),
            position_count: u16_at(bytes, 14),
            first_run: u16_at(bytes, 16),
            run_count: u16_at(bytes, 18),
            mins: [i16_at(bytes, 20), i16_at(bytes, 22), i16_at(bytes, 24)],
            maxs: [i16_at(bytes, 26), i16_at(bytes, 28), i16_at(bytes, 30)],
            flags: u16_at(bytes, 32),
        })
    }
}

impl RenderQuadFace {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_FACE_BYTES)?;
        Some(Self {
            source_face: u16_at(bytes, 0),
            first_corner: u16_at(bytes, 2),
            first_quad: u16_at(bytes, 4),
            quad_count: u16_at(bytes, 6),
            plane: u16_at(bytes, 8),
            material: u16_at(bytes, 10),
            flags: bytes[12],
            corner_count: bytes[13],
            light_styles: [bytes[14], bytes[15]],
        })
    }
}

impl RenderQuadCorner {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_CORNER_BYTES)?;
        if bytes[3] != 0 {
            return None;
        }
        Some(Self {
            position: bytes[0],
            texture: [bytes[1], bytes[2]],
            light: u32_at(bytes, 4),
        })
    }
}

impl RenderQuad {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_RECORD_BYTES)?;
        let mut invariant_words = [0; 8];
        for (index, word) in invariant_words.iter_mut().enumerate() {
            *word = u32_at(bytes, 4 + index * 4);
        }
        Some(Self {
            positions: [bytes[0], bytes[1], bytes[2], bytes[3]],
            invariant_words,
        })
    }
}

impl RenderQuadRun {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_RUN_BYTES)?;
        Some(Self {
            first_quad: u16_at(bytes, 0),
            quad_count: u16_at(bytes, 2),
            material: u16_at(bytes, 4),
            flags: u16_at(bytes, 6),
        })
    }
}

impl RenderQuadCell {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_CELL_BYTES)?;
        if u16_at(bytes, 14) != 0 {
            return None;
        }
        Some(Self {
            leaf: u16_at(bytes, 0),
            command_count: u16_at(bytes, 2),
            stream_offset: u32_at(bytes, 4),
            flags: u16_at(bytes, 8),
            portal_leaf: u16_at(bytes, 10),
            portal_plane: i16_at(bytes, 12),
        })
    }
}

impl RenderQuadCommand {
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RENDER_QUAD_COMMAND_BYTES)?;
        Some(Self {
            object: u16_at(bytes, 0),
            flags: u16_at(bytes, 2),
            visible_faces: u32_at(bytes, 4),
            portal_faces: u32_at(bytes, 8),
            dynamic_faces: u32_at(bytes, 12),
            template_faces: u32_at(bytes, 16),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderQuadPayload<'a> {
    bytes: &'a [u8],
    objects: &'a [u8],
    faces: &'a [u8],
    corners: &'a [u8],
    quads: &'a [u8],
    positions: &'a [u8],
    runs: &'a [u8],
    cells: &'a [u8],
    visibility_rows: &'a [u8],
    visibility_row_bytes: usize,
    streams_offset: usize,
    packet_pool_bytes: u32,
    projection_bytes: u32,
    runtime_metadata_bytes: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderQuadSectionMemory {
    /// Canonical compact bytes staged while activating the section.
    pub staging_bytes: u32,
    /// Retained CPU metadata plus projected-position storage.
    pub activation_bytes: u32,
    /// Installed fixed packet bytes in one display pool.
    pub packet_pool_bytes: u32,
    /// Projected-position portion of `activation_bytes`.
    pub projection_bytes: u32,
}

impl<'a> RenderQuadPayload<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderQuadPayloadError> {
        let header = RenderQuadPayloadHeader::parse(bytes, bytes.len())?;
        let object_count = header.object_count();
        let face_count = header.face_count();
        let corner_count = header.corner_count();
        let quad_count = header.quad_count();
        let position_count = header.position_count();
        let run_count = header.run_count();
        let cell_count = header.cell_count();
        let visibility_row_bytes = header.visibility_row_bytes();
        let objects_offset = header.objects_offset();
        let faces_offset = header.faces_offset();
        let corners_offset = header.corners_offset();
        let quads_offset = header.quads_offset();
        let positions_offset = header.positions_offset();
        let runs_offset = header.runs_offset();
        let cells_offset = header.cells_offset();
        let streams_offset = header.streams_offset();
        let objects_end =
            checked_table_end(objects_offset, object_count, RENDER_QUAD_OBJECT_BYTES)?;
        let faces_end = checked_table_end(faces_offset, face_count, RENDER_QUAD_FACE_BYTES)?;
        let corners_end =
            checked_table_end(corners_offset, corner_count, RENDER_QUAD_CORNER_BYTES)?;
        let quads_end = checked_table_end(quads_offset, quad_count, RENDER_QUAD_RECORD_BYTES)?;
        let positions_end =
            checked_table_end(positions_offset, position_count, RENDER_QUAD_POSITION_BYTES)?;
        let runs_end = checked_table_end(runs_offset, run_count, RENDER_QUAD_RUN_BYTES)?;
        let cells_end = checked_table_end(cells_offset, cell_count, RENDER_QUAD_CELL_BYTES)?;
        let visibility_end = cells_end
            .checked_add(
                cell_count
                    .checked_mul(visibility_row_bytes)
                    .and_then(|bytes| bytes.checked_mul(2))
                    .ok_or(RenderQuadPayloadError::NonCanonicalLayout)?,
            )
            .ok_or(RenderQuadPayloadError::NonCanonicalLayout)?;

        let payload = Self {
            bytes,
            objects: bytes
                .get(objects_offset..objects_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            faces: bytes
                .get(faces_offset..faces_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            corners: bytes
                .get(corners_offset..corners_end)
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
            visibility_rows: bytes
                .get(cells_end..visibility_end)
                .ok_or(RenderQuadPayloadError::BadFileSize)?,
            visibility_row_bytes,
            streams_offset,
            packet_pool_bytes: header.packet_pool_bytes(),
            projection_bytes: header.projection_bytes(),
            runtime_metadata_bytes: header.runtime_metadata_bytes(),
        };

        let mut expected_face = 0usize;
        let mut expected_corner = 0usize;
        let mut expected_quad = 0usize;
        let mut expected_position = 0usize;
        let mut expected_run = 0usize;
        let mut previous_source_face = None;
        for object_index in 0..object_count {
            let object = payload.object(object_index).unwrap();
            if object.face_count == 0
                || object.face_count as usize > RENDER_QUAD_OBJECT_MAX_FACES
                || object.corner_count == 0
                || object.quad_count as usize > RENDER_QUAD_OBJECT_MAX_QUADS
                || object.position_count == 0
                || object.position_count as usize > RENDER_QUAD_OBJECT_MAX_POSITIONS
                || object.first_face as usize != expected_face
                || object.first_corner as usize != expected_corner
                || object.first_quad as usize != expected_quad
                || object.first_position as usize != expected_position
                || object.first_run as usize != expected_run
                || object.flags & !RENDER_QUAD_OBJECT_FLAGS != 0
                || object.flags & RENDER_QUAD_OBJECT_SUBMODEL != 0
                    && (object.quad_count != 0 || object.run_count != 0)
                || u16_at(
                    payload.objects,
                    object_index * RENDER_QUAD_OBJECT_BYTES + 34,
                ) != 0
                || (0..3).any(|axis| object.mins[axis] > object.maxs[axis])
            {
                return Err(RenderQuadPayloadError::BadObjectRange);
            }
            let object_face_end = expected_face
                .checked_add(object.face_count as usize)
                .ok_or(RenderQuadPayloadError::BadFaceRange)?;
            let object_corner_end = expected_corner
                .checked_add(object.corner_count as usize)
                .ok_or(RenderQuadPayloadError::BadCornerRange)?;
            let object_quad_end = expected_quad
                .checked_add(object.quad_count as usize)
                .ok_or(RenderQuadPayloadError::BadObjectRange)?;
            let object_position_end = expected_position
                .checked_add(object.position_count as usize)
                .ok_or(RenderQuadPayloadError::BadObjectRange)?;
            let object_run_end = expected_run
                .checked_add(object.run_count as usize)
                .ok_or(RenderQuadPayloadError::BadRunRange)?;
            if object_face_end > face_count
                || object_corner_end > corner_count
                || object_quad_end > quad_count
                || object_position_end > position_count
                || object_run_end > run_count
            {
                return Err(RenderQuadPayloadError::BadObjectRange);
            }

            let mut face_corner = expected_corner;
            let mut face_quad = expected_quad;
            let mut face_run = expected_run;
            for face_index in expected_face..object_face_end {
                let face = payload.face(face_index).unwrap();
                if face.corner_count < 3
                    || face.first_corner as usize != face_corner
                    || face.first_quad as usize != face_quad
                    || face.flags & !RENDER_QUAD_FACE_FLAGS != 0
                    || previous_source_face.is_some_and(|source| source >= face.source_face)
                {
                    return Err(RenderQuadPayloadError::BadFaceRange);
                }
                face_corner = face_corner
                    .checked_add(face.corner_count as usize)
                    .ok_or(RenderQuadPayloadError::BadCornerRange)?;
                if face_corner > object_corner_end {
                    return Err(RenderQuadPayloadError::BadCornerRange);
                }
                face_quad = face_quad
                    .checked_add(face.quad_count as usize)
                    .ok_or(RenderQuadPayloadError::BadFaceRange)?;
                if face_quad > object_quad_end {
                    return Err(RenderQuadPayloadError::BadFaceRange);
                }
                if face.quad_count != 0 {
                    if face_run >= object_run_end {
                        return Err(RenderQuadPayloadError::BadRunRange);
                    }
                    let run = payload.run(face_run).unwrap();
                    if run.first_quad != face.first_quad
                        || run.quad_count != face.quad_count
                        || run.material != face.material
                        || run.flags & !RENDER_QUAD_RUN_FLAGS != 0
                    {
                        return Err(RenderQuadPayloadError::BadRunRange);
                    }
                    face_run += 1;
                }
                previous_source_face = Some(face.source_face);
            }
            if face_corner != object_corner_end || face_quad != object_quad_end {
                return Err(RenderQuadPayloadError::BadFaceRange);
            }
            if face_run != object_run_end {
                return Err(RenderQuadPayloadError::BadRunRange);
            }

            for corner_index in expected_corner..object_corner_end {
                let record =
                    table_record(payload.corners, corner_index, RENDER_QUAD_CORNER_BYTES).unwrap();
                if record[3] != 0
                    || payload.corner(corner_index).unwrap().position as usize
                        >= object.position_count as usize
                {
                    return Err(RenderQuadPayloadError::BadCornerPosition);
                }
            }

            for quad_index in expected_quad..object_quad_end {
                if payload
                    .quad(quad_index)
                    .unwrap()
                    .positions
                    .iter()
                    .any(|position| *position as usize >= object.position_count as usize)
                {
                    return Err(RenderQuadPayloadError::BadQuadPosition);
                }
            }

            expected_face = object_face_end;
            expected_corner = object_corner_end;
            expected_quad = object_quad_end;
            expected_position = object_position_end;
            expected_run = object_run_end;
        }
        if expected_face != face_count
            || expected_corner != corner_count
            || expected_quad != quad_count
            || expected_position != position_count
            || expected_run != run_count
        {
            return Err(RenderQuadPayloadError::BadObjectRange);
        }

        let mut previous_leaf = None;
        let mut expected_stream = streams_offset;
        for cell_index in 0..cell_count {
            let cell = payload.cell(cell_index).unwrap();
            if previous_leaf.is_some_and(|leaf| leaf >= cell.leaf)
                || cell.stream_offset as usize != expected_stream
                || cell.flags & !RENDER_QUAD_CELL_FLAGS != 0
                || (cell.flags & RENDER_QUAD_CELL_WATER_PORTAL == 0
                    && (cell.portal_leaf != u16::MAX || cell.portal_plane != -1))
                || (cell.flags & RENDER_QUAD_CELL_WATER_PORTAL != 0
                    && (cell.portal_leaf == 0
                        || cell.portal_leaf == u16::MAX
                        || cell.portal_leaf == cell.leaf
                        || cell.portal_plane < 0))
            {
                return Err(RenderQuadPayloadError::BadCellOrder);
            }
            if cell.flags & RENDER_QUAD_CELL_WATER_PORTAL == 0
                && payload
                    .portal_visibility(cell_index)
                    .is_none_or(|row| row.iter().any(|byte| *byte != 0))
            {
                return Err(RenderQuadPayloadError::BadCellStream);
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
                let object_index = command.object as usize;
                if object_index >= object_count
                    || previous_object.is_some_and(|object| object >= command.object)
                {
                    return Err(RenderQuadPayloadError::BadCellStream);
                }
                if payload.object(object_index).unwrap().flags & RENDER_QUAD_OBJECT_SUBMODEL != 0 {
                    return Err(RenderQuadPayloadError::BadCellStream);
                }
                let face_count = payload.object(object_index).unwrap().face_count as usize;
                let valid_mask = if face_count == 32 {
                    u32::MAX
                } else {
                    (1u32 << face_count) - 1
                };
                let selected_faces = command.visible_faces | command.portal_faces;
                if selected_faces == 0
                    || command.visible_faces & !valid_mask != 0
                    || command.portal_faces & !valid_mask != 0
                    || command.portal_faces & command.visible_faces != 0
                    || command.portal_faces != 0 && cell.flags & RENDER_QUAD_CELL_WATER_PORTAL == 0
                    || command.dynamic_faces & !selected_faces != 0
                    || command.template_faces & !selected_faces != 0
                {
                    return Err(RenderQuadPayloadError::BadFaceMask);
                }
                let object = payload.object(object_index).unwrap();
                for local_face in 0..face_count {
                    if command.template_faces & (1 << local_face) != 0
                        && payload
                            .face(object.first_face as usize + local_face)
                            .unwrap()
                            .quad_count
                            == 0
                    {
                        return Err(RenderQuadPayloadError::BadFaceMask);
                    }
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
    pub const fn face_count(self) -> usize {
        self.faces.len() / RENDER_QUAD_FACE_BYTES
    }

    #[inline]
    pub const fn corner_count(self) -> usize {
        self.corners.len() / RENDER_QUAD_CORNER_BYTES
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

    /// Retained object/face/reference/position/cell/command bytes after the
    /// compact invariant template and activation-run tables are discarded.
    #[inline]
    pub const fn runtime_metadata_bytes(self) -> u32 {
        self.runtime_metadata_bytes
    }

    /// Compact fallback bytes kept for inline brush models independently of
    /// the active world section. Submodel objects intentionally have no fixed
    /// packet templates or persistent projected-position storage.
    pub fn resident_object_bytes(self) -> Option<u32> {
        let mut bytes = 0usize;
        for object_index in 0..self.object_count() {
            let object = self.object(object_index)?;
            if object.flags & RENDER_QUAD_OBJECT_SUBMODEL == 0 {
                continue;
            }
            bytes = bytes
                .checked_add(RENDER_QUAD_OBJECT_BYTES)?
                .checked_add(
                    (object.face_count as usize).checked_mul(RENDER_QUAD_RUNTIME_FACE_BYTES)?,
                )?
                .checked_add((object.corner_count as usize).checked_mul(RENDER_QUAD_CORNER_BYTES)?)?
                .checked_add(
                    (object.position_count as usize).checked_mul(RENDER_QUAD_POSITION_BYTES)?,
                )?;
        }
        bytes.try_into().ok()
    }

    #[inline]
    pub fn object(self, index: usize) -> Option<RenderQuadObject> {
        let bytes = table_record(self.objects, index, RENDER_QUAD_OBJECT_BYTES)?;
        RenderQuadObject::decode(bytes)
    }

    #[inline]
    pub fn face(self, index: usize) -> Option<RenderQuadFace> {
        let bytes = table_record(self.faces, index, RENDER_QUAD_FACE_BYTES)?;
        RenderQuadFace::decode(bytes)
    }

    #[inline]
    pub fn corner(self, index: usize) -> Option<RenderQuadCorner> {
        let bytes = table_record(self.corners, index, RENDER_QUAD_CORNER_BYTES)?;
        RenderQuadCorner::decode(bytes)
    }

    #[inline]
    pub fn quad(self, index: usize) -> Option<RenderQuad> {
        let bytes = table_record(self.quads, index, RENDER_QUAD_RECORD_BYTES)?;
        RenderQuad::decode(bytes)
    }

    #[inline]
    pub fn position(self, index: usize) -> Option<[i16; 3]> {
        let bytes = table_record(self.positions, index, RENDER_QUAD_POSITION_BYTES)?;
        Some([i16_at(bytes, 0), i16_at(bytes, 2), i16_at(bytes, 4)])
    }

    #[inline]
    pub fn run(self, index: usize) -> Option<RenderQuadRun> {
        let bytes = table_record(self.runs, index, RENDER_QUAD_RUN_BYTES)?;
        RenderQuadRun::decode(bytes)
    }

    #[inline]
    pub fn cell(self, index: usize) -> Option<RenderQuadCell> {
        let bytes = table_record(self.cells, index, RENDER_QUAD_CELL_BYTES)?;
        RenderQuadCell::decode(bytes)
    }

    #[inline]
    pub fn visibility(self, cell_index: usize) -> Option<&'a [u8]> {
        let rows = table_record(
            self.visibility_rows,
            cell_index,
            self.visibility_row_bytes.checked_mul(2)?,
        )?;
        rows.get(..self.visibility_row_bytes)
    }

    #[inline]
    pub fn portal_visibility(self, cell_index: usize) -> Option<&'a [u8]> {
        let rows = table_record(
            self.visibility_rows,
            cell_index,
            self.visibility_row_bytes.checked_mul(2)?,
        )?;
        rows.get(self.visibility_row_bytes..)
    }

    #[inline]
    pub const fn visibility_row_bytes(self) -> usize {
        self.visibility_row_bytes
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

    /// Derive the exact compact and activated footprint for a consecutive
    /// cell range without allocating. Render objects are counted once even
    /// when multiple cells reference them. Fallback-only objects retain their
    /// complete topology but do not install packet templates.
    pub fn section_memory(
        self,
        first_cell: usize,
        cell_count: usize,
    ) -> Option<RenderQuadSectionMemory> {
        let end_cell = first_cell.checked_add(cell_count)?;
        if cell_count == 0 || end_cell > self.cell_count() {
            return None;
        }
        let mut object_count = 0usize;
        let mut face_count = 0usize;
        let mut corner_count = 0usize;
        let mut quad_count = 0usize;
        let mut position_count = 0usize;
        let mut run_count = 0usize;
        let mut command_count = 0usize;
        for cell_index in first_cell..end_cell {
            command_count =
                command_count.checked_add(self.cell(cell_index)?.command_count as usize)?;
        }
        for object_index in 0..self.object_count() {
            let mut referenced = false;
            let mut template_faces = 0u32;
            for cell_index in first_cell..end_cell {
                let cell = self.cell(cell_index)?;
                for command_index in 0..cell.command_count as usize {
                    let command = self.command(cell, command_index)?;
                    match (command.object as usize).cmp(&object_index) {
                        core::cmp::Ordering::Less => {}
                        core::cmp::Ordering::Equal => {
                            referenced = true;
                            template_faces |= command.template_faces;
                            break;
                        }
                        core::cmp::Ordering::Greater => break,
                    }
                }
            }
            if !referenced {
                continue;
            }
            let object = self.object(object_index)?;
            object_count = object_count.checked_add(1)?;
            face_count = face_count.checked_add(object.face_count as usize)?;
            corner_count = corner_count.checked_add(object.corner_count as usize)?;
            position_count = position_count.checked_add(object.position_count as usize)?;
            for local_face in 0..object.face_count as usize {
                if template_faces & (1 << local_face) == 0 {
                    continue;
                }
                let face = self.face(object.first_face as usize + local_face)?;
                quad_count = quad_count.checked_add(face.quad_count as usize)?;
                run_count = run_count.checked_add(1)?;
            }
        }

        let objects_bytes = object_count.checked_mul(RENDER_QUAD_OBJECT_BYTES)?;
        let faces_bytes = face_count.checked_mul(RENDER_QUAD_FACE_BYTES)?;
        let runtime_faces_bytes = face_count.checked_mul(RENDER_QUAD_RUNTIME_FACE_BYTES)?;
        let corners_bytes = corner_count.checked_mul(RENDER_QUAD_CORNER_BYTES)?;
        let quads_bytes = quad_count.checked_mul(RENDER_QUAD_RECORD_BYTES)?;
        let positions_bytes = position_count.checked_mul(RENDER_QUAD_POSITION_BYTES)?;
        let runs_bytes = run_count.checked_mul(RENDER_QUAD_RUN_BYTES)?;
        let cells_bytes = cell_count.checked_mul(RENDER_QUAD_CELL_BYTES)?;
        let visibility_bytes = cell_count
            .checked_mul(self.visibility_row_bytes)?
            .checked_mul(2)?;
        let commands_bytes = command_count.checked_mul(RENDER_QUAD_COMMAND_BYTES)?;
        let positions_end = RENDER_QUAD_HEADER_BYTES
            .checked_add(objects_bytes)?
            .checked_add(faces_bytes)?
            .checked_add(corners_bytes)?
            .checked_add(quads_bytes)?
            .checked_add(positions_bytes)?;
        let staging_bytes = align_up_4(positions_end)
            .checked_add(runs_bytes)?
            .checked_add(cells_bytes)?
            .checked_add(visibility_bytes)?
            .checked_add(commands_bytes)?;
        let runtime_metadata_bytes = objects_bytes
            .checked_add(runtime_faces_bytes)?
            .checked_add(corners_bytes)?
            .checked_add(quad_count.checked_mul(RENDER_QUAD_REFERENCE_BYTES)?)?
            .checked_add(positions_bytes)?
            .checked_add(cells_bytes)?
            .checked_add(visibility_bytes)?
            .checked_add(commands_bytes)?;
        let projection_bytes = position_count.checked_mul(RENDER_QUAD_PROJECTED_POSITION_BYTES)?;
        Some(RenderQuadSectionMemory {
            staging_bytes: staging_bytes.try_into().ok()?,
            activation_bytes: runtime_metadata_bytes
                .checked_add(projection_bytes)?
                .try_into()
                .ok()?,
            packet_pool_bytes: quad_count
                .checked_mul(RENDER_QUAD_PACKET_BYTES)?
                .try_into()
                .ok()?,
            projection_bytes: projection_bytes.try_into().ok()?,
        })
    }
}

fn decode_command(bytes: &[u8], index: usize) -> Option<RenderQuadCommand> {
    let bytes = table_record(bytes, index, RENDER_QUAD_COMMAND_BYTES)?;
    RenderQuadCommand::decode(bytes)
}

fn table_record(bytes: &[u8], index: usize, record_bytes: usize) -> Option<&[u8]> {
    let start = index.checked_mul(record_bytes)?;
    bytes.get(start..start.checked_add(record_bytes)?)
}

fn record_offset(
    table_offset: usize,
    count: usize,
    index: usize,
    record_bytes: usize,
) -> Option<usize> {
    if index >= count {
        return None;
    }
    table_offset.checked_add(index.checked_mul(record_bytes)?)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_payload() -> [u8; RENDER_QUAD_HEADER_BYTES] {
        let mut bytes = [0u8; RENDER_QUAD_HEADER_BYTES];
        bytes[0..4].copy_from_slice(&RENDER_QUAD_PAYLOAD_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&RENDER_QUAD_PAYLOAD_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(RENDER_QUAD_HEADER_BYTES as u16).to_le_bytes());
        for offset in (24..=52).step_by(4) {
            bytes[offset..offset + 4]
                .copy_from_slice(&(RENDER_QUAD_HEADER_BYTES as u32).to_le_bytes());
        }
        bytes[56..60].copy_from_slice(&(RENDER_QUAD_HEADER_BYTES as u32).to_le_bytes());
        bytes
    }

    #[test]
    fn header_only_layout_matches_the_full_parser() {
        let bytes = empty_payload();
        let header = RenderQuadPayloadHeader::parse(&bytes, bytes.len()).unwrap();
        assert_eq!(header.file_bytes(), RENDER_QUAD_HEADER_BYTES);
        assert_eq!(header.objects_offset(), RENDER_QUAD_HEADER_BYTES);
        assert_eq!(header.streams_offset(), RENDER_QUAD_HEADER_BYTES);
        assert_eq!(header.object_offset(0), None);
        assert_eq!(header.visibility_offset(0, false), None);
        assert!(RenderQuadPayload::parse(&bytes).is_ok());
    }

    #[test]
    fn header_only_layout_rejects_noncanonical_random_access_offsets() {
        let mut bytes = empty_payload();
        bytes[28..32].copy_from_slice(&((RENDER_QUAD_HEADER_BYTES - 4) as u32).to_le_bytes());
        assert_eq!(
            RenderQuadPayloadHeader::parse(&bytes, bytes.len()),
            Err(RenderQuadPayloadError::NonCanonicalLayout)
        );
    }

    #[test]
    fn streamed_record_decoders_reject_reserved_bytes() {
        let mut object = [0u8; RENDER_QUAD_OBJECT_BYTES];
        object[34] = 1;
        assert_eq!(RenderQuadObject::decode(&object), None);
        let mut corner = [0u8; RENDER_QUAD_CORNER_BYTES];
        corner[3] = 1;
        assert_eq!(RenderQuadCorner::decode(&corner), None);
        let mut cell = [0u8; RENDER_QUAD_CELL_BYTES];
        cell[14] = 1;
        assert_eq!(RenderQuadCell::decode(&cell), None);
    }
}
