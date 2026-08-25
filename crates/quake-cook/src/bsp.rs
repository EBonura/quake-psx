use super::{checked_slice, read_i32, CookError};

const BSP_VERSION: i32 = 29;
const BSP_HEADER_BYTES: usize = 4 + BSP_LUMP_COUNT * 8;
const BSP_LUMP_COUNT: usize = 15;
const MIP_TEXTURE_HEADER_BYTES: usize = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BspLump {
    Entities = 0,
    Planes = 1,
    MipTextures = 2,
    Vertices = 3,
    Visibility = 4,
    Nodes = 5,
    TextureInfo = 6,
    Faces = 7,
    Lighting = 8,
    ClipNodes = 9,
    Leaves = 10,
    MarkSurfaces = 11,
    Edges = 12,
    SurfaceEdges = 13,
    Models = 14,
}

impl BspLump {
    pub const ALL: [Self; BSP_LUMP_COUNT] = [
        Self::Entities,
        Self::Planes,
        Self::MipTextures,
        Self::Vertices,
        Self::Visibility,
        Self::Nodes,
        Self::TextureInfo,
        Self::Faces,
        Self::Lighting,
        Self::ClipNodes,
        Self::Leaves,
        Self::MarkSurfaces,
        Self::Edges,
        Self::SurfaceEdges,
        Self::Models,
    ];

    pub const fn record_size(self) -> Option<usize> {
        match self {
            Self::Entities | Self::MipTextures | Self::Visibility | Self::Lighting => None,
            Self::Planes => Some(20),
            Self::Vertices => Some(12),
            Self::Nodes => Some(24),
            Self::TextureInfo => Some(40),
            Self::Faces => Some(20),
            Self::ClipNodes => Some(8),
            Self::Leaves => Some(28),
            Self::MarkSurfaces => Some(2),
            Self::Edges => Some(4),
            Self::SurfaceEdges => Some(4),
            Self::Models => Some(64),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LumpRange {
    offset: usize,
    len: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BspStats {
    pub planes: usize,
    pub textures: usize,
    pub vertices: usize,
    pub nodes: usize,
    pub faces: usize,
    pub clip_nodes: usize,
    pub leaves: usize,
    pub models: usize,
}

#[derive(Clone, Debug)]
pub struct Bsp<'a> {
    bytes: &'a [u8],
    lumps: [LumpRange; BSP_LUMP_COUNT],
    texture_count: usize,
}

impl<'a> Bsp<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CookError> {
        if bytes.len() < BSP_HEADER_BYTES {
            return Err(CookError::new("truncated Quake BSP header"));
        }
        let version = read_i32(bytes, 0, "BSP version")?;
        if version != BSP_VERSION {
            return Err(CookError::new(format!(
                "unsupported Quake BSP version {version}"
            )));
        }
        let mut lumps = [LumpRange::default(); BSP_LUMP_COUNT];
        for kind in BspLump::ALL {
            let header = 4 + kind as usize * 8;
            let offset = read_i32(bytes, header, "BSP lump offset")?;
            let len = read_i32(bytes, header + 4, "BSP lump length")?;
            if offset < 0 || len < 0 {
                return Err(CookError::new(format!("negative {kind:?} BSP lump")));
            }
            if let Some(record_size) = kind.record_size() {
                if len as usize % record_size != 0 {
                    return Err(CookError::new(format!(
                        "{kind:?} BSP lump is not aligned to {record_size} bytes"
                    )));
                }
            }
            checked_slice(
                bytes,
                offset as usize,
                len as usize,
                &format!("{kind:?} BSP lump"),
            )?;
            lumps[kind as usize] = LumpRange {
                offset: offset as usize,
                len: len as usize,
            };
        }
        let mip_lump = range_slice(bytes, lumps[BspLump::MipTextures as usize]);
        let texture_count = validate_mip_textures(mip_lump)?;
        Ok(Self {
            bytes,
            lumps,
            texture_count,
        })
    }

    pub fn lump(&self, kind: BspLump) -> &'a [u8] {
        range_slice(self.bytes, self.lumps[kind as usize])
    }

    pub fn record_count(&self, kind: BspLump) -> Option<usize> {
        Some(self.lumps[kind as usize].len / kind.record_size()?)
    }

    pub const fn texture_count(&self) -> usize {
        self.texture_count
    }

    pub fn stats(&self) -> BspStats {
        BspStats {
            planes: self.record_count(BspLump::Planes).unwrap(),
            textures: self.texture_count,
            vertices: self.record_count(BspLump::Vertices).unwrap(),
            nodes: self.record_count(BspLump::Nodes).unwrap(),
            faces: self.record_count(BspLump::Faces).unwrap(),
            clip_nodes: self.record_count(BspLump::ClipNodes).unwrap(),
            leaves: self.record_count(BspLump::Leaves).unwrap(),
            models: self.record_count(BspLump::Models).unwrap(),
        }
    }

    pub fn mip_texture(&self, index: usize) -> Result<Option<MipTexture<'a>>, CookError> {
        if index >= self.texture_count {
            return Err(CookError::new(format!(
                "mip texture index {index} is out of range"
            )));
        }
        let lump = self.lump(BspLump::MipTextures);
        let relative = read_i32(lump, 4 + index * 4, "mip texture offset")?;
        if relative < 0 {
            return Ok(None);
        }
        let header = checked_slice(
            lump,
            relative as usize,
            MIP_TEXTURE_HEADER_BYTES,
            "mip texture header",
        )?;
        let name_len = header[..16]
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(16);
        let name = std::str::from_utf8(&header[..name_len])
            .map_err(|_| CookError::new("mip texture name is not UTF-8"))?;
        let width = read_i32(header, 16, "mip texture width")?;
        let height = read_i32(header, 20, "mip texture height")?;
        if width <= 0 || height <= 0 {
            return Err(CookError::new(format!(
                "mip texture {name} has invalid size"
            )));
        }
        let mut levels = [&[][..]; 4];
        let mut level_width = width as usize;
        let mut level_height = height as usize;
        for (level, destination) in levels.iter_mut().enumerate() {
            let level_offset = read_i32(header, 24 + level * 4, "mip level offset")?;
            if level_offset < MIP_TEXTURE_HEADER_BYTES as i32 {
                return Err(CookError::new(format!(
                    "mip texture {name} has bad level {level}"
                )));
            }
            let len = level_width
                .checked_mul(level_height)
                .ok_or_else(|| CookError::new("mip texture size overflow"))?;
            *destination = checked_slice(
                lump,
                relative as usize + level_offset as usize,
                len,
                &format!("mip texture {name} level {level}"),
            )?;
            level_width = (level_width / 2).max(1);
            level_height = (level_height / 2).max(1);
        }
        Ok(Some(MipTexture {
            name,
            width: width as usize,
            height: height as usize,
            levels,
        }))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MipTexture<'a> {
    pub name: &'a str,
    pub width: usize,
    pub height: usize,
    pub levels: [&'a [u8]; 4],
}

fn validate_mip_textures(bytes: &[u8]) -> Result<usize, CookError> {
    let count = read_i32(bytes, 0, "mip texture count")?;
    if count < 0 || count > 512 {
        return Err(CookError::new(format!("invalid mip texture count {count}")));
    }
    checked_slice(bytes, 4, count as usize * 4, "mip texture offset table")?;
    for index in 0..count as usize {
        let relative = read_i32(bytes, 4 + index * 4, "mip texture offset")?;
        if relative >= 0 {
            checked_slice(
                bytes,
                relative as usize,
                MIP_TEXTURE_HEADER_BYTES,
                "mip texture header",
            )?;
        }
    }
    Ok(count as usize)
}

fn range_slice(bytes: &[u8], range: LumpRange) -> &[u8] {
    &bytes[range.offset..range.offset + range.len]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_bsp() -> Vec<u8> {
        let mut bytes = vec![0u8; BSP_HEADER_BYTES + 4];
        bytes[..4].copy_from_slice(&BSP_VERSION.to_le_bytes());
        for kind in BspLump::ALL {
            let header = 4 + kind as usize * 8;
            bytes[header..header + 4].copy_from_slice(&(BSP_HEADER_BYTES as i32).to_le_bytes());
        }
        let mip_header = 4 + BspLump::MipTextures as usize * 8;
        bytes[mip_header + 4..mip_header + 8].copy_from_slice(&4i32.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_checked_empty_bsp() {
        let bytes = empty_bsp();
        let bsp = Bsp::parse(&bytes).unwrap();
        assert_eq!(bsp.texture_count(), 0);
        assert_eq!(bsp.stats(), BspStats::default());
    }

    #[test]
    fn rejects_misaligned_fixed_record_lump() {
        let mut bytes = empty_bsp();
        let plane_header = 4 + BspLump::Planes as usize * 8;
        bytes[plane_header + 4..plane_header + 8].copy_from_slice(&1i32.to_le_bytes());
        assert!(Bsp::parse(&bytes).is_err());
    }
}
