//! Rust host-side readers and cookers for the Quake shareware data.
//!
//! This crate replaces the historical C utilities. Inputs are treated as
//! untrusted binary data so a damaged PAK, WAD, or picture fails with a
//! descriptive error instead of indexing beyond its buffer.

use std::collections::BTreeMap;
use std::fmt;

use quake_formats::{
    GRAPHICS_WEAPON_ICON_BYTES, GRAPHICS_WEAPON_ICON_OFFSETS, GRAPHICS_WEAPON_ICON_VARIANT_BYTES,
};

mod bsp;
mod entities;
mod geometry;
mod map;
mod model;
mod sound;

pub use bsp::{Bsp, BspLump, BspStats, MipTexture};
pub use entities::{cook_entities, CookedEntities, SourceEntity};
pub use geometry::{cook_geometry, GeometryLumps, SkyEncoding};
pub use map::{cook_map, CookedMap, MapCookConfig};
pub use model::{cook_geometry_and_models, CookedModels, ModelCookStats};
pub use psx_sfx::PARKING_TAIL as SFX_PARKING_TAIL;
pub use sound::{
    cook_global_sounds, cook_monolithic_sounds_for_validation, cook_sounds,
    merge_sound_banks_for_validation, CookedGlobalSounds, CookedSounds, SoundCookStats,
};

const PACK_MAGIC: &[u8; 4] = b"PACK";
const PACK_HEADER_BYTES: usize = 12;
const PACK_ENTRY_BYTES: usize = 64;
const PACK_NAME_BYTES: usize = 56;
const WAD_MAGIC: &[u8; 4] = b"WAD2";
const WAD_HEADER_BYTES: usize = 12;
const WAD_ENTRY_BYTES: usize = 32;
const WAD_NAME_BYTES: usize = 16;

const CLUT_COLORS: usize = 256;
const GAMMA_LEVELS: usize = 8;
const CLUT_BYTES: usize = CLUT_COLORS * GAMMA_LEVELS * 2;
const VRAM_WIDTH_WORDS: usize = 64;
const VRAM_HEIGHT: usize = 512;
const VRAM_BYTES: usize = VRAM_WIDTH_WORDS * VRAM_HEIGHT * 2;
const PICTURE_RECORD_BYTES: usize = 6;
const MAX_PICTURES: usize = 256;
const PICTURES_X_START: usize = 960;
const PICTURES_X_END: usize = 1023;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookError(String);

impl CookError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CookError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0)
    }
}

impl std::error::Error for CookError {}

#[derive(Clone, Debug)]
pub struct PakArchive<'a> {
    bytes: &'a [u8],
    entries: BTreeMap<String, (usize, usize)>,
}

impl<'a> PakArchive<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CookError> {
        if bytes.len() < PACK_HEADER_BYTES {
            return Err(CookError::new("truncated Quake PAK header"));
        }
        if bytes.get(..4) != Some(PACK_MAGIC) {
            return Err(CookError::new("bad Quake PAK magic"));
        }
        let directory_offset = read_i32(bytes, 4, "PAK directory offset")?;
        let directory_len = read_i32(bytes, 8, "PAK directory length")?;
        if directory_offset < 0
            || directory_len < 0
            || directory_len as usize % PACK_ENTRY_BYTES != 0
        {
            return Err(CookError::new("invalid Quake PAK directory"));
        }
        let directory = checked_slice(
            bytes,
            directory_offset as usize,
            directory_len as usize,
            "PAK directory",
        )?;
        let mut entries = BTreeMap::new();
        for record in directory.chunks_exact(PACK_ENTRY_BYTES) {
            let name = c_string(&record[..PACK_NAME_BYTES], "PAK entry name")?;
            let offset = read_i32(record, PACK_NAME_BYTES, "PAK entry offset")?;
            let len = read_i32(record, PACK_NAME_BYTES + 4, "PAK entry length")?;
            if offset < 0 || len < 0 {
                return Err(CookError::new(format!("negative PAK range for {name}")));
            }
            checked_slice(
                bytes,
                offset as usize,
                len as usize,
                &format!("PAK entry {name}"),
            )?;
            if entries
                .insert(name.clone(), (offset as usize, len as usize))
                .is_some()
            {
                return Err(CookError::new(format!("duplicate PAK entry {name}")));
            }
        }
        Ok(Self { bytes, entries })
    }

    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        let &(offset, len) = self.entries.get(name)?;
        self.bytes.get(offset..offset + len)
    }

    pub fn require(&self, name: &str) -> Result<&'a [u8], CookError> {
        self.get(name)
            .ok_or_else(|| CookError::new(format!("PAK has no {name}")))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug)]
struct WadArchive<'a> {
    bytes: &'a [u8],
    entries: BTreeMap<String, (usize, usize)>,
}

impl<'a> WadArchive<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, CookError> {
        if bytes.len() < WAD_HEADER_BYTES {
            return Err(CookError::new("truncated Quake WAD header"));
        }
        if bytes.get(..4) != Some(WAD_MAGIC) {
            return Err(CookError::new("bad Quake WAD magic"));
        }
        let count = read_i32(bytes, 4, "WAD lump count")?;
        let directory_offset = read_i32(bytes, 8, "WAD directory offset")?;
        if count < 0 || directory_offset < 0 {
            return Err(CookError::new("invalid Quake WAD directory"));
        }
        let directory_len = (count as usize)
            .checked_mul(WAD_ENTRY_BYTES)
            .ok_or_else(|| CookError::new("WAD directory size overflow"))?;
        let directory = checked_slice(
            bytes,
            directory_offset as usize,
            directory_len,
            "WAD directory",
        )?;
        let mut entries = BTreeMap::new();
        for record in directory.chunks_exact(WAD_ENTRY_BYTES) {
            let offset = read_i32(record, 0, "WAD entry offset")?;
            let disk_len = read_i32(record, 4, "WAD entry disk length")?;
            let len = read_i32(record, 8, "WAD entry length")?;
            let compression = record[13];
            let name =
                c_string(&record[16..16 + WAD_NAME_BYTES], "WAD entry name")?.to_ascii_uppercase();
            if offset < 0 || disk_len < 0 || len < 0 || disk_len != len {
                return Err(CookError::new(format!("invalid WAD range for {name}")));
            }
            if compression != 0 {
                return Err(CookError::new(format!("compressed WAD lump {name}")));
            }
            checked_slice(
                bytes,
                offset as usize,
                len as usize,
                &format!("WAD lump {name}"),
            )?;
            if entries
                .insert(name.clone(), (offset as usize, len as usize))
                .is_some()
            {
                return Err(CookError::new(format!("duplicate WAD lump {name}")));
            }
        }
        Ok(Self { bytes, entries })
    }

    fn require(&self, name: &str) -> Result<&'a [u8], CookError> {
        let key = name.to_ascii_uppercase();
        let &(offset, len) = self
            .entries
            .get(&key)
            .ok_or_else(|| CookError::new(format!("WAD has no {key}")))?;
        Ok(&self.bytes[offset..offset + len])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PictureRecord {
    tpage: u16,
    u: u8,
    v: u8,
    width: u8,
    height: u8,
}

/// Cook `gfx.dat` directly from Quake's PAK and the repository pic map.
pub fn cook_gfx(pak: &PakArchive<'_>, pic_map: &str) -> Result<Vec<u8>, CookError> {
    let palette = pak.require("gfx/palette.lmp")?;
    if palette.len() < CLUT_COLORS * 3 {
        return Err(CookError::new("gfx/palette.lmp is too small"));
    }
    let cluts = make_cluts(palette);
    let wad = WadArchive::parse(pak.require("gfx.wad")?)?;
    let mut vram = vec![0u8; VRAM_BYTES];
    // Which picture owns each halfword of the picture band, by name. VRAM is
    // the one resource with nothing between the writer and the hardware, so a
    // second picture landing on the first silently reinterprets its pixels and
    // the symptom shows up on whichever screen samples the loser. Every
    // coordinate here is authored by hand in picmap.txt; `validate_picture`
    // only checks each one against the band and the page edges, never against
    // its neighbours.
    let mut owner: Vec<Option<&str>> = vec![None; VRAM_WIDTH_WORDS * VRAM_HEIGHT];
    let mut pictures = vec![PictureRecord {
        tpage: 0,
        u: 0,
        v: 0,
        width: 0,
        height: 0,
    }];

    for (line_index, line) in pic_map.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else {
            continue;
        };
        if kind.starts_with('#') {
            continue;
        }
        let (rgb555, cropped) = match kind {
            "pic" => (false, false),
            "pic16" => (true, false),
            "piccrop" => (false, true),
            _ => continue,
        };
        let name = required_field(fields.next(), line_index, "picture name")?;
        let x = parse_usize(fields.next(), line_index, "VRAM X")?;
        let y = parse_usize(fields.next(), line_index, "VRAM Y")?;
        let crop = if cropped {
            Some((
                parse_usize(fields.next(), line_index, "crop X")?,
                parse_usize(fields.next(), line_index, "crop Y")?,
                parse_usize(fields.next(), line_index, "crop width")?,
                parse_usize(fields.next(), line_index, "crop height")?,
            ))
        } else {
            None
        };
        let color_key = match fields.next() {
            Some(value) => value.parse::<u8>().map_err(|_| {
                CookError::new(format!("picmap line {} has bad color key", line_index + 1))
            })?,
            None => 0xff,
        };
        if pictures.len() >= MAX_PICTURES {
            return Err(CookError::new("picmap exceeds 255 real pictures"));
        }

        let (mut width, mut height, mut pixels) = load_picture(pak, &wad, name)?;
        if let Some((crop_x, crop_y, crop_width, crop_height)) = crop {
            pixels = crop_picture(
                name,
                &pixels,
                width,
                height,
                crop_x,
                crop_y,
                crop_width,
                crop_height,
            )?;
            width = crop_width;
            height = crop_height;
        }
        validate_picture(name, x, y, width, height, rgb555)?;
        let bytes_per_pixel = if rgb555 { 2 } else { 1 };
        let row_bytes = width * bytes_per_pixel;
        if rgb555 {
            let mut converted = Vec::with_capacity(width * height * 2);
            for &index in &pixels {
                converted.extend_from_slice(&cluts[index as usize].to_le_bytes());
            }
            pixels = converted;
        } else if color_key != 0xff {
            for pixel in &mut pixels {
                if *pixel == color_key {
                    *pixel = 0xff;
                }
            }
        }

        let page_x = x & 0x3c0;
        for row in 0..height {
            let destination = ((y + row) * VRAM_WIDTH_WORDS + (x - page_x)) * 2;
            let source = row * row_bytes;
            let word = destination / 2;
            for cell in &mut owner[word..word + row_bytes / 2] {
                if let Some(first) = *cell {
                    return Err(CookError::new(format!(
                        "picture {name} at VRAM {x},{y} overlaps picture {first}"
                    )));
                }
                *cell = Some(name);
            }
            vram[destination..destination + row_bytes]
                .copy_from_slice(&pixels[source..source + row_bytes]);
        }
        pictures.push(PictureRecord {
            tpage: picture_tpage(rgb555, x, y),
            u: (((x - page_x) << usize::from(!rgb555)) & 0xff) as u8,
            v: (y & 0xff) as u8,
            width: width as u8,
            height: height as u8,
        });
    }

    let mut output = Vec::with_capacity(
        CLUT_BYTES
            + 2
            + pictures.len() * PICTURE_RECORD_BYTES
            + VRAM_BYTES
            + GRAPHICS_WEAPON_ICON_BYTES,
    );
    for color in cluts {
        output.extend_from_slice(&color.to_le_bytes());
    }
    output.extend_from_slice(&(pictures.len() as u16).to_le_bytes());
    for picture in pictures {
        output.extend_from_slice(&picture.tpage.to_le_bytes());
        output.extend_from_slice(&[picture.u, picture.v, picture.width, picture.height]);
    }
    output.extend_from_slice(&vram);
    // The picture band has no room for both original strip states. Carry the
    // exact indexed pixels after the resident band instead: the guest keeps
    // this tiny pair in RAM and uploads only the old/new slots on a weapon
    // switch. No Quake artwork is checked into the repository; it is still
    // derived from the player's verified shareware PAK at cook time.
    const WEAPONS: [&str; 7] = [
        "shotgun", "sshotgun", "nailgun", "snailgun", "rlaunch", "srlaunch", "lightng",
    ];
    for prefix in ["inv_", "inv2_"] {
        let start = output.len();
        for (index, weapon) in WEAPONS.iter().enumerate() {
            let name = format!("{prefix}{weapon}");
            let (width, height, pixels) = load_picture(pak, &wad, &name)?;
            let expected =
                GRAPHICS_WEAPON_ICON_OFFSETS[index + 1] - GRAPHICS_WEAPON_ICON_OFFSETS[index];
            if width * height != expected || pixels.len() != expected || height != 16 {
                return Err(CookError::new(format!(
                    "weapon strip picture {name} has unexpected {width}x{height} geometry"
                )));
            }
            output.extend_from_slice(&pixels);
        }
        if output.len() - start != GRAPHICS_WEAPON_ICON_VARIANT_BYTES {
            return Err(CookError::new("weapon strip variant has unexpected size"));
        }
    }
    Ok(output)
}

fn crop_picture(
    name: &str,
    pixels: &[u8],
    source_width: usize,
    source_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, CookError> {
    if width == 0
        || height == 0
        || x.checked_add(width)
            .is_none_or(|right| right > source_width)
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > source_height)
    {
        return Err(CookError::new(format!(
            "picture {name} crop lies outside its source"
        )));
    }
    let mut cropped = Vec::with_capacity(width * height);
    for row in y..y + height {
        let start = row * source_width + x;
        cropped.extend_from_slice(&pixels[start..start + width]);
    }
    Ok(cropped)
}

fn picture_tpage(rgb555: bool, x: usize, y: usize) -> u16 {
    // Indexed pictures use the PS1's 8-bit texture mode. `pic16` pictures are
    // expanded to RGB555 above and must be sampled as 16-bit direct colour;
    // labelling them as 8-bit makes the GPU reinterpret pairs of colour bytes
    // as palette indexes (the corrupted loading-disc icon was the visible
    // symptom).
    psx_tpage(if rgb555 { 2 } else { 1 }, 0, x, y)
}

fn load_picture(
    pak: &PakArchive<'_>,
    wad: &WadArchive<'_>,
    name: &str,
) -> Result<(usize, usize, Vec<u8>), CookError> {
    if name == "conchars" {
        let pixels = wad.require("CONCHARS")?;
        if pixels.len() < 128 * 128 {
            return Err(CookError::new("CONCHARS is too small"));
        }
        return Ok((128, 128, pixels[..128 * 128].to_vec()));
    }
    let bytes = if name.contains('/') || name.contains('.') {
        pak.require(name)?
    } else {
        wad.require(name)?
    };
    let width = read_i32(bytes, 0, &format!("{name} width"))?;
    let height = read_i32(bytes, 4, &format!("{name} height"))?;
    if width <= 0 || height <= 0 {
        return Err(CookError::new(format!("picture {name} has invalid size")));
    }
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| CookError::new(format!("picture {name} size overflow")))?;
    let pixels = checked_slice(bytes, 8, pixel_count, &format!("picture {name} pixels"))?;
    Ok((width as usize, height as usize, pixels.to_vec()))
}

fn validate_picture(
    name: &str,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    rgb555: bool,
) -> Result<(), CookError> {
    if width >= 256 || height >= 256 || x & 1 != 0 || width & 1 != 0 {
        return Err(CookError::new(format!(
            "picture {name} has unsupported geometry"
        )));
    }
    if !(PICTURES_X_START..=PICTURES_X_END).contains(&x) || y >= VRAM_HEIGHT {
        return Err(CookError::new(format!(
            "picture {name} lies outside picture VRAM"
        )));
    }
    let width_words = if rgb555 { width } else { width / 2 };
    let page_x = x & 0x3c0;
    let page_y = y & 0x100;
    if width_words + x - page_x > VRAM_WIDTH_WORDS || height + y - page_y > 256 {
        return Err(CookError::new(format!(
            "picture {name} crosses a texture page"
        )));
    }
    Ok(())
}

fn make_cluts(palette: &[u8]) -> [u16; CLUT_COLORS * GAMMA_LEVELS] {
    let mut output = [0u16; CLUT_COLORS * GAMMA_LEVELS];
    // Levels 7 and 8 continue the uniform 0.1 spacing past the original
    // brightest row. The console picture reads far darker than the emulator's,
    // so the slider needs headroom the six-row table did not have.
    let powers = [1.0f64, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3];
    let mut adjusted = [0u8; CLUT_COLORS * 3];
    for (level, power) in powers.into_iter().enumerate() {
        if level == 0 {
            adjusted.copy_from_slice(&palette[..CLUT_COLORS * 3]);
        } else {
            for (destination, &source) in adjusted.iter_mut().zip(&palette[..CLUT_COLORS * 3]) {
                let value =
                    (((source as f64 + 1.0) / 256.0).powf(power) * 255.0 + 0.5).clamp(0.0, 255.0);
                *destination = value as u8;
            }
        }
        for color in 0..CLUT_COLORS - 1 {
            let offset = color * 3;
            let mut packed = ((adjusted[offset + 2] as u16 >> 3) << 10)
                | ((adjusted[offset + 1] as u16 >> 3) << 5)
                | (adjusted[offset] as u16 >> 3);
            if packed == 0 {
                packed = 0x8000;
            }
            output[level * CLUT_COLORS + color] = packed;
        }
        output[level * CLUT_COLORS + CLUT_COLORS - 1] = 0;
    }
    output
}

pub(crate) fn psx_tpage(depth: usize, blend: usize, x: usize, y: usize) -> u16 {
    ((((x & 0x3ff) >> 6) | ((y >> 8) << 4) | ((blend & 3) << 5) | ((depth & 3) << 7)) & 0xffff)
        as u16
}

fn required_field<'a>(
    value: Option<&'a str>,
    line_index: usize,
    field: &str,
) -> Result<&'a str, CookError> {
    value.ok_or_else(|| CookError::new(format!("picmap line {} has no {field}", line_index + 1)))
}

fn parse_usize(value: Option<&str>, line_index: usize, field: &str) -> Result<usize, CookError> {
    required_field(value, line_index, field)?
        .parse()
        .map_err(|_| CookError::new(format!("picmap line {} has bad {field}", line_index + 1)))
}

fn c_string(bytes: &[u8], context: &str) -> Result<String, CookError> {
    let len = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    let text = std::str::from_utf8(&bytes[..len])
        .map_err(|_| CookError::new(format!("{context} is not UTF-8")))?;
    if text.is_empty() {
        return Err(CookError::new(format!("{context} is empty")));
    }
    Ok(text.to_owned())
}

fn checked_slice<'a>(
    bytes: &'a [u8],
    offset: usize,
    len: usize,
    context: &str,
) -> Result<&'a [u8], CookError> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| CookError::new(format!("{context} range overflow")))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| CookError::new(format!("{context} is truncated")))
}

fn read_i32(bytes: &[u8], offset: usize, context: &str) -> Result<i32, CookError> {
    let word = checked_slice(bytes, offset, 4, context)?;
    Ok(i32::from_le_bytes(word.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pak_reader_rejects_out_of_bounds_entries() {
        let mut bytes = vec![0u8; PACK_HEADER_BYTES + PACK_ENTRY_BYTES];
        bytes[..4].copy_from_slice(PACK_MAGIC);
        bytes[4..8].copy_from_slice(&(PACK_HEADER_BYTES as i32).to_le_bytes());
        bytes[8..12].copy_from_slice(&(PACK_ENTRY_BYTES as i32).to_le_bytes());
        bytes[12] = b'x';
        bytes[12 + PACK_NAME_BYTES..16 + PACK_NAME_BYTES].copy_from_slice(&1000i32.to_le_bytes());
        bytes[16 + PACK_NAME_BYTES..20 + PACK_NAME_BYTES].copy_from_slice(&4i32.to_le_bytes());
        assert!(PakArchive::parse(&bytes).is_err());
    }

    #[test]
    fn palette_preserves_opaque_black_and_transparent_last_index() {
        let palette = [0u8; CLUT_COLORS * 3];
        let cluts = make_cluts(&palette);
        assert_eq!(cluts[0], 0x8000);
        assert_eq!(cluts[CLUT_COLORS - 1], 0);
        assert_eq!(cluts[CLUT_COLORS], 0x8000);
    }

    #[test]
    fn tpage_matches_psx_bit_layout() {
        assert_eq!(psx_tpage(1, 0, 960, 0), 0x008f);
        assert_eq!(psx_tpage(1, 0, 960, 256), 0x009f);
    }

    #[test]
    fn direct_colour_pictures_use_a_16_bit_tpage() {
        let indexed = picture_tpage(false, 960, 424);
        let direct = picture_tpage(true, 960, 424);
        assert_eq!((indexed >> 7) & 0x3, 1);
        assert_eq!((direct >> 7) & 0x3, 2);
    }
}

#[cfg(test)]
mod gfx_layout_tests {
    use super::*;

    /// The shipping picmap must not author two pictures onto the same VRAM
    /// halfword. Every coordinate in it is hand-written, and nothing else
    /// compares them, so this is the only thing standing between a bad edit
    /// and a picture that silently reinterprets its neighbour's pixels.
    #[test]
    fn the_shipping_picmap_places_every_picture_clear_of_the_others() {
        let Ok(pak_bytes) = std::fs::read("../../.quakepsx/cache/shareware/ID1/PAK0.PAK") else {
            // The extracted shareware is not part of the repo; skip when the
            // cache has not been primed yet.
            return;
        };
        let pic_map = std::fs::read_to_string("../../tools/cfg/id1/picmap.txt")
            .expect("the repository pic map");
        let pak = PakArchive::parse(&pak_bytes).expect("shareware PAK0");
        let cooked = cook_gfx(&pak, &pic_map).expect("no picture overlaps another");
        // Proves the check actually ran over real pictures rather than
        // short-circuiting: the cooked blob carries the CLUTs, the record
        // table and the whole picture band.
        assert!(cooked.len() > VRAM_BYTES, "cook_gfx produced a real band");
    }

    /// And the guard has to actually fire, or the test above proves nothing.
    #[test]
    fn two_pictures_authored_onto_the_same_words_are_rejected() {
        let Ok(pak_bytes) = std::fs::read("../../.quakepsx/cache/shareware/ID1/PAK0.PAK") else {
            return;
        };
        let pak = PakArchive::parse(&pak_bytes).expect("shareware PAK0");
        let clashing = "pic sb_armor1 976 256\npic sb_armor2 976 256\n";
        let error = cook_gfx(&pak, clashing).expect_err("the second picture lands on the first");
        assert!(
            error.to_string().contains("overlaps picture sb_armor1"),
            "the error names the picture already there: {error}"
        );
    }

    #[test]
    fn a_wide_wad_picture_can_be_cropped_into_one_page_sized_record() {
        let Ok(pak_bytes) = std::fs::read("../../.quakepsx/cache/shareware/ID1/PAK0.PAK") else {
            return;
        };
        let pak = PakArchive::parse(&pak_bytes).expect("shareware PAK0");
        let cooked = cook_gfx(&pak, "piccrop ibar 960 448 128 0 128 24\n")
            .expect("the middle third of ibar fits one texture page");
        let count = u16::from_le_bytes([cooked[CLUT_BYTES], cooked[CLUT_BYTES + 1]]);
        assert_eq!(count, 2, "null record plus the cropped picture");
        let record = CLUT_BYTES + 2 + PICTURE_RECORD_BYTES;
        assert_eq!(cooked[record + 4], 128);
        assert_eq!(cooked[record + 5], 24);
        assert_eq!(
            cooked.len(),
            CLUT_BYTES
                + 2
                + count as usize * PICTURE_RECORD_BYTES
                + VRAM_BYTES
                + GRAPHICS_WEAPON_ICON_BYTES
        );
    }
}
