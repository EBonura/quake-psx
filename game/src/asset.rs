//! Quake-specific streaming and VRAM policy over PSoXide's resident XBSP map.

use alloc::vec::Vec;

use psx_bsp::resident::{
    IndexedVertices, MapLoadError as SharedMapLoadError, ResidentMap as SharedResidentMap,
    TEXTURE_ROW_BYTES, TEXTURE_VRAM_MAX_ROWS, TEXTURE_VRAM_WIDTH, TEXTURE_VRAM_X,
};
use psx_math::int32::mul_q12_i32_wide;
use psx_vram::VramRect;
use quake_formats::{
    episode_directory_index, episode_directory_index_or_try, leaf_bounds_at, leaf_portal_graph,
    AliasModelTable, BrushModel, CachedIndexReader, ClipNode, CompactNode, Face, GraphicsPicture,
    GraphicsPictureId, Leaf, LeafBounds, LeafPortalGraph, LumpKind, LumpRange, MapEntity, Node,
    Plane, PsbError, PsbIndex, PsbVersion, ReadAt, RecordSlice, TextureInfo, Vec3I32, Vertex,
    EPISODE_DIRECTORY_BYTES,
    GRAPHICS_PICTURE_RECORD_BYTES, GRAPHICS_WEAPON_ICON_BYTES, RESIDENT_MAP_ARENA_BYTES,
    TEXTURE_LIQUID,
};

use crate::platform::{self, StorageError};

const EPISODE_DIRECTORY_CHUNK: u32 = 2;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EpisodeMap {
    Start,
    E1M1,
    E1M2,
    E1M3,
    E1M4,
    E1M5,
    E1M6,
    E1M7,
    E1M8,
}

impl EpisodeMap {
    pub const ALL: [Self; 9] = [
        Self::Start,
        Self::E1M1,
        Self::E1M2,
        Self::E1M3,
        Self::E1M4,
        Self::E1M5,
        Self::E1M6,
        Self::E1M7,
        Self::E1M8,
    ];

    #[optimize(size)]
    pub const fn chunk_id(self) -> u32 {
        match self {
            Self::Start => 100,
            Self::E1M1 => 101,
            Self::E1M2 => 102,
            Self::E1M3 => 103,
            Self::E1M4 => 104,
            Self::E1M5 => 105,
            Self::E1M6 => 106,
            Self::E1M7 => 107,
            Self::E1M8 => 108,
        }
    }

    #[optimize(size)]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Start => "ENTRANCE",
            Self::E1M1 => "THE SLIPGATE COMPLEX",
            Self::E1M2 => "CASTLE OF THE DAMNED",
            Self::E1M3 => "THE NECROPOLIS",
            Self::E1M4 => "THE GRISLY GROTTO",
            Self::E1M5 => "GLOOM KEEP",
            Self::E1M6 => "THE DOOR TO CHTHON",
            Self::E1M7 => "THE HOUSE OF CHTHON",
            Self::E1M8 => "ZIGGURAT VERTIGO",
        }
    }

    /// Resolve the uppercase map names retained by `trigger_changelevel`.
    #[optimize(size)]
    pub fn from_cooked_name(name: &[u8]) -> Option<Self> {
        match name {
            b"START" | b"start" => Some(Self::Start),
            b"E1M1" | b"e1m1" => Some(Self::E1M1),
            b"E1M2" | b"e1m2" => Some(Self::E1M2),
            b"E1M3" | b"e1m3" => Some(Self::E1M3),
            b"E1M4" | b"e1m4" => Some(Self::E1M4),
            b"E1M5" | b"e1m5" => Some(Self::E1M5),
            b"E1M6" | b"e1m6" => Some(Self::E1M6),
            b"E1M7" | b"e1m7" => Some(Self::E1M7),
            b"E1M8" | b"e1m8" => Some(Self::E1M8),
            _ => None,
        }
    }
}

struct ChunkReader {
    chunk_id: u32,
    len: u32,
}

impl ChunkReader {
    #[optimize(size)]
    fn open(chunk_id: u32) -> Result<Self, StorageError> {
        Ok(Self {
            chunk_id,
            len: platform::chunk_size(chunk_id)?,
        })
    }
}

impl ReadAt for ChunkReader {
    type Error = StorageError;

    #[optimize(size)]
    fn len(&self) -> u32 {
        self.len
    }

    #[optimize(size)]
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        platform::read_chunk_exact(self.chunk_id, offset, output)
    }
}

#[optimize(size)]
fn legacy_index_map(map: EpisodeMap) -> Result<PsbIndex, PsbError<StorageError>> {
    let mut reader = ChunkReader::open(map.chunk_id()).map_err(PsbError::Read)?;
    PsbIndex::read(&mut reader)
}

#[optimize(size)]
pub fn index_map(map: EpisodeMap) -> Result<PsbIndex, PsbError<StorageError>> {
    let mut directory = [0; EPISODE_DIRECTORY_BYTES];
    if platform::read_chunk_exact(EPISODE_DIRECTORY_CHUNK, 0, &mut directory).is_ok() {
        return episode_directory_index_or_try(&directory, map.chunk_id(), || {
            legacy_index_map(map)
        });
    }
    legacy_index_map(map)
}

/// Payload reader used after one PSB index has already been validated. Its
/// first payload access opens a single forward-only CD session; the shared
/// resident loader then consumes ModelData through Entities in disc order.
struct ForwardChunkReader<'a> {
    chunk_id: u32,
    len: u32,
    stream: Option<platform::ChunkStream>,
    node_range: LumpRange,
    node_cache: &'a mut Vec<u8>,
}

impl<'a> ForwardChunkReader<'a> {
    #[optimize(size)]
    fn open(
        chunk_id: u32,
        node_range: LumpRange,
        node_cache: &'a mut Vec<u8>,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            chunk_id,
            len: platform::chunk_size(chunk_id)?,
            stream: None,
            node_range,
            node_cache,
        })
    }
}

impl ReadAt for ForwardChunkReader<'_> {
    type Error = StorageError;

    #[optimize(size)]
    fn len(&self) -> u32 {
        self.len
    }

    #[optimize(size)]
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        if output.is_empty() {
            return Ok(());
        }
        if self.stream.is_none() {
            self.stream = Some(platform::ChunkStream::open_at(self.chunk_id, offset)?);
        }
        let output_end = offset
            .checked_add(u32::try_from(output.len()).map_err(|_| StorageError::OutOfBounds)?)
            .ok_or(StorageError::OutOfBounds)?;
        if offset >= self.node_range.offset && output_end <= self.node_range.end() {
            if self.node_cache.is_empty() {
                self.node_cache.resize(self.node_range.len as usize, 0);
                self.stream
                    .as_mut()
                    .ok_or(StorageError::ReadFailed)?
                    .read_exact_at(self.node_range.offset, self.node_cache)?;
                // PSB5 nodes are expanded one record at a time by the shared
                // loader. Pause ReadN while that CPU pass consumes the cache;
                // otherwise the drive advances past the following clip-node
                // sector before the next payload read.
                self.stream = None;
            }
            let start = (offset - self.node_range.offset) as usize;
            let end = start + output.len();
            output.copy_from_slice(
                self.node_cache
                    .get(start..end)
                    .ok_or(StorageError::OutOfBounds)?,
            );
            return Ok(());
        }
        self.stream
            .as_mut()
            .ok_or(StorageError::ReadFailed)?
            .read_exact_at(offset, output)
    }
}

const TEXTURE_UPLOAD_ROWS: usize = 8;
// Read 24 rows (30 KiB) per CD session, then pause the drive before three
// bounded VRAM uploads. The timing model already drops sectors when ReadN is
// left active during GPU work; silicon reads faster, so that pattern is not a
// hardware-safe optimization.
const TEXTURE_READ_ROWS: usize = TEXTURE_UPLOAD_ROWS * 3;
const GRAPHICS_CHUNK: u32 = 1;
const GRAPHICS_CLUT_BYTES: usize = 6 * 256 * 2;
const GRAPHICS_PICTURE_WIDTH: u16 = 64;
const GRAPHICS_PICTURE_ROWS: usize = 512;
const GRAPHICS_PICTURE_ROW_BYTES: usize = GRAPHICS_PICTURE_WIDTH as usize * 2;
const GRAPHICS_PICTURE_BYTES: usize = GRAPHICS_PICTURE_ROW_BYTES * GRAPHICS_PICTURE_ROWS;
const GRAPHICS_STREAM_ROWS: usize = 8;
pub(crate) const STREAM_SCRATCH_BYTES: usize = TEXTURE_ROW_BYTES * TEXTURE_READ_ROWS;
/// Episode 1's measured maximum is 2,948 planes (E1M4). The fixed bound keeps
/// this transition-owned allocation one-time under the PS1 bump allocator.
const COLLISION_PLANE_CAPACITY: usize = 3_000;
/// Fixed decoded table for the compact texture records sampled by every
/// visible world face. Episode 1 stays well below this fail-closed bound.
const RENDER_TEXTURE_CAPACITY: usize = 128;
/// Episode 1 authors at most four 64x64 turbulent textures in one map.
const LIQUID_TEXTURE_CAPACITY: usize = 4;
const LIQUID_TEXTURE_BYTES: usize = quake_core::liquid::LIQUID_TILE_BYTES;
const LIQUID_SOURCE_CAPACITY: usize = LIQUID_TEXTURE_CAPACITY * LIQUID_TEXTURE_BYTES;

/// One immutable source tile retained while its map is resident.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResidentLiquidTexture {
    pub texture_index: u16,
    pub primary: TextureInfo,
    pub alternate: TextureInfo,
    source_offset: u16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MapLoadError {
    Storage(StorageError),
    Format,
    TooLarge,
    BadTextureData,
    BadVertexData,
    BadAliasModels,
    VramUpload,
    BadFace(usize),
    BadMarkSurface(usize),
    BadLeaf(usize),
    BadNode(usize),
    BadClipNode(usize),
    BadBrushModel(usize),
    BadEntity(usize),
    MissingEntities,
}

/// Whether a level request reused the immutable current map or replaced it.
///
/// Gameplay state is intentionally outside this result: a resident hit still
/// rebuilds the player, entities, voices and map-local renderer state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MapResidency {
    Hit { generation: u32 },
    Loaded { generation: u32 },
}

impl MapResidency {
    #[optimize(size)]
    pub const fn is_hit(self) -> bool {
        matches!(self, Self::Hit { .. })
    }

    #[optimize(size)]
    pub const fn generation(self) -> u32 {
        match self {
            Self::Hit { generation } | Self::Loaded { generation } => generation,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EpisodeDirectoryState {
    Unread,
    Ready,
    LegacyOnly,
}

#[derive(Clone)]
struct MapLoadPlan {
    map: EpisodeMap,
    index: PsbIndex,
}

const RESIDENT_LUMPS: [LumpKind; 13] = [
    LumpKind::ModelData,
    LumpKind::Vertices,
    LumpKind::Planes,
    LumpKind::TextureInfo,
    LumpKind::Faces,
    LumpKind::MarkSurfaces,
    LumpKind::Visibility,
    LumpKind::Leaves,
    LumpKind::Nodes,
    LumpKind::ClipNodes,
    LumpKind::Models,
    LumpKind::Strings,
    LumpKind::Entities,
];

/// Quake map identity plus platform-owned streaming around canonical resident
/// storage and cross-lump validation.
pub struct ResidentMap {
    map: EpisodeMap,
    index: Option<PsbIndex>,
    shared: SharedResidentMap,
    collision_planes: Vec<Plane>,
    render_textures: Vec<TextureInfo>,
    liquid_textures: Vec<ResidentLiquidTexture>,
    liquid_source: Vec<u8>,
    world_render_head_node: i16,
    stream_scratch: Vec<u8>,
    graphics: Vec<GraphicsPicture>,
    weapon_icons: Vec<u8>,
    episode_directory: [u8; EPISODE_DIRECTORY_BYTES],
    episode_directory_state: EpisodeDirectoryState,
}

impl ResidentMap {
    #[optimize(size)]
    pub fn new() -> Self {
        Self {
            map: EpisodeMap::Start,
            index: None,
            // Not `SharedResidentMap::new()`: the engine default reserves a
            // generic 1,100,000-byte world arena, and the ~111 KB the cooked
            // Episode 1 corpus never uses is PS1 RAM the entity pools, audio
            // bank, and renderer arenas need. See `RESIDENT_MAP_ARENA_BYTES`.
            shared: SharedResidentMap::with_capacity(RESIDENT_MAP_ARENA_BYTES),
            collision_planes: Vec::with_capacity(COLLISION_PLANE_CAPACITY),
            render_textures: Vec::with_capacity(RENDER_TEXTURE_CAPACITY),
            liquid_textures: Vec::with_capacity(LIQUID_TEXTURE_CAPACITY),
            liquid_source: Vec::with_capacity(LIQUID_SOURCE_CAPACITY),
            world_render_head_node: 0,
            stream_scratch: Vec::with_capacity(STREAM_SCRATCH_BYTES),
            graphics: Vec::with_capacity(64),
            weapon_icons: Vec::with_capacity(GRAPHICS_WEAPON_ICON_BYTES),
            episode_directory: [0; EPISODE_DIRECTORY_BYTES],
            episode_directory_state: EpisodeDirectoryState::Unread,
        }
    }

    #[optimize(size)]
    pub fn load(&mut self, map: EpisodeMap) -> Result<(), MapLoadError> {
        let plan = self.prepare_load(map)?;
        self.commit_load(plan)
    }

    /// Ensure one immutable map generation is resident. An exact hit performs
    /// no directory, PSB or VRAM I/O; callers must still reset gameplay state.
    #[optimize(size)]
    pub fn ensure_resident(&mut self, map: EpisodeMap) -> Result<MapResidency, MapLoadError> {
        if self.is_resident(map) {
            return Ok(MapResidency::Hit {
                generation: self.shared.generation(),
            });
        }
        self.load(map)?;
        Ok(MapResidency::Loaded {
            generation: self.shared.generation(),
        })
    }

    #[optimize(size)]
    pub fn is_resident(&self, map: EpisodeMap) -> bool {
        self.map == map && self.index.is_some() && self.shared.map_id() == Some(map.chunk_id())
    }

    #[optimize(size)]
    fn prepare_load(&mut self, map: EpisodeMap) -> Result<MapLoadPlan, MapLoadError> {
        let index = self.index_for(map)?;
        let chunk_len = platform::chunk_size(map.chunk_id()).map_err(MapLoadError::Storage)?;
        if chunk_len != index.file_len() {
            return Err(MapLoadError::Format);
        }
        validate_texture_lump(&index)?;
        if resident_bytes_required(&index).ok_or(MapLoadError::TooLarge)? > RESIDENT_MAP_ARENA_BYTES
        {
            return Err(MapLoadError::TooLarge);
        }
        Ok(MapLoadPlan { map, index })
    }

    #[optimize(size)]
    fn commit_load(&mut self, plan: MapLoadPlan) -> Result<(), MapLoadError> {
        // One 880 KiB CPU arena cannot retain both maps. Invalidate the public
        // identity before the shared loader destructively reuses that arena,
        // so a short read or cross-reference failure can never masquerade as
        // the old map still being resident. The preflight above removes every
        // deterministic format/capacity failure; any later CD/validation/VRAM
        // failure is explicitly fail-stop and requires a fresh disc reload.
        self.index = None;
        self.stream_scratch.clear();
        let shared_result = {
            let payload = ForwardChunkReader::open(
                plan.map.chunk_id(),
                plan.index.lump(LumpKind::Nodes),
                &mut self.stream_scratch,
            )
            .map_err(MapLoadError::Storage)?;
            let mut reader = CachedIndexReader::new(&plan.index, payload);
            self.shared.load(plan.map.chunk_id(), &mut reader)
        };
        self.stream_scratch.clear();
        shared_result.map_err(map_shared_error)?;
        self.refresh_collision_working_set()?;
        self.upload_textures(plan.map, &plan.index)?;
        self.index = Some(plan.index);
        self.map = plan.map;
        Ok(())
    }

    #[optimize(size)]
    fn refresh_collision_working_set(&mut self) -> Result<(), MapLoadError> {
        let packed = self.shared.planes();
        if packed.len() > COLLISION_PLANE_CAPACITY {
            return Err(MapLoadError::TooLarge);
        }
        let textures = self.shared.textures();
        if textures.len() > RENDER_TEXTURE_CAPACITY {
            return Err(MapLoadError::TooLarge);
        }
        if self.shared.clip_nodes().as_native_clip_nodes().is_none() {
            return Err(MapLoadError::Format);
        }
        if self.shared.nodes().as_native_compact_nodes().is_none() {
            return Err(MapLoadError::Format);
        }
        let world = self
            .shared
            .brush_models()
            .get(0)
            .ok_or(MapLoadError::Format)?;
        self.collision_planes.clear();
        self.collision_planes.extend(packed.iter());
        self.render_textures.clear();
        self.render_textures.extend(textures.iter());
        self.world_render_head_node = world.head_nodes[0];
        Ok(())
    }

    #[optimize(size)]
    fn index_for(&mut self, map: EpisodeMap) -> Result<PsbIndex, MapLoadError> {
        if self.episode_directory_state == EpisodeDirectoryState::Unread {
            self.episode_directory_state = if platform::read_chunk_exact(
                EPISODE_DIRECTORY_CHUNK,
                0,
                &mut self.episode_directory,
            )
            .is_ok()
                && episode_directory_index(&self.episode_directory, map.chunk_id())
                    .ok()
                    .flatten()
                    .is_some()
            {
                EpisodeDirectoryState::Ready
            } else {
                EpisodeDirectoryState::LegacyOnly
            };
        }

        if self.episode_directory_state == EpisodeDirectoryState::Ready {
            if let Some(index) = episode_directory_index(&self.episode_directory, map.chunk_id())
                .map_err(|_| MapLoadError::Format)?
            {
                return Ok(index);
            }
        }

        legacy_index_map(map).map_err(|_| MapLoadError::Format)
    }

    #[optimize(size)]
    pub fn load_graphics(&mut self) -> Result<(), MapLoadError> {
        let file_len = platform::chunk_size(GRAPHICS_CHUNK).map_err(MapLoadError::Storage)?;
        if file_len < (GRAPHICS_CLUT_BYTES + 2 + GRAPHICS_PICTURE_BYTES) as u32 {
            return Err(MapLoadError::Format);
        }

        self.stream_scratch.clear();
        self.stream_scratch.resize(GRAPHICS_CLUT_BYTES, 0);
        platform::read_chunk_exact(GRAPHICS_CHUNK, 0, &mut self.stream_scratch)
            .map_err(MapLoadError::Storage)?;
        platform::upload_vram(VramRect::new(0, 240, 256, 6), &self.stream_scratch)
            .map_err(|_| MapLoadError::VramUpload)?;

        // Textured PS1 semitransparency is selected per palette entry. Mirror
        // the six gamma rows into the unused framebuffer gap with STP set on
        // every visible colour; index 255 remains the ordinary transparent
        // texel. A distinct CLUT word also satisfies the silicon-confirmed
        // per-line CLUT cache when opaque and liquid packets interleave.
        for (index, color) in self.stream_scratch.chunks_exact_mut(2).enumerate() {
            if index % 256 != 255 {
                let translucent = u16::from_le_bytes([color[0], color[1]]) | 0x8000;
                color.copy_from_slice(&translucent.to_le_bytes());
            }
        }
        platform::upload_vram(VramRect::new(0, 246, 256, 6), &self.stream_scratch)
            .map_err(|_| MapLoadError::VramUpload)?;

        let mut count_bytes = [0u8; 2];
        platform::read_chunk_exact(GRAPHICS_CHUNK, GRAPHICS_CLUT_BYTES as u32, &mut count_bytes)
            .map_err(MapLoadError::Storage)?;
        let picture_count = u16::from_le_bytes(count_bytes) as usize;
        let picture_offset = GRAPHICS_CLUT_BYTES
            + 2
            + picture_count
                .checked_mul(GRAPHICS_PICTURE_RECORD_BYTES)
                .ok_or(MapLoadError::Format)?;
        if picture_count == 0
            || picture_count > 256
            || picture_count <= GraphicsPictureId::StatusBar0.index() + 28
            || picture_offset + GRAPHICS_PICTURE_BYTES + GRAPHICS_WEAPON_ICON_BYTES
                != file_len as usize
        {
            return Err(MapLoadError::Format);
        }

        let record_bytes = picture_count
            .checked_mul(GRAPHICS_PICTURE_RECORD_BYTES)
            .ok_or(MapLoadError::Format)?;
        self.stream_scratch.clear();
        self.stream_scratch.resize(record_bytes, 0);
        platform::read_chunk_exact(
            GRAPHICS_CHUNK,
            (GRAPHICS_CLUT_BYTES + 2) as u32,
            &mut self.stream_scratch,
        )
        .map_err(MapLoadError::Storage)?;
        self.graphics.clear();
        for (id, record) in self
            .stream_scratch
            .chunks_exact(GRAPHICS_PICTURE_RECORD_BYTES)
            .enumerate()
        {
            let picture = GraphicsPicture::decode(record).ok_or(MapLoadError::Format)?;
            if (id == 0 && picture != GraphicsPicture::default())
                || (id != 0 && !picture.is_valid_real_picture())
            {
                self.graphics.clear();
                return Err(MapLoadError::Format);
            }
            self.graphics.push(picture);
        }

        let mut row = 0usize;
        while row < GRAPHICS_PICTURE_ROWS {
            let batch_rows = (GRAPHICS_PICTURE_ROWS - row).min(GRAPHICS_STREAM_ROWS);
            let byte_count = batch_rows * GRAPHICS_PICTURE_ROW_BYTES;
            self.stream_scratch.clear();
            self.stream_scratch.resize(byte_count, 0);
            platform::read_chunk_exact(
                GRAPHICS_CHUNK,
                (picture_offset + row * GRAPHICS_PICTURE_ROW_BYTES) as u32,
                &mut self.stream_scratch,
            )
            .map_err(MapLoadError::Storage)?;
            platform::upload_vram(
                VramRect::new(960, row as u16, GRAPHICS_PICTURE_WIDTH, batch_rows as u16),
                &self.stream_scratch,
            )
            .map_err(|_| MapLoadError::VramUpload)?;
            row += batch_rows;
        }
        self.weapon_icons.clear();
        self.weapon_icons.resize(GRAPHICS_WEAPON_ICON_BYTES, 0);
        platform::read_chunk_exact(
            GRAPHICS_CHUNK,
            (picture_offset + GRAPHICS_PICTURE_BYTES) as u32,
            &mut self.weapon_icons,
        )
        .map_err(MapLoadError::Storage)?;
        self.stream_scratch.clear();
        Ok(())
    }

    /// Copy one retained picture descriptor. The returned value never borrows
    /// `stream_scratch`, which map and sound loads reuse after graphics boot.
    #[optimize(size)]
    pub fn picture(&self, id: GraphicsPictureId) -> Option<GraphicsPicture> {
        self.picture_at(id.index())
    }

    #[optimize(size)]
    pub fn picture_at(&self, id: usize) -> Option<GraphicsPicture> {
        self.graphics
            .get(id)
            .copied()
            .filter(|picture| picture.is_valid_real_picture())
    }

    /// Exact inactive then selected strip pixels appended after the packed
    /// graphics band. The allocation is immutable after boot, so deferred GPU
    /// uploads may safely retain pointers into it until frame submission.
    #[optimize(size)]
    pub(crate) fn weapon_icon_pixels(&self) -> &[u8] {
        &self.weapon_icons
    }

    #[optimize(size)]
    pub const fn map(&self) -> EpisodeMap {
        self.map
    }

    /// Identity of the current immutable resident byte layout.
    #[optimize(size)]
    pub const fn generation(&self) -> u32 {
        self.shared.generation()
    }

    #[optimize(size)]
    pub fn source_lump(&self, kind: LumpKind) -> Option<LumpRange> {
        self.index.as_ref().map(|index| index.lump(kind))
    }

    #[optimize(size)]
    pub fn vertices(&self) -> RecordSlice<'_, Vertex> {
        self.shared.vertices()
    }

    #[optimize(size)]
    pub fn vertex_data(&self) -> &[u8] {
        self.shared.vertex_data()
    }

    #[optimize(size)]
    pub fn indexed_vertices(&self) -> Option<IndexedVertices<'_>> {
        self.shared.indexed_vertices()
    }

    #[optimize(size)]
    pub fn planes(&self) -> RecordSlice<'_, Plane> {
        self.shared.planes()
    }

    /// Hot collision planes decoded once when the immutable map generation is
    /// committed. Same-map residency hits retain this working set unchanged.
    #[optimize(size)]
    pub fn collision_planes(&self) -> &[Plane] {
        &self.collision_planes
    }

    #[optimize(size)]
    pub fn textures(&self) -> RecordSlice<'_, TextureInfo> {
        self.shared.textures()
    }

    /// Compact texture descriptors decoded once for the active map generation.
    #[optimize(size)]
    pub fn render_textures(&self) -> &[TextureInfo] {
        &self.render_textures
    }

    /// Immutable original liquid tiles retained from the atlas upload.
    #[optimize(size)]
    pub(crate) fn liquid_textures(&self) -> &[ResidentLiquidTexture] {
        &self.liquid_textures
    }

    #[optimize(size)]
    pub(crate) fn liquid_source(&self, liquid: ResidentLiquidTexture) -> Option<&[u8]> {
        let start = usize::from(liquid.source_offset);
        self.liquid_source
            .get(start..start + quake_core::liquid::LIQUID_TILE_BYTES)
    }

    #[optimize(size)]
    pub fn faces(&self) -> RecordSlice<'_, Face> {
        self.shared.faces()
    }

    #[optimize(size)]
    pub fn leaves(&self) -> RecordSlice<'_, Leaf> {
        self.shared.leaves()
    }

    #[optimize(size)]
    pub fn nodes(&self) -> RecordSlice<'_, Node> {
        self.shared.nodes()
    }

    #[optimize(size)]
    pub fn render_nodes(&self) -> &[CompactNode] {
        self.shared
            .nodes()
            .as_native_compact_nodes()
            .expect("resident-map load validated render-node alignment")
    }

    #[optimize(size)]
    pub fn clip_nodes(&self) -> RecordSlice<'_, ClipNode> {
        self.shared.clip_nodes()
    }

    /// Clip-node wire records are already the native little-endian layout, so
    /// collision traversals borrow them directly without a duplicate arena.
    #[optimize(size)]
    pub fn collision_clip_nodes(&self) -> &[ClipNode] {
        self.shared
            .clip_nodes()
            .as_native_clip_nodes()
            .expect("resident-map load validated clip-node alignment")
    }

    #[optimize(size)]
    pub fn brush_models(&self) -> RecordSlice<'_, BrushModel> {
        self.shared.brush_models()
    }

    #[optimize(size)]
    pub fn entities(&self) -> RecordSlice<'_, MapEntity> {
        self.shared.entities()
    }

    #[optimize(size)]
    pub fn mark_surfaces(&self) -> RecordSlice<'_, u16> {
        self.shared.mark_surfaces()
    }

    #[optimize(size)]
    pub fn visibility(&self) -> &[u8] {
        self.shared.visibility()
    }

    #[optimize(size)]
    pub(crate) fn leaf_bounds(&self, leaf_index: usize) -> Option<LeafBounds> {
        leaf_bounds_at(self.shared.visibility(), leaf_index)
    }

    #[cfg(feature = "renderer-portal-areas")]
    #[optimize(size)]
    pub(crate) fn leaf_portal_graph(&self) -> Option<LeafPortalGraph<'_>> {
        leaf_portal_graph(self.shared.visibility())
    }

    #[optimize(size)]
    pub fn model_data(&self) -> &[u8] {
        self.shared.model_data()
    }

    #[optimize(size)]
    pub fn alias_models(&self) -> AliasModelTable<'_> {
        self.shared.alias_models()
    }

    #[optimize(size)]
    pub fn strings(&self) -> &[u8] {
        self.shared.strings()
    }

    #[optimize(size)]
    pub fn string_at(&self, offset: u16) -> Option<&[u8]> {
        self.shared.string_at(offset)
    }

    #[optimize(size)]
    pub fn point_leaf_index(&self, point: Vec3I32) -> Option<usize> {
        let nodes = self.render_nodes();
        let planes = self.collision_planes();
        let mut node_index = self.world_render_head_node;
        let mut budget = nodes.len();
        loop {
            if node_index < 0 {
                return Some((-1i32 - node_index as i32) as usize);
            }
            if budget == 0 {
                return None;
            }
            budget -= 1;
            let node = unsafe { nodes.get_unchecked(node_index as usize) };
            let plane = unsafe { planes.get_unchecked(node.plane as usize) };
            let dot = match plane.kind {
                0 => point.x,
                1 => point.y,
                2 => point.z,
                _ => mul_q12_i32_wide(point.x, plane.normal.x as i32)
                    .wrapping_add(mul_q12_i32_wide(point.y, plane.normal.y as i32))
                    .wrapping_add(mul_q12_i32_wide(point.z, plane.normal.z as i32)),
            };
            node_index = node.children[(dot.wrapping_sub(plane.distance) <= 0) as usize];
        }
    }

    /// Temporarily lend the map loader's retained transfer buffer to another
    /// load-time subsystem. The world keeps no references into this buffer,
    /// and callers must restore it before the next map or graphics load.
    #[optimize(size)]
    pub(crate) fn take_stream_scratch(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.stream_scratch)
    }

    #[optimize(size)]
    pub(crate) fn restore_stream_scratch(&mut self, mut scratch: Vec<u8>) {
        scratch.clear();
        self.stream_scratch = scratch;
    }

    #[optimize(size)]
    fn upload_textures(&mut self, map: EpisodeMap, index: &PsbIndex) -> Result<(), MapLoadError> {
        let texture = validate_texture_lump(index)?;
        let rows = texture.len as usize / TEXTURE_ROW_BYTES;
        self.prepare_liquid_sources()?;

        self.stream_scratch.clear();
        self.stream_scratch.resize(STREAM_SCRATCH_BYTES, 0);
        let mut row = 0usize;
        while row < rows {
            let read_rows = (rows - row).min(TEXTURE_READ_ROWS);
            let read_bytes = read_rows * TEXTURE_ROW_BYTES;
            {
                let offset = texture.offset + (row * TEXTURE_ROW_BYTES) as u32;
                let mut stream = platform::ChunkStream::open_at(map.chunk_id(), offset)
                    .map_err(MapLoadError::Storage)?;
                stream
                    .read_exact_at(offset, &mut self.stream_scratch[..read_bytes])
                    .map_err(MapLoadError::Storage)?;
            }
            self.capture_liquid_rows(row, read_rows)?;

            let mut uploaded_rows = 0usize;
            while uploaded_rows < read_rows {
                let batch_rows = (read_rows - uploaded_rows).min(TEXTURE_UPLOAD_ROWS);
                let start = uploaded_rows * TEXTURE_ROW_BYTES;
                let end = start + batch_rows * TEXTURE_ROW_BYTES;
                platform::upload_vram(
                    VramRect::new(
                        TEXTURE_VRAM_X,
                        (row + uploaded_rows) as u16,
                        TEXTURE_VRAM_WIDTH,
                        batch_rows as u16,
                    ),
                    &self.stream_scratch[start..end],
                )
                .map_err(|_| MapLoadError::VramUpload)?;
                uploaded_rows += batch_rows;
            }
            row += read_rows;
        }
        self.stream_scratch.clear();
        Ok(())
    }

    #[optimize(size)]
    fn prepare_liquid_sources(&mut self) -> Result<(), MapLoadError> {
        self.liquid_textures.clear();
        self.liquid_source.clear();
        for (texture_index, texture) in self.render_textures.iter().copied().enumerate() {
            if texture.flags & TEXTURE_LIQUID == 0 {
                continue;
            }
            if self.liquid_textures.len() == LIQUID_TEXTURE_CAPACITY
                || texture.size.x != (quake_core::liquid::LIQUID_TILE_SIDE / 2) as i16
                || texture.size.y != quake_core::liquid::LIQUID_TILE_SIDE as i16
                || texture.atlas.x & 1 != 0
            {
                return Err(MapLoadError::BadTextureData);
            }
            let alternate = quake_formats::liquid_alternate_texture(texture)
                .ok_or(MapLoadError::BadTextureData)?;
            let primary_rect = texture_rect(texture).ok_or(MapLoadError::BadTextureData)?;
            let alternate_rect = texture_rect(alternate).ok_or(MapLoadError::BadTextureData)?;
            if primary_rect == alternate_rect {
                return Err(MapLoadError::BadTextureData);
            }
            let source_offset = self.liquid_source.len();
            self.liquid_source
                .resize(source_offset + LIQUID_TEXTURE_BYTES, 0);
            self.liquid_textures.push(ResidentLiquidTexture {
                texture_index: texture_index as u16,
                primary: texture,
                alternate,
                source_offset: source_offset as u16,
            });
        }
        Ok(())
    }

    #[optimize(size)]
    fn capture_liquid_rows(
        &mut self,
        first_atlas_row: usize,
        atlas_row_count: usize,
    ) -> Result<(), MapLoadError> {
        let last_atlas_row = first_atlas_row + atlas_row_count;
        for liquid in &self.liquid_textures {
            let Some(rect) = texture_rect(liquid.primary) else {
                return Err(MapLoadError::BadTextureData);
            };
            let first = usize::from(rect.y).max(first_atlas_row);
            let last = (usize::from(rect.y) + usize::from(rect.h)).min(last_atlas_row);
            if first >= last {
                continue;
            }
            let atlas_x_bytes = usize::from(rect.x - TEXTURE_VRAM_X) * 2;
            let row_bytes = usize::from(rect.w) * 2;
            for atlas_y in first..last {
                let source = (atlas_y - first_atlas_row) * TEXTURE_ROW_BYTES + atlas_x_bytes;
                let destination =
                    usize::from(liquid.source_offset) + (atlas_y - usize::from(rect.y)) * row_bytes;
                let source_end = source + row_bytes;
                let destination_end = destination + row_bytes;
                let Some(source_row) = self.stream_scratch.get(source..source_end) else {
                    return Err(MapLoadError::BadTextureData);
                };
                let Some(destination_row) =
                    self.liquid_source.get_mut(destination..destination_end)
                else {
                    return Err(MapLoadError::BadTextureData);
                };
                destination_row.copy_from_slice(source_row);
            }
        }
        Ok(())
    }
}

#[optimize(size)]
pub(crate) fn texture_rect(texture: TextureInfo) -> Option<VramRect> {
    if texture.size.x <= 0 || texture.size.y <= 0 || texture.atlas.x & 1 != 0 {
        return None;
    }
    let tpage_x = u16::from(texture.texture_page & 0x000f) * 64;
    let tpage_y = u16::from((texture.texture_page >> 4) & 1) * 256;
    let x = tpage_x.checked_add(u16::from(texture.atlas.x) / 2)?;
    let y = tpage_y.checked_add(u16::from(texture.atlas.y))?;
    let width = texture.size.x as u16;
    let height = texture.size.y as u16;
    if x < TEXTURE_VRAM_X
        || x.checked_add(width)? > TEXTURE_VRAM_X + TEXTURE_VRAM_WIDTH
        || y.checked_add(height)? > TEXTURE_VRAM_MAX_ROWS
    {
        return None;
    }
    Some(VramRect::new(x, y, width, height))
}

#[optimize(size)]
fn validate_texture_lump(index: &PsbIndex) -> Result<LumpRange, MapLoadError> {
    let texture = index.lump(LumpKind::TextureData);
    if texture.len == 0 || texture.len as usize % TEXTURE_ROW_BYTES != 0 {
        return Err(MapLoadError::BadTextureData);
    }
    let rows = texture.len as usize / TEXTURE_ROW_BYTES;
    if rows > TEXTURE_VRAM_MAX_ROWS as usize {
        return Err(MapLoadError::BadTextureData);
    }
    Ok(texture)
}

#[optimize(size)]
fn resident_bytes_required(index: &PsbIndex) -> Option<usize> {
    let mut total = 0usize;
    for kind in RESIDENT_LUMPS {
        total = total.checked_add(3)? & !3;
        let source_len = index.lump(kind).len as usize;
        let resident_len = if index.version() == PsbVersion::LegacyV1 {
            match (
                kind.record_size(PsbVersion::LegacyV1),
                kind.record_size(PsbVersion::IndexedV5),
            ) {
                (Some(legacy), Some(compact)) if legacy != compact => source_len
                    .checked_div(legacy as usize)?
                    .checked_mul(compact as usize)?,
                _ => source_len,
            }
        } else {
            source_len
        };
        total = total.checked_add(resident_len)?;
    }
    Some(total)
}

#[optimize(size)]
fn map_shared_error(error: SharedMapLoadError<StorageError>) -> MapLoadError {
    match error {
        SharedMapLoadError::Index(_) => MapLoadError::Format,
        SharedMapLoadError::LegacyRecord { .. } => MapLoadError::Format,
        SharedMapLoadError::Read(error) => MapLoadError::Storage(error),
        SharedMapLoadError::TooLarge { .. } => MapLoadError::TooLarge,
        SharedMapLoadError::BadTextureData => MapLoadError::BadTextureData,
        SharedMapLoadError::BadVertexData => MapLoadError::BadVertexData,
        SharedMapLoadError::BadAliasModels => MapLoadError::BadAliasModels,
        SharedMapLoadError::BadFace(index) => MapLoadError::BadFace(index),
        SharedMapLoadError::BadMarkSurface(index) => MapLoadError::BadMarkSurface(index),
        SharedMapLoadError::BadLeaf(index) => MapLoadError::BadLeaf(index),
        SharedMapLoadError::BadNode(index) => MapLoadError::BadNode(index),
        SharedMapLoadError::BadClipNode(index) => MapLoadError::BadClipNode(index),
        SharedMapLoadError::BadBrushModel(index) => MapLoadError::BadBrushModel(index),
        SharedMapLoadError::BadEntity(index) => MapLoadError::BadEntity(index),
        SharedMapLoadError::MissingEntities => MapLoadError::MissingEntities,
    }
}
