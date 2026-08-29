#![feature(optimize_attribute)]
#![no_std]

//! Quake policy names over PSoXide's canonical XBSP wire-format crate.
//!
//! PSoXide owns the checked readers and record definitions. This crate stays
//! as a compatibility facade for Quake-specific host and guest code while the
//! remaining game-policy modules migrate independently.

#[cfg(test)]
extern crate std;

mod sound;
mod render_quad_payload;
mod render_sections;

pub use render_quad_payload::*;
pub use render_sections::*;
pub use psx_bsp::*;
pub use sound::*;

/// Footer magic for Quake's optional source-leaf AABB table appended to the
/// otherwise unchanged compressed visibility lump (`QLB1`). Existing PVS
/// offsets continue to address the original prefix.
pub const LEAF_BOUNDS_TRAILER_MAGIC: u32 = u32::from_le_bytes(*b"QLB1");
pub const LEAF_BOUNDS_RECORD_BYTES: usize = 6;
pub const LEAF_BOUNDS_FOOTER_BYTES: usize = 8;
/// World units represented by one signed leaf-bound code.
pub const LEAF_BOUNDS_GRID: i16 = 32;
const LEAF_BOUNDS_GRID_SHIFT: u32 = LEAF_BOUNDS_GRID.trailing_zeros();

pub const fn encode_leaf_bound_min(value: i16) -> i8 {
    let units = (value as i32) >> LEAF_BOUNDS_GRID_SHIFT;
    if units <= i8::MIN as i32 {
        i8::MIN
    } else {
        units as i8
    }
}

pub const fn encode_leaf_bound_max(value: i16) -> i8 {
    let units = ((value as i32) + (LEAF_BOUNDS_GRID as i32 - 1)) >> LEAF_BOUNDS_GRID_SHIFT;
    if units >= i8::MAX as i32 {
        i8::MAX
    } else {
        units as i8
    }
}

pub const fn decode_leaf_bound_min(code: i8) -> i16 {
    if code == i8::MIN {
        i16::MIN
    } else {
        (code as i16) << LEAF_BOUNDS_GRID_SHIFT
    }
}

pub const fn decode_leaf_bound_max(code: i8) -> i16 {
    if code == i8::MAX {
        i16::MAX
    } else {
        (code as i16) << LEAF_BOUNDS_GRID_SHIFT
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LeafBounds {
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
}

/// Read one optional Quake leaf-bounds record from a visibility-lump suffix.
/// Legacy maps and malformed trailers return `None` without affecting PVS
/// decompression.
pub fn leaf_bounds_at(visibility: &[u8], leaf_index: usize) -> Option<LeafBounds> {
    let footer = visibility.get(visibility.len().checked_sub(LEAF_BOUNDS_FOOTER_BYTES)?..)?;
    if u32::from_le_bytes(footer[0..4].try_into().ok()?) != LEAF_BOUNDS_TRAILER_MAGIC
        || u16::from_le_bytes(footer[6..8].try_into().ok()?) as usize != LEAF_BOUNDS_RECORD_BYTES
    {
        return None;
    }
    let count = u16::from_le_bytes(footer[4..6].try_into().ok()?) as usize;
    if leaf_index >= count {
        return None;
    }
    let table_bytes = count.checked_mul(LEAF_BOUNDS_RECORD_BYTES)?;
    let table_start = visibility
        .len()
        .checked_sub(LEAF_BOUNDS_FOOTER_BYTES + table_bytes)?;
    let start = table_start.checked_add(leaf_index.checked_mul(LEAF_BOUNDS_RECORD_BYTES)?)?;
    let record = visibility.get(start..start + LEAF_BOUNDS_RECORD_BYTES)?;
    Some(LeafBounds {
        mins: [
            decode_leaf_bound_min(record[0] as i8),
            decode_leaf_bound_min(record[1] as i8),
            decode_leaf_bound_min(record[2] as i8),
        ],
        maxs: [
            decode_leaf_bound_max(record[3] as i8),
            decode_leaf_bound_max(record[4] as i8),
            decode_leaf_bound_max(record[5] as i8),
        ],
    })
}

#[cfg(test)]
mod leaf_bounds_tests {
    use super::*;

    #[test]
    fn optional_visibility_suffix_is_bounded_and_legacy_safe() {
        let mut bytes = std::vec![0xaa, 0xbb, 0xcc];
        bytes.extend_from_slice(&[-1i8 as u8, -1i8 as u8, -1i8 as u8, 1, 1, 1]);
        bytes.extend_from_slice(&LEAF_BOUNDS_TRAILER_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&(LEAF_BOUNDS_RECORD_BYTES as u16).to_le_bytes());
        assert_eq!(
            leaf_bounds_at(&bytes, 0),
            Some(LeafBounds {
                mins: [-32, -32, -32],
                maxs: [32, 32, 32],
            })
        );
        assert_eq!(leaf_bounds_at(&bytes, 1), None);
        assert_eq!(leaf_bounds_at(&bytes[..3], 0), None);
    }

    #[test]
    fn leaf_bound_grid_rounds_outward_and_saturates_conservatively() {
        assert_eq!(decode_leaf_bound_min(encode_leaf_bound_min(-33)), -64);
        assert_eq!(decode_leaf_bound_max(encode_leaf_bound_max(33)), 64);
        assert_eq!(
            decode_leaf_bound_min(encode_leaf_bound_min(i16::MIN)),
            i16::MIN
        );
        assert_eq!(
            decode_leaf_bound_max(encode_leaf_bound_max(i16::MAX)),
            i16::MAX
        );
    }
}

/// Quake-only `TextureInfo` marker for a liquid tile with a second VRAM home.
///
/// Liquid names never participate in Quake's `+0` texture animation chains,
/// so their five animation bytes are available to carry the alternate atlas
/// coordinates without widening the shared PSB4 texture record.
pub const LIQUID_DOUBLE_BUFFER_MARKER: i8 = -2;

/// Cooked alias-style model flag for a Quake `.spr` sheet. The shared model
/// table still validates its bounded frames and texture binding; the Quake
/// renderer interprets each twelve-byte frame as sprite metadata instead of
/// four compact mesh vertices.
pub const ALIAS_MODEL_SPRITE: u8 = 0x80;

#[optimize(size)]
pub const fn alias_model_is_sprite(header: AliasModelHeader) -> bool {
    // Quake's ordinary alias effect flags also use 0x80 (`EF_TRACER3`). The
    // sprite cooker owns this complete otherwise-impossible mesh signature;
    // checking it prevents a tracer model from being decoded as frame metadata.
    header.flags & ALIAS_MODEL_SPRITE != 0
        && header.flags & 0x07 <= 4
        && header.vertex_count == 4
        && header.triangle_count == 2
        && header.scale.x == 0
        && header.scale.y == 0
        && header.scale.z == 0
}

#[cfg(test)]
mod sprite_marker_tests {
    use super::*;

    #[test]
    fn tracer3_alias_flag_alone_is_not_a_sprite() {
        let alias = AliasModelHeader {
            flags: 0x80,
            vertex_count: 4,
            triangle_count: 2,
            scale: Vec3I16 { x: 1, y: 1, z: 1 },
            ..AliasModelHeader::default()
        };
        assert!(!alias_model_is_sprite(alias));
    }

    #[test]
    fn cooked_sprite_signature_keeps_its_original_orientation_kind() {
        for kind in 0..=4 {
            let sprite = AliasModelHeader {
                flags: ALIAS_MODEL_SPRITE | kind,
                vertex_count: 4,
                triangle_count: 2,
                scale: Vec3I16::default(),
                ..AliasModelHeader::default()
            };
            assert!(alias_model_is_sprite(sprite));
        }
    }
}

/// Reconstruct the inactive copy of a double-buffered liquid texture.
#[optimize(size)]
pub fn liquid_alternate_texture(texture: TextureInfo) -> Option<TextureInfo> {
    if texture.flags & TEXTURE_LIQUID == 0 || texture.animation_total != LIQUID_DOUBLE_BUFFER_MARKER
    {
        return None;
    }
    Some(TextureInfo {
        atlas: Vec2U8 {
            x: texture.animation_min as u8,
            y: texture.animation_max as u8,
        },
        texture_page: u16::from(texture.animation_next as u8)
            | (u16::from(texture.animation_alt as u8) << 8),
        animation_total: 0,
        animation_min: 0,
        animation_max: 0,
        animation_next: -1,
        animation_alt: -1,
        ..texture
    })
}

#[cfg(test)]
mod liquid_double_buffer_tests {
    use super::*;

    #[optimize(size)]
    fn marked_liquid() -> TextureInfo {
        TextureInfo {
            atlas: Vec2U8 { x: 64, y: 32 },
            size: Vec2I16 { x: 32, y: 64 },
            texture_page: 0x0086,
            flags: TEXTURE_LIQUID,
            animation_total: LIQUID_DOUBLE_BUFFER_MARKER,
            // Alternate atlas (192, 200) on tpage 0x0195: both coordinates
            // and the high tpage byte exercise the negative i8 range.
            animation_min: 192u8 as i8,
            animation_max: 200u8 as i8,
            animation_next: 0x95u8 as i8,
            animation_alt: 0x01,
        }
    }

    #[optimize(size)]
    #[test]
    fn alternate_decodes_atlas_page_and_neutralised_animation_fields() {
        let primary = marked_liquid();
        let alternate = liquid_alternate_texture(primary).expect("marked liquid");
        assert_eq!(alternate.atlas, Vec2U8 { x: 192, y: 200 });
        assert_eq!(alternate.texture_page, 0x0195);
        assert_eq!(alternate.size, primary.size);
        assert_eq!(alternate.flags, primary.flags);
        assert_eq!(alternate.animation_total, 0);
        assert_eq!(alternate.animation_min, 0);
        assert_eq!(alternate.animation_max, 0);
        assert_eq!(alternate.animation_next, -1);
        assert_eq!(alternate.animation_alt, -1);
    }

    #[optimize(size)]
    #[test]
    fn non_liquid_records_are_rejected_even_with_the_marker() {
        let mut wall = marked_liquid();
        wall.flags = 0;
        assert_eq!(liquid_alternate_texture(wall), None);
        let mut sky = marked_liquid();
        sky.flags = TEXTURE_SKY;
        assert_eq!(liquid_alternate_texture(sky), None);
    }

    #[optimize(size)]
    #[test]
    fn liquids_without_the_marker_are_rejected() {
        for total in [-1i8, 0, 1, 10] {
            let mut legacy = marked_liquid();
            legacy.animation_total = total;
            assert_eq!(liquid_alternate_texture(legacy), None, "total {total}");
        }
    }

    #[optimize(size)]
    #[test]
    fn representative_coordinates_round_trip_through_the_wire_record() {
        // Encode exactly as the cooker's serialize_textures writes the
        // 14-byte PSB4 record, then decode through the canonical reader.
        let source = marked_liquid();
        let record = [
            source.atlas.x,
            source.atlas.y,
            source.size.x.to_le_bytes()[0],
            source.size.x.to_le_bytes()[1],
            source.size.y.to_le_bytes()[0],
            source.size.y.to_le_bytes()[1],
            source.texture_page.to_le_bytes()[0],
            source.texture_page.to_le_bytes()[1],
            source.flags,
            source.animation_total as u8,
            source.animation_min as u8,
            source.animation_max as u8,
            source.animation_next as u8,
            source.animation_alt as u8,
        ];
        let decoded = <TextureInfo as CookedRecord>::decode(&record);
        assert_eq!(decoded, source);
        let alternate = liquid_alternate_texture(decoded).expect("marked liquid");
        assert_eq!(alternate.atlas, Vec2U8 { x: 192, y: 200 });
        assert_eq!(alternate.texture_page, 0x0195);
        // The alternate copy must land somewhere else in VRAM.
        assert_ne!(
            (alternate.atlas, alternate.texture_page),
            (source.atlas, source.texture_page)
        );
    }
}

pub const EPISODE_DIRECTORY_MAGIC: u32 = 0x5844_4951; // `QIDX`
pub const EPISODE_DIRECTORY_VERSION: u16 = 2;
pub const EPISODE_DIRECTORY_LEGACY_VERSION: u16 = 1;
pub const EPISODE_DIRECTORY_MAPS: usize = 9;
const EPISODE_DIRECTORY_HEADER_BYTES: usize = 12;
const EPISODE_DIRECTORY_LEGACY_RECORD_BYTES: usize = 8 + LUMP_COUNT * 4;
const EPISODE_DIRECTORY_RECORD_BYTES: usize = 12 + LUMP_COUNT * 4;
pub const EPISODE_DIRECTORY_LEGACY_BYTES: usize = EPISODE_DIRECTORY_HEADER_BYTES
    + EPISODE_DIRECTORY_MAPS * EPISODE_DIRECTORY_LEGACY_RECORD_BYTES
    + 4;
pub const EPISODE_DIRECTORY_BYTES: usize =
    EPISODE_DIRECTORY_HEADER_BYTES + EPISODE_DIRECTORY_MAPS * EPISODE_DIRECTORY_RECORD_BYTES + 4;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EpisodeDirectoryError {
    WrongSize,
    BadMagic,
    BadVersion,
    BadMapCount,
    BadChecksum,
    BadIndex,
}

/// Prefer a valid optional directory index, otherwise evaluate the PSB's own
/// interleaved headers supplied by the caller. This keeps QIDX1/PSB1 discs and
/// independently-produced PSB3 maps loadable without conflating the schemas.
#[optimize(size)]
pub fn episode_directory_index_or_try<E, F>(
    bytes: &[u8],
    chunk_id: u32,
    legacy: F,
) -> Result<PsbIndex, E>
where
    F: FnOnce() -> Result<PsbIndex, E>,
{
    match episode_directory_index(bytes, chunk_id) {
        Ok(Some(index)) => Ok(index),
        Ok(None) | Err(_) => legacy(),
    }
}

/// Fixed-size writer for the optional Episode 1 PSB directory sidecar.
///
/// The sidecar mirrors the interleaved headers and records each map's explicit
/// PSB magic, so a CD guest reconstructs a versioned checked [`PsbIndex`] with
/// one bounded read instead of fifteen distant seeks.
pub struct EpisodeDirectoryEncoder {
    bytes: [u8; EPISODE_DIRECTORY_BYTES],
}

impl EpisodeDirectoryEncoder {
    #[optimize(size)]
    pub fn new() -> Self {
        let mut bytes = [0; EPISODE_DIRECTORY_BYTES];
        bytes[0..4].copy_from_slice(&EPISODE_DIRECTORY_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&EPISODE_DIRECTORY_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(EPISODE_DIRECTORY_MAPS as u16).to_le_bytes());
        bytes[8..12].copy_from_slice(&(EPISODE_DIRECTORY_BYTES as u32).to_le_bytes());
        Self { bytes }
    }

    #[optimize(size)]
    pub fn set(&mut self, slot: usize, chunk_id: u32, index: &PsbIndex) -> bool {
        if slot >= EPISODE_DIRECTORY_MAPS {
            return false;
        }
        let start = EPISODE_DIRECTORY_HEADER_BYTES + slot * EPISODE_DIRECTORY_RECORD_BYTES;
        self.bytes[start..start + 4].copy_from_slice(&chunk_id.to_le_bytes());
        self.bytes[start + 4..start + 8].copy_from_slice(&index.file_len().to_le_bytes());
        self.bytes[start + 8..start + 12].copy_from_slice(&index.magic().to_le_bytes());
        for (kind_index, kind) in LumpKind::ALL.into_iter().enumerate() {
            let offset = start + 12 + kind_index * 4;
            self.bytes[offset..offset + 4].copy_from_slice(&index.lump(kind).len.to_le_bytes());
        }
        true
    }

    #[optimize(size)]
    pub fn finish(mut self) -> [u8; EPISODE_DIRECTORY_BYTES] {
        let checksum_offset = EPISODE_DIRECTORY_BYTES - 4;
        let checksum = directory_checksum(&self.bytes[..checksum_offset]);
        self.bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
        self.bytes
    }
}

impl Default for EpisodeDirectoryEncoder {
    #[optimize(size)]
    fn default() -> Self {
        Self::new()
    }
}

struct DirectoryIndexReader<'a> {
    record: &'a [u8],
    file_len: u32,
    magic: u32,
    length_offset: usize,
}

impl ReadAt for DirectoryIndexReader<'_> {
    type Error = EpisodeDirectoryError;

    #[optimize(size)]
    fn len(&self) -> u32 {
        self.file_len
    }

    #[optimize(size)]
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        if offset == 0 && output.len() == PSB_HEADER_BYTES as usize {
            output.copy_from_slice(&self.magic.to_le_bytes());
            return Ok(());
        }
        let mut cursor = PSB_HEADER_BYTES;
        for (kind_index, kind) in LumpKind::ALL.into_iter().enumerate() {
            let length_offset = self.length_offset + kind_index * 4;
            let length = u32::from_le_bytes(
                self.record[length_offset..length_offset + 4]
                    .try_into()
                    .unwrap(),
            );
            if offset == cursor && output.len() == LUMP_HEADER_BYTES as usize {
                output[..4].copy_from_slice(&(kind as i32).to_le_bytes());
                output[4..].copy_from_slice(&(length as i32).to_le_bytes());
                return Ok(());
            }
            cursor = cursor
                .checked_add(LUMP_HEADER_BYTES)
                .and_then(|cursor| cursor.checked_add(length))
                .ok_or(EpisodeDirectoryError::BadIndex)?;
        }
        Err(EpisodeDirectoryError::BadIndex)
    }
}

/// Decode one map index from a validated sidecar. `Ok(None)` means the
/// directory is valid but does not contain that chunk; malformed data is an
/// error so the guest can deliberately fall back to the legacy PSB scan.
#[optimize(size)]
pub fn episode_directory_index(
    bytes: &[u8],
    chunk_id: u32,
) -> Result<Option<PsbIndex>, EpisodeDirectoryError> {
    if bytes.len() < EPISODE_DIRECTORY_HEADER_BYTES + 4 {
        return Err(EpisodeDirectoryError::WrongSize);
    }
    if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != EPISODE_DIRECTORY_MAGIC {
        return Err(EpisodeDirectoryError::BadMagic);
    }
    let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    let (expected_bytes, record_bytes, magic_offset, length_offset) = match version {
        EPISODE_DIRECTORY_LEGACY_VERSION => (
            EPISODE_DIRECTORY_LEGACY_BYTES,
            EPISODE_DIRECTORY_LEGACY_RECORD_BYTES,
            None,
            8,
        ),
        EPISODE_DIRECTORY_VERSION => (
            EPISODE_DIRECTORY_BYTES,
            EPISODE_DIRECTORY_RECORD_BYTES,
            Some(8),
            12,
        ),
        _ => return Err(EpisodeDirectoryError::BadVersion),
    };
    if bytes.len() != expected_bytes {
        return Err(EpisodeDirectoryError::WrongSize);
    }
    if u16::from_le_bytes(bytes[6..8].try_into().unwrap()) as usize != EPISODE_DIRECTORY_MAPS {
        return Err(EpisodeDirectoryError::BadMapCount);
    }
    if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != expected_bytes {
        return Err(EpisodeDirectoryError::WrongSize);
    }
    let checksum_offset = expected_bytes - 4;
    let expected = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    if directory_checksum(&bytes[..checksum_offset]) != expected {
        return Err(EpisodeDirectoryError::BadChecksum);
    }
    for slot in 0..EPISODE_DIRECTORY_MAPS {
        let start = EPISODE_DIRECTORY_HEADER_BYTES + slot * record_bytes;
        let record = &bytes[start..start + record_bytes];
        if u32::from_le_bytes(record[0..4].try_into().unwrap()) != chunk_id {
            continue;
        }
        let file_len = u32::from_le_bytes(record[4..8].try_into().unwrap());
        let magic = magic_offset
            .map(|offset| u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap()))
            .unwrap_or(PSB_MAGIC);
        if magic != PSB_MAGIC && magic != PSB5_MAGIC {
            return Err(EpisodeDirectoryError::BadIndex);
        }
        if version == EPISODE_DIRECTORY_LEGACY_VERSION && magic != PSB_MAGIC {
            return Err(EpisodeDirectoryError::BadIndex);
        }
        let mut reader = DirectoryIndexReader {
            record,
            file_len,
            magic,
            length_offset,
        };
        let index = PsbIndex::read(&mut reader).map_err(|_| EpisodeDirectoryError::BadIndex)?;
        return Ok(Some(index));
    }
    Ok(None)
}

#[optimize(size)]
fn directory_checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

/// A reader which replays one already-validated PSB index from memory while
/// delegating payload reads to `inner`.
///
/// The on-disc format interleaves every eight-byte lump header with its
/// payload. Re-reading the index through a random-access CD reader otherwise
/// turns one validation pass into fifteen extra seeks. This adapter preserves
/// the shared loader's validation boundary without touching the disc again for
/// headers.
pub struct CachedIndexReader<'a, R> {
    index: &'a PsbIndex,
    inner: R,
}

impl<'a, R> CachedIndexReader<'a, R> {
    #[optimize(size)]
    pub const fn new(index: &'a PsbIndex, inner: R) -> Self {
        Self { index, inner }
    }

    #[optimize(size)]
    pub fn inner(&self) -> &R {
        &self.inner
    }

    #[optimize(size)]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: ReadAt> ReadAt for CachedIndexReader<'_, R> {
    type Error = R::Error;

    #[optimize(size)]
    fn len(&self) -> u32 {
        self.index.file_len()
    }

    #[optimize(size)]
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        if offset == 0 && output.len() == PSB_HEADER_BYTES as usize {
            output.copy_from_slice(&self.index.magic().to_le_bytes());
            return Ok(());
        }
        if output.len() == LUMP_HEADER_BYTES as usize {
            for kind in LumpKind::ALL {
                let range = self.index.lump(kind);
                if range.offset.checked_sub(LUMP_HEADER_BYTES) == Some(offset) {
                    output[..4].copy_from_slice(&(kind as i32).to_le_bytes());
                    output[4..].copy_from_slice(&(range.len as i32).to_le_bytes());
                    return Ok(());
                }
            }
        }
        self.inner.read_exact_at(offset, output)
    }
}

/// Quake's cooked sky texture contains adjacent foreground/background layers.
/// This remains Quake renderer policy rather than part of canonical PXBSP.
pub const TEXTURE_LAYERED_SKY: u8 = 64;

/// Bytes the guest reserves for the one resident-map arena reused by every
/// Episode 1 map.
///
/// PSoXide's engine default is [`psx_bsp::resident::MAX_RESIDENT_MAP_BYTES`]
/// (1,100,000), a generic budget for any XBSP world. The shareware Episode 1
/// corpus is fully known at build time, so this is Quake policy instead: the
/// largest indexed PSB5 map (`e1m3`) needs about 856 KiB after the canonical
/// render-node expansion. A measured 24 KiB structural-growth margin frees the
/// old PSB1 arena's unused heap
/// without making routine recooks brittle. `assert_cooked_maps_fit` loads every
/// map through this exact capacity and pins the measured high-water mark.
pub const RESIDENT_MAP_ARENA_BYTES: usize = 880_000;

/// One packed `pic_t` record in the Rust-cooked `gfx.dat` index.
pub const GRAPHICS_PICTURE_RECORD_BYTES: usize = 6;
/// One complete seven-weapon strip: six 24x16 icons and one 48x16 lightning
/// icon, all 8-bit indexed pixels.
pub const GRAPHICS_WEAPON_ICON_VARIANT_BYTES: usize = 3_072;
/// Exact inactive (`inv_*`) followed by active (`inv2_*`) strip pixels. They
/// remain outside the packed VRAM band and are uploaded over the seven icon
/// slots only when selection changes.
pub const GRAPHICS_WEAPON_ICON_BYTES: usize = GRAPHICS_WEAPON_ICON_VARIANT_BYTES * 2;
/// Byte offsets of each weapon icon inside one variant, with the terminal
/// offset included so a caller can take a checked slice without width tables.
pub const GRAPHICS_WEAPON_ICON_OFFSETS: [usize; 8] = [
    0,
    384,
    768,
    1_152,
    1_536,
    1_920,
    2_304,
    GRAPHICS_WEAPON_ICON_VARIANT_BYTES,
];

/// Fixed picture identifiers generated by `tools/cfg/id1/picmap.txt`.
///
/// These values deliberately match the last C runtime's `picids.h`. Keeping
/// the identifiers beside the wire record avoids duplicating unchecked magic
/// numbers in the guest renderer.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GraphicsPictureId {
    Conchars = 0x01,
    Number0 = 0x02,
    Face5 = 0x0e,
    Face4 = 0x0f,
    Face3 = 0x10,
    Face2 = 0x11,
    Face1 = 0x12,
    FacePain5 = 0x13,
    FacePain4 = 0x14,
    FacePain3 = 0x15,
    FacePain2 = 0x16,
    FacePain1 = 0x17,
    FaceQuad = 0x18,
    FaceInvisibility = 0x19,
    FaceInvulnerability = 0x1a,
    FaceInvisibilityInvulnerability = 0x1b,
    Armor1 = 0x1d,
    Armor2 = 0x1e,
    Armor3 = 0x1f,
    Shells = 0x20,
    Nails = 0x21,
    Rockets = 0x22,
    Cells = 0x23,
    Key1 = 0x24,
    Key2 = 0x25,
    Sigil1 = 0x26,
    Sigil2 = 0x27,
    Sigil3 = 0x28,
    Sigil4 = 0x29,
    PowerInvisibility = 0x2a,
    PowerInvulnerability = 0x2b,
    PowerBiosuit = 0x2c,
    PowerQuad = 0x2d,
    MenuDot1 = 0x2e,
    MenuDot2 = 0x2f,
    MenuDot3 = 0x30,
    MenuDot4 = 0x31,
    MenuDot5 = 0x32,
    MenuDot6 = 0x33,
    Disc = 0x35,
    InventoryBar0 = 0x36,
    InventoryBar1 = 0x37,
    InventoryBar2 = 0x38,
    InventoryWeaponShotgun = 0x39,
    InventoryWeaponSuperShotgun = 0x3a,
    InventoryWeaponNailgun = 0x3b,
    InventoryWeaponSuperNailgun = 0x3c,
    InventoryWeaponGrenadeLauncher = 0x3d,
    InventoryWeaponRocketLauncher = 0x3e,
    InventoryWeaponLightning = 0x3f,
    /// First of 29 exact crops that reconstruct the original 320x24 `sbar`.
    StatusBar0 = 0x40,
}

impl GraphicsPictureId {
    #[optimize(size)]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Owned, alignment-independent form of one cooked picture record.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphicsPicture {
    pub tpage: u16,
    pub u: u8,
    pub v: u8,
    pub width: u8,
    pub height: u8,
}

impl GraphicsPicture {
    /// Decode one little-endian record without borrowing the streaming
    /// scratch buffer which will immediately be reused for VRAM pixels.
    #[optimize(size)]
    pub fn decode(record: &[u8]) -> Option<Self> {
        let record = record.get(..GRAPHICS_PICTURE_RECORD_BYTES)?;
        Some(Self {
            tpage: u16::from_le_bytes([record[0], record[1]]),
            u: record[2],
            v: record[3],
            width: record[4],
            height: record[5],
        })
    }

    /// The null id is the only empty record. Every real picture must fit one
    /// PS1 texture page because the GPU quad carries 8-bit UVs.
    #[optimize(size)]
    pub const fn is_valid_real_picture(self) -> bool {
        self.width != 0
            && self.height != 0
            && self.u as u16 + self.width as u16 <= 256
            && self.v as u16 + self.height as u16 <= 256
    }
}

#[cfg(test)]
mod graphics_tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn picture_records_are_copied_from_unaligned_little_endian_bytes() {
        let storage = [0xaa, 0x9f, 0x00, 16, 64, 24, 8, 0xbb];
        let picture = GraphicsPicture::decode(&storage[1..7]).expect("complete record");
        assert_eq!(
            picture,
            GraphicsPicture {
                tpage: 0x009f,
                u: 16,
                v: 64,
                width: 24,
                height: 8,
            }
        );
        assert!(picture.is_valid_real_picture());
        assert_eq!(storage[1], 0x9f, "the source buffer was only borrowed");
    }

    #[optimize(size)]
    #[test]
    fn picture_records_reject_truncation_empty_images_and_page_crossings() {
        assert_eq!(GraphicsPicture::decode(&[0; 5]), None);
        assert!(!GraphicsPicture::default().is_valid_real_picture());
        assert!(!GraphicsPicture {
            tpage: 0x008f,
            u: 250,
            v: 0,
            width: 12,
            height: 24,
        }
        .is_valid_real_picture());
    }
}

#[cfg(test)]
mod cached_index_tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    struct CountingReader<'a> {
        bytes: &'a [u8],
        reads: usize,
    }

    impl ReadAt for CountingReader<'_> {
        type Error = ();

        #[optimize(size)]
        fn len(&self) -> u32 {
            self.bytes.len() as u32
        }

        #[optimize(size)]
        fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start.checked_add(output.len()).ok_or(())?;
            output.copy_from_slice(self.bytes.get(start..end).ok_or(())?);
            self.reads += 1;
            Ok(())
        }
    }

    #[optimize(size)]
    fn psb(version: PsbVersion) -> Vec<u8> {
        let mut bytes = version.magic().to_le_bytes().to_vec();
        for (index, kind) in LumpKind::ALL.into_iter().enumerate() {
            let len = kind
                .record_size(version)
                .map_or(index + 1, |size| size as usize);
            let payload = vec![index as u8; len];
            bytes.extend_from_slice(&(kind as i32).to_le_bytes());
            bytes.extend_from_slice(&(payload.len() as i32).to_le_bytes());
            bytes.extend_from_slice(&payload);
        }
        bytes
    }

    #[optimize(size)]
    #[test]
    fn reparsing_a_cached_index_performs_no_payload_reads() {
        let bytes = psb(PsbVersion::IndexedV5);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let inner = CountingReader {
            bytes: &bytes,
            reads: 0,
        };
        let mut cached = CachedIndexReader::new(&index, inner);

        assert_eq!(PsbIndex::read(&mut cached).unwrap(), index);
        assert_eq!(cached.inner().reads, 0);

        let range = index.lump(LumpKind::Vertices);
        let mut payload = vec![0; range.len as usize];
        cached.read_exact_at(range.offset, &mut payload).unwrap();
        assert_eq!(cached.inner().reads, 1);
        assert!(payload.iter().all(|byte| *byte == LumpKind::Vertices as u8));
    }

    #[optimize(size)]
    #[test]
    fn cached_index_replays_legacy_magic_without_reinterpreting_records() {
        let bytes = psb(PsbVersion::LegacyV1);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let inner = CountingReader {
            bytes: &bytes,
            reads: 0,
        };
        let mut cached = CachedIndexReader::new(&index, inner);
        assert_eq!(PsbIndex::read(&mut cached).unwrap(), index);
        assert_eq!(cached.inner().reads, 0);
        assert_eq!(index.version(), PsbVersion::LegacyV1);
    }

    #[optimize(size)]
    #[test]
    fn episode_directory_round_trips_checked_indexes_and_detects_corruption() {
        let bytes = psb(PsbVersion::IndexedV5);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let mut encoder = EpisodeDirectoryEncoder::new();
        assert!(encoder.set(0, 100, &index));
        assert!(!encoder.set(EPISODE_DIRECTORY_MAPS, 109, &index));
        let directory = encoder.finish();

        assert_eq!(
            episode_directory_index(&directory, 100).unwrap(),
            Some(index.clone())
        );
        assert_eq!(index.version(), PsbVersion::IndexedV5);
        assert_eq!(episode_directory_index(&directory, 109).unwrap(), None);

        let mut corrupt = directory;
        corrupt[32] ^= 0x80;
        assert_eq!(
            episode_directory_index(&corrupt, 100),
            Err(EpisodeDirectoryError::BadChecksum)
        );
        assert_eq!(
            episode_directory_index(&directory[..directory.len() - 1], 100),
            Err(EpisodeDirectoryError::WrongSize)
        );
    }

    #[optimize(size)]
    #[test]
    fn missing_or_malformed_optional_directory_uses_the_legacy_index() {
        let bytes = psb(PsbVersion::LegacyV1);
        let mut source = SliceReader::new(&bytes);
        let legacy = PsbIndex::read(&mut source).unwrap();
        let mut calls = 0;
        let actual = episode_directory_index_or_try(&[], 100, || {
            calls += 1;
            Ok::<_, ()>(legacy.clone())
        })
        .unwrap();
        assert_eq!(actual, legacy);
        assert_eq!(calls, 1);

        let mut encoder = EpisodeDirectoryEncoder::new();
        assert!(encoder.set(0, 100, &legacy));
        let directory = encoder.finish();
        let actual = episode_directory_index_or_try(&directory, 100, || {
            calls += 1;
            Ok::<_, ()>(legacy.clone())
        })
        .unwrap();
        assert_eq!(actual, legacy);
        assert_eq!(calls, 1, "valid directory must not scan legacy headers");
    }

    #[optimize(size)]
    #[test]
    fn legacy_qidx_v1_reconstructs_only_legacy_psb_magic() {
        let bytes = psb(PsbVersion::LegacyV1);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let mut directory = [0u8; EPISODE_DIRECTORY_LEGACY_BYTES];
        directory[0..4].copy_from_slice(&EPISODE_DIRECTORY_MAGIC.to_le_bytes());
        directory[4..6].copy_from_slice(&EPISODE_DIRECTORY_LEGACY_VERSION.to_le_bytes());
        directory[6..8].copy_from_slice(&(EPISODE_DIRECTORY_MAPS as u16).to_le_bytes());
        directory[8..12].copy_from_slice(&(EPISODE_DIRECTORY_LEGACY_BYTES as u32).to_le_bytes());
        let record = &mut directory[EPISODE_DIRECTORY_HEADER_BYTES
            ..EPISODE_DIRECTORY_HEADER_BYTES + EPISODE_DIRECTORY_LEGACY_RECORD_BYTES];
        record[0..4].copy_from_slice(&100u32.to_le_bytes());
        record[4..8].copy_from_slice(&index.file_len().to_le_bytes());
        for (kind_index, kind) in LumpKind::ALL.into_iter().enumerate() {
            let offset = 8 + kind_index * 4;
            record[offset..offset + 4].copy_from_slice(&index.lump(kind).len.to_le_bytes());
        }
        let checksum_offset = EPISODE_DIRECTORY_LEGACY_BYTES - 4;
        let checksum = directory_checksum(&directory[..checksum_offset]);
        directory[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());

        let decoded = episode_directory_index(&directory, 100).unwrap().unwrap();
        assert_eq!(decoded, index);
        assert_eq!(decoded.version(), PsbVersion::LegacyV1);
    }

    #[optimize(size)]
    #[test]
    fn psb_magic_selects_record_schema_without_silent_reinterpretation() {
        let mut compact = psb(PsbVersion::IndexedV5);
        compact[0..4].copy_from_slice(&PSB_MAGIC.to_le_bytes());
        let mut source = SliceReader::new(&compact);
        let error = PsbIndex::read(&mut source).unwrap_err();
        assert!(
            matches!(error, PsbError::MisalignedLump { .. }),
            "legacy magic over compact lumps must fail alignment: {error:?}"
        );
    }

    #[optimize(size)]
    #[test]
    fn qidx2_rejects_an_unknown_embedded_psb_magic() {
        let bytes = psb(PsbVersion::IndexedV5);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let mut encoder = EpisodeDirectoryEncoder::new();
        assert!(encoder.set(0, 100, &index));
        let mut directory = encoder.finish();
        let magic = EPISODE_DIRECTORY_HEADER_BYTES + 8;
        directory[magic..magic + 4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        let checksum_offset = directory.len() - 4;
        let checksum = directory_checksum(&directory[..checksum_offset]);
        directory[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            episode_directory_index(&directory, 100),
            Err(EpisodeDirectoryError::BadIndex)
        );
    }

    #[optimize(size)]
    #[test]
    fn qidx2_rejects_the_experimental_psb2_magic() {
        let bytes = psb(PsbVersion::IndexedV5);
        let mut source = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut source).unwrap();
        let mut encoder = EpisodeDirectoryEncoder::new();
        assert!(encoder.set(0, 100, &index));
        let mut directory = encoder.finish();
        let magic = EPISODE_DIRECTORY_HEADER_BYTES + 8;
        directory[magic..magic + 4].copy_from_slice(&PSB2_MAGIC.to_le_bytes());
        let checksum_offset = directory.len() - 4;
        let checksum = directory_checksum(&directory[..checksum_offset]);
        directory[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            episode_directory_index(&directory, 100),
            Err(EpisodeDirectoryError::BadIndex)
        );
    }
}
