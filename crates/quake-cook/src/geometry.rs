use std::collections::BTreeMap;

use psx_render_contract::CookedDrawSurface;
use quake_formats::{
    encode_leaf_bound_max, encode_leaf_bound_min, LEAF_BOUNDS_FOOTER_BYTES,
    LEAF_BOUNDS_RECORD_BYTES, LEAF_BOUNDS_TRAILER_MAGIC, LIQUID_DOUBLE_BUFFER_MARKER,
};

use super::{psx_tpage, Bsp, BspLump, CookError, MipTexture};

const TEXTURE_SPECIAL: u8 = 1;
const TEXTURE_LIQUID: u8 = 2;
const TEXTURE_SKY: u8 = 4;
const TEXTURE_INVISIBLE: u8 = 8;
const TEXTURE_ANIMATED: u8 = 16;
const TEXTURE_LARGE: u8 = 32;
const TEXTURE_LAYERED_SKY: u8 = 64;
const TEXTURE_NULL: u8 = 0x80;

const FACE_BACKSIDE: u8 = 1;
const FACE_BAKED_UV: u8 = 2;
const FACE_BAKED_LIGHT: u8 = 4;
const MAX_LIGHT_STYLES: usize = 64;
const NORMAL_LIGHT_STYLE_VALUE: u32 = 12 * 22;

const VRAM_X_START: usize = 320;
const VRAM_PAGE_WORDS: usize = 640;
const VRAM_PAGE_HEIGHT: usize = 256;
const VRAM_PAGE_COUNT: usize = 2;
const MAX_TEXTURE_WIDTH: usize = 64;
const MAX_TEXTURE_HEIGHT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkyEncoding {
    FlattenedLegacy,
    Layered,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GeometryLumps {
    pub texture_data: Vec<u8>,
    pub vertices: Vec<u8>,
    pub planes: Vec<u8>,
    pub texture_info: Vec<u8>,
    pub faces: Vec<u8>,
    pub mark_surfaces: Vec<u8>,
    pub visibility: Vec<u8>,
    pub leaves: Vec<u8>,
    pub nodes: Vec<u8>,
    pub clip_nodes: Vec<u8>,
    pub models: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CookTexture {
    atlas: [u8; 2],
    size: [i16; 2],
    tpage: u16,
    flags: u8,
    animation_total: i8,
    animation_min: i8,
    animation_max: i8,
    animation_next: i8,
    animation_alt: i8,
}

#[derive(Clone, Copy, Debug, Default)]
struct CookVertex {
    position: [i16; 3],
    uv: [u8; 2],
    light: [u8; 4],
}

#[derive(Clone, Copy, Debug)]
struct FaceVertex {
    position: [f32; 3],
    texture_space: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default)]
struct CookFace {
    plane: u16,
    flags: u8,
    first_vertex: u16,
    vertex_count: u8,
    texture: u16,
    styles: [u8; 2],
}

#[derive(Clone, Copy, Debug, Default)]
struct CookLeaf {
    contents: i8,
    /// `-1` = no visibility row (the PSB5 leaf record keeps the source width).
    visibility_offset: i32,
    first_mark_surface: u16,
    mark_surface_count: u16,
    mins: [i16; 3],
    maxs: [i16; 3],
    light: [u8; 2],
    styles: [u8; 2],
}

#[derive(Clone, Copy, Debug)]
struct CookNode {
    plane: u16,
    children: [i16; 2],
}

pub(crate) struct TextureAtlas {
    occupied: Vec<bool>,
    bytes: Vec<u8>,
    max_y: usize,
}

impl TextureAtlas {
    pub(crate) fn new() -> Self {
        Self {
            occupied: vec![false; VRAM_PAGE_COUNT * VRAM_PAGE_WORDS * VRAM_PAGE_HEIGHT],
            bytes: vec![0; VRAM_PAGE_COUNT * VRAM_PAGE_WORDS * VRAM_PAGE_HEIGHT * 2],
            max_y: 0,
        }
    }

    pub(crate) fn fit(
        &mut self,
        pixel_width: usize,
        height: usize,
    ) -> Result<([u8; 2], u16, usize, usize), CookError> {
        self.fit_with_alignment(pixel_width, height, 8, 1)
    }

    fn fit_with_alignment(
        &mut self,
        pixel_width: usize,
        height: usize,
        x_alignment_pixels: usize,
        y_alignment: usize,
    ) -> Result<([u8; 2], u16, usize, usize), CookError> {
        debug_assert!(x_alignment_pixels >= 8 && x_alignment_pixels.is_power_of_two());
        debug_assert!(y_alignment != 0 && y_alignment.is_power_of_two());
        let word_width = pixel_width / 2;
        let x_alignment_words = x_alignment_pixels / 2;
        for page in 0..VRAM_PAGE_COUNT {
            for x in (0..=VRAM_PAGE_WORDS.saturating_sub(word_width)).step_by(x_alignment_words) {
                for y in (0..=VRAM_PAGE_HEIGHT.saturating_sub(height)).step_by(y_alignment) {
                    let within_tpage = x - (x & 0x3c0);
                    if within_tpage + word_width > 128
                        || !self.rectangle_free(page, x, y, word_width, height)
                    {
                        continue;
                    }
                    self.fill_rectangle(page, x, y, word_width, height);
                    let global_x = x + VRAM_X_START;
                    let global_y = y + page * VRAM_PAGE_HEIGHT;
                    let tpage_x = global_x & 0x3c0;
                    return Ok((
                        [
                            (((global_x - tpage_x) << 1) & 0xff) as u8,
                            (global_y & 0xff) as u8,
                        ],
                        psx_tpage(1, 0, global_x, global_y),
                        x,
                        global_y,
                    ));
                }
            }
        }
        Err(CookError::new(format!(
            "VRAM atlas cannot fit {pixel_width}x{height} texture"
        )))
    }

    pub(crate) fn store(
        &mut self,
        x_words: usize,
        y: usize,
        width: usize,
        height: usize,
        pixels: &[u8],
    ) {
        self.max_y = self.max_y.max(y + height);
        for row in 0..height {
            let source = row * width;
            let destination = (y * VRAM_PAGE_WORDS + x_words) * 2 + row * VRAM_PAGE_WORDS * 2;
            self.bytes[destination..destination + width]
                .copy_from_slice(&pixels[source..source + width]);
        }
    }

    fn rectangle_free(&self, page: usize, x: usize, y: usize, width: usize, height: usize) -> bool {
        (y..y + height).all(|row| {
            (x..x + width).all(|column| {
                !self.occupied[(page * VRAM_PAGE_HEIGHT + row) * VRAM_PAGE_WORDS + column]
            })
        })
    }

    fn fill_rectangle(&mut self, page: usize, x: usize, y: usize, width: usize, height: usize) {
        for row in y..y + height {
            for column in x..x + width {
                self.occupied[(page * VRAM_PAGE_HEIGHT + row) * VRAM_PAGE_WORDS + column] = true;
            }
        }
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.bytes.truncate(self.max_y * VRAM_PAGE_WORDS * 2);
        self.bytes
    }
}

pub fn cook_geometry(bsp: &Bsp<'_>, sky: SkyEncoding) -> Result<GeometryLumps, CookError> {
    let (mut geometry, atlas) = cook_geometry_staged(bsp, sky)?;
    geometry.texture_data = atlas.finish();
    Ok(geometry)
}

pub(crate) fn cook_geometry_staged(
    bsp: &Bsp<'_>,
    sky: SkyEncoding,
) -> Result<(GeometryLumps, TextureAtlas), CookError> {
    let (textures, atlas) = cook_textures(bsp, sky)?;
    let planes = cook_planes(bsp)?;
    let (mut vertices, mut faces, face_offsets) = cook_faces(bsp, &textures)?;
    let (nodes, mut leaves) = cook_nodes_and_leaves(bsp)?;
    let mark_surfaces = remap_mark_surfaces(bsp, &face_offsets, &mut leaves)?;
    update_leaf_lighting(&mark_surfaces, &faces, &vertices, &mut leaves)?;
    bake_vertices(&textures, &mut faces, &mut vertices);

    let geometry = GeometryLumps {
        texture_data: Vec::new(),
        vertices: serialize_vertices(&vertices),
        planes,
        texture_info: serialize_textures(&textures),
        faces: serialize_faces(&faces),
        mark_surfaces: serialize_mark_surfaces(&mark_surfaces),
        visibility: serialize_visibility(bsp, &leaves)?,
        leaves: serialize_leaves(&leaves),
        nodes: serialize_nodes(&nodes),
        clip_nodes: cook_clip_nodes(bsp)?,
        models: cook_models(bsp, &face_offsets)?,
    };
    Ok((geometry, atlas))
}

fn cook_textures(
    bsp: &Bsp<'_>,
    sky: SkyEncoding,
) -> Result<(Vec<CookTexture>, TextureAtlas), CookError> {
    let mut source = Vec::with_capacity(bsp.texture_count());
    for index in 0..bsp.texture_count() {
        source.push(bsp.mip_texture(index)?);
    }
    let mut order = (0..source.len()).collect::<Vec<_>>();
    order.sort_by(|&left, &right| match (source[left], source[right]) {
        (None, None) => left.cmp(&right),
        (None, Some(_)) => core::cmp::Ordering::Greater,
        (Some(_), None) => core::cmp::Ordering::Less,
        (Some(a), Some(b)) => {
            let (aw, ah) = sort_mip_dimensions(a);
            let (bw, bh) = sort_mip_dimensions(b);
            bh.cmp(&ah)
                .then_with(|| bw.cmp(&aw))
                .then_with(|| left.cmp(&right))
        }
    });

    let mut textures = vec![CookTexture::default(); source.len()];
    let mut atlas = TextureAtlas::new();
    for index in order {
        let Some(texture) = source[index] else {
            textures[index].flags = TEXTURE_NULL | TEXTURE_INVISIBLE | TEXTURE_SPECIAL;
            continue;
        };
        let mut flags = texture_flags(texture.name);
        if flags & TEXTURE_SKY != 0 && sky == SkyEncoding::Layered {
            flags |= TEXTURE_LAYERED_SKY;
        }
        if flags & TEXTURE_INVISIBLE != 0 {
            textures[index].flags = flags;
            continue;
        }
        let (level, width, height) = selected_mip(texture, flags);
        let (atlas_uv, tpage, x, y) = if flags & TEXTURE_SKY != 0 {
            atlas.fit_with_alignment(width, height, width / 2, height)?
        } else {
            atlas.fit(width, height)?
        };
        let mut pixels = texture.levels[level].to_vec();
        if flags & TEXTURE_SKY != 0 {
            let half_width = width / 2;
            match sky {
                SkyEncoding::FlattenedLegacy => {
                    for row in pixels.chunks_exact_mut(width) {
                        for column in 0..half_width {
                            let color = if row[column] != 0 {
                                row[column]
                            } else {
                                row[column + half_width]
                            };
                            row[column] = color;
                            row[column + half_width] = color;
                        }
                    }
                }
                SkyEncoding::Layered => {
                    for row in pixels.chunks_exact_mut(width) {
                        for foreground in &mut row[..half_width] {
                            if *foreground == 0 {
                                *foreground = 0xff;
                            }
                        }
                    }
                }
            }
        }
        atlas.store(x, y, width, height, &pixels);
        let mut cooked = CookTexture {
            atlas: atlas_uv,
            size: [(width / 2) as i16, height as i16],
            tpage,
            flags,
            animation_next: -1,
            animation_alt: -1,
            ..CookTexture::default()
        };
        if flags & TEXTURE_LIQUID != 0 {
            let (alternate_uv, alternate_tpage, alternate_x, alternate_y) =
                atlas.fit(width, height)?;
            atlas.store(alternate_x, alternate_y, width, height, &pixels);
            cooked.animation_total = LIQUID_DOUBLE_BUFFER_MARKER;
            cooked.animation_min = alternate_uv[0] as i8;
            cooked.animation_max = alternate_uv[1] as i8;
            cooked.animation_next = alternate_tpage as u8 as i8;
            cooked.animation_alt = (alternate_tpage >> 8) as u8 as i8;
        }
        textures[index] = cooked;
    }
    sequence_animations(&source, &mut textures)?;
    Ok((textures, atlas))
}

fn sort_mip_dimensions(texture: MipTexture<'_>) -> (usize, usize) {
    let mut width = texture.width;
    let mut height = texture.height;
    let mut level = 0;
    while (width > MAX_TEXTURE_WIDTH || height > MAX_TEXTURE_HEIGHT)
        && width > 2
        && height > 1
        && level < 3
    {
        level += 1;
        width >>= 1;
        height >>= 1;
    }
    (width, height)
}

fn selected_mip(texture: MipTexture<'_>, flags: u8) -> (usize, usize, usize) {
    // Layered skies retain their original 256x128 layout: Quake stores the
    // foreground and background as adjacent 128x128 layers.
    let maximum = if flags & (TEXTURE_LARGE | TEXTURE_SKY) != 0 {
        256
    } else {
        MAX_TEXTURE_WIDTH
    };
    let maximum_height = if flags & (TEXTURE_LARGE | TEXTURE_SKY) != 0 {
        256
    } else {
        MAX_TEXTURE_HEIGHT
    };
    let mut width = texture.width;
    let mut height = texture.height;
    let mut level = 0;
    while (width > maximum || height > maximum_height) && width > 2 && height > 1 && level < 3 {
        level += 1;
        width >>= 1;
        height >>= 1;
    }
    (level, width, height)
}

fn texture_flags(name: &str) -> u8 {
    if name.starts_with('*') {
        TEXTURE_SPECIAL | TEXTURE_LIQUID
    } else if name.starts_with('+') {
        TEXTURE_ANIMATED
    } else if name.starts_with("sky") {
        TEXTURE_SPECIAL | TEXTURE_SKY
    } else if name.starts_with("clip") || name.starts_with("trigger") {
        TEXTURE_SPECIAL | TEXTURE_INVISIBLE
    } else if name == "quake" {
        TEXTURE_LARGE
    } else {
        0
    }
}

fn sequence_animations(
    source: &[Option<MipTexture<'_>>],
    textures: &mut [CookTexture],
) -> Result<(), CookError> {
    for first in 0..source.len() {
        let Some(texture) = source[first] else {
            continue;
        };
        let bytes = texture.name.as_bytes();
        if bytes.first() != Some(&b'+') || textures[first].animation_next >= 0 || bytes.len() < 2 {
            continue;
        }
        let suffix = &texture.name[2..];
        let mut normal = [None; 10];
        let mut alternate = [None; 10];
        for (index, candidate) in source.iter().enumerate() {
            let Some(candidate) = candidate else { continue };
            let candidate_bytes = candidate.name.as_bytes();
            if candidate_bytes.len() < 2
                || candidate_bytes[0] != b'+'
                || &candidate.name[2..] != suffix
            {
                continue;
            }
            let marker = candidate_bytes[1].to_ascii_uppercase();
            if marker.is_ascii_digit() {
                normal[(marker - b'0') as usize] = Some(index);
            } else if (b'A'..=b'J').contains(&marker) {
                alternate[(marker - b'A') as usize] = Some(index);
            } else {
                return Err(CookError::new(format!(
                    "bad animated texture {}",
                    candidate.name
                )));
            }
        }
        link_animation_sequence(textures, &normal)?;
        link_animation_sequence(textures, &alternate)?;
        let normal_first = normal.iter().flatten().next().copied();
        let alternate_first = alternate.iter().flatten().next().copied();
        for index in normal.iter().flatten().copied() {
            textures[index].animation_alt = alternate_first.map(|v| v as i8).unwrap_or(-1);
        }
        for index in alternate.iter().flatten().copied() {
            textures[index].animation_alt = normal_first.map(|v| v as i8).unwrap_or(-1);
        }
    }
    Ok(())
}

fn link_animation_sequence(
    textures: &mut [CookTexture],
    sequence: &[Option<usize>; 10],
) -> Result<(), CookError> {
    let count = sequence
        .iter()
        .rposition(Option::is_some)
        .map(|v| v + 1)
        .unwrap_or(0);
    if count == 0 {
        return Ok(());
    }
    if sequence[..count].iter().any(Option::is_none) {
        return Err(CookError::new("animated texture sequence has a gap"));
    }
    for frame in 0..count {
        let index = sequence[frame].unwrap();
        textures[index].animation_total = (count * 2) as i8;
        textures[index].animation_min = (frame * 2) as i8;
        textures[index].animation_max = ((frame + 1) * 2) as i8;
        textures[index].animation_next = sequence[(frame + 1) % count].unwrap() as i8;
    }
    Ok(())
}

fn cook_planes(bsp: &Bsp<'_>) -> Result<Vec<u8>, CookError> {
    let input = bsp.lump(BspLump::Planes);
    let mut output = Vec::with_capacity(input.len() / 20 * 14);
    for plane in input.chunks_exact(20) {
        for axis in 0..3 {
            output.extend_from_slice(&float_to_fixed16(f32_at(plane, axis * 4)?).to_le_bytes());
        }
        output.extend_from_slice(&float_to_fixed32(f32_at(plane, 12)?).to_le_bytes());
        // Retain Quake's authored plane class. The derived-kind experiment
        // was not visually equivalent on the real-MIPS owner-camera gate.
        output.extend_from_slice(&i32_at(plane, 16)?.to_le_bytes());
    }
    Ok(output)
}

fn cook_faces(
    bsp: &Bsp<'_>,
    textures: &[CookTexture],
) -> Result<(Vec<CookVertex>, Vec<CookFace>, Vec<usize>), CookError> {
    let face_bytes = bsp.lump(BspLump::Faces);
    let mut output_vertices = Vec::new();
    let mut output_faces = Vec::with_capacity(face_bytes.len() / 20);
    let mut face_offsets = Vec::with_capacity(face_bytes.len() / 20 + 1);
    face_offsets.push(0);
    for source_face in face_bytes.chunks_exact(20) {
        let plane = compact_face_index(u16_at(source_face, 0)? as usize, "plane")?;
        let side = i16_at(source_face, 2)?;
        let first_edge = nonnegative(i32_at(source_face, 4)?, "face first edge")?;
        let source_vertex_count = nonnegative(i16_at(source_face, 8)? as i32, "face edge count")?;
        if source_vertex_count < 3 {
            return Err(CookError::new("BSP face has fewer than three vertices"));
        }
        if source_vertex_count > 39 {
            return Err(CookError::new(
                "BSP face exceeds PSB3's 39-vertex packet limit",
            ));
        }
        let texture_info_index = nonnegative(i16_at(source_face, 10)? as i32, "face texture info")?;
        let texture_info = record(
            bsp.lump(BspLump::TextureInfo),
            40,
            texture_info_index,
            "texture info",
        )?;
        let texture_index = nonnegative(i32_at(texture_info, 32)?, "mip texture index")?;
        let cooked_texture = *textures
            .get(texture_index)
            .ok_or_else(|| CookError::new("face mip texture index is out of range"))?;
        let source_texture = bsp.mip_texture(texture_index)?;
        let texture_size = source_texture
            .map(|texture| [texture.width as f32, texture.height as f32])
            .unwrap_or([1.0, 1.0]);

        let mut source_polygon = Vec::with_capacity(source_vertex_count);
        for edge_offset in 0..source_vertex_count {
            let surface_edge =
                record_i32(bsp.lump(BspLump::SurfaceEdges), first_edge + edge_offset)?;
            let edge_index = surface_edge.unsigned_abs() as usize;
            let edge = record(bsp.lump(BspLump::Edges), 4, edge_index, "edge")?;
            let vertex_index = if surface_edge >= 0 {
                u16_at(edge, 0)? as usize
            } else {
                u16_at(edge, 2)? as usize
            };
            let vertex = record(bsp.lump(BspLump::Vertices), 12, vertex_index, "vertex")?;
            let position = [f32_at(vertex, 0)?, f32_at(vertex, 4)?, f32_at(vertex, 8)?];
            let mut texture_space = [0.0; 2];
            for axis in 0..2 {
                let base = axis * 16;
                texture_space[axis] = dot3_host(
                    position,
                    [
                        f32_at(texture_info, base)?,
                        f32_at(texture_info, base + 4)?,
                        f32_at(texture_info, base + 8)?,
                    ],
                ) + f32_at(texture_info, base + 12)?;
            }
            source_polygon.push(FaceVertex {
                position,
                texture_space,
            });
        }

        let polygons = if cooked_texture.flags & TEXTURE_LIQUID != 0 {
            subdivide_liquid_polygon(source_polygon, 128.0)
        } else {
            vec![source_polygon]
        };

        for polygon in polygons {
            let vertex_count = polygon.len();
            if vertex_count < 3 {
                continue;
            }
            if vertex_count > 39 {
                return Err(CookError::new(
                    "subdivided liquid face exceeds PSB3's 39-vertex packet limit",
                ));
            }
            let mut st_min = [f32::MAX; 2];
            let mut st_max = [f32::MIN; 2];
            for vertex in &polygon {
                for axis in 0..2 {
                    st_min[axis] = st_min[axis].min(vertex.texture_space[axis]);
                    st_max[axis] = st_max[axis].max(vertex.texture_space[axis]);
                }
            }
            let mut uv_min = [0.0; 2];
            let mut uv_size = [0.0; 2];
            let mut uv_max = [0.0; 2];
            let mut liquid_uv_base = [0.0; 2];
            let mut light_min = [0.0; 2];
            let mut light_size = [0.0; 2];
            for axis in 0..2 {
                uv_min[axis] = signed_fraction(st_min[axis] / texture_size[axis]);
                uv_size[axis] = (st_max[axis] - st_min[axis]) / texture_size[axis];
                uv_max[axis] = uv_min[axis] + uv_size[axis];
                let cooked_extent = if axis == 0 {
                    cooked_texture.size[0] as f32 * 2.0
                } else {
                    cooked_texture.size[1] as f32
                };
                let liquid_min = st_min[axis] * cooked_extent / texture_size[axis];
                let atlas = cooked_texture.atlas[axis] as f32;
                liquid_uv_base[axis] = ((liquid_min + atlas) / 64.0).floor() * 64.0;
                let minimum = (st_min[axis] / 16.0).floor();
                let maximum = (st_max[axis] / 16.0).ceil();
                light_min[axis] = minimum * 16.0;
                light_size[axis] = (maximum - minimum) * 16.0;
            }

            let first_vertex = output_vertices.len();
            let mut light = vec![[0u16; 4]; vertex_count];
            let mut light_accumulator = [0u32; 4];
            for (index, vertex) in polygon.iter().enumerate() {
                let mut face_uv = [0.0; 2];
                for axis in 0..2 {
                    if cooked_texture.flags & TEXTURE_LIQUID != 0 {
                        // Quake's turbulent drawer consumes continuous authored
                        // coordinates and repeats them every 64 texels.  The
                        // polygon has already been clipped to a 128-texel cell,
                        // so a whole-tile shift preserves continuity without a
                        // u8 interpolation wrap inside any PS1 packet.
                        let cooked_extent = if axis == 0 {
                            cooked_texture.size[0] as f32 * 2.0
                        } else {
                            cooked_texture.size[1] as f32
                        };
                        let authored =
                            vertex.texture_space[axis] * cooked_extent / texture_size[axis];
                        face_uv[axis] = authored - liquid_uv_base[axis];
                    } else {
                        let delta =
                            (vertex.texture_space[axis] - st_min[axis]) / texture_size[axis];
                        face_uv[axis] = if uv_min[axis] >= -1.0 && uv_max[axis] <= 0.0 {
                            1.0 + uv_min[axis] + delta
                        } else if uv_min[axis] >= 0.0 && uv_max[axis] <= 1.0 {
                            uv_min[axis] + delta
                        } else if uv_size[axis] <= 1.0 {
                            delta
                        } else {
                            delta / uv_size[axis]
                        };
                    }
                }
                sample_vertex_light(
                    bsp,
                    source_face,
                    texture_info,
                    source_texture,
                    vertex.position,
                    light_min,
                    light_size,
                    &mut light[index],
                )?;
                for style in 0..4 {
                    light_accumulator[style] += light[index][style] as u32;
                }
                output_vertices.push(CookVertex {
                    position: vertex.position.map(float_to_position),
                    uv: [
                        if cooked_texture.flags & TEXTURE_LIQUID != 0 {
                            float_to_u8(face_uv[0])
                        } else {
                            float_to_u8(face_uv[0] * 2.0 * (cooked_texture.size[0] - 1) as f32)
                        },
                        if cooked_texture.flags & TEXTURE_LIQUID != 0 {
                            float_to_u8(face_uv[1])
                        } else {
                            float_to_u8(face_uv[1] * (cooked_texture.size[1] - 1) as f32)
                        },
                    ],
                    light: [0; 4],
                });
            }
            if output_vertices.len() > u16::MAX as usize {
                return Err(CookError::new(
                    "cooked vertex table exceeds PSB3 u16 address space",
                ));
            }
            let mut order = [0usize, 1, 2, 3];
            compare_swap_desc(&mut order, 0, 1, &light_accumulator);
            compare_swap_desc(&mut order, 2, 3, &light_accumulator);
            compare_swap_desc(&mut order, 0, 2, &light_accumulator);
            compare_swap_desc(&mut order, 1, 3, &light_accumulator);
            compare_swap_desc(&mut order, 1, 2, &light_accumulator);
            let texture_flags = i32_at(texture_info, 36)?;
            let styles = if texture_flags & 7 != 0 {
                [0, 0]
            } else {
                [
                    source_face[12 + order[0]].min(MAX_LIGHT_STYLES as u8),
                    source_face[12 + order[1]].min(MAX_LIGHT_STYLES as u8),
                ]
            };
            for vertex in 0..vertex_count {
                output_vertices[first_vertex + vertex].light[0] = light[vertex][order[0]] as u8;
                output_vertices[first_vertex + vertex].light[1] = light[vertex][order[1]] as u8;
            }
            let first_vertex = u16::try_from(first_vertex)
                .map_err(|_| CookError::new("cooked face first vertex exceeds u16"))?;
            let vertex_count = u8::try_from(vertex_count)
                .map_err(|_| CookError::new("cooked face vertex count exceeds u8"))?;
            let texture = compact_face_index(texture_index, "texture")?;
            output_faces.push(CookFace {
                plane,
                flags: if side != 0 { FACE_BACKSIDE } else { 0 },
                first_vertex,
                vertex_count,
                texture,
                styles,
            });
        }
        face_offsets.push(output_faces.len());
    }
    Ok((output_vertices, output_faces, face_offsets))
}

fn subdivide_liquid_polygon(polygon: Vec<FaceVertex>, cell_size: f32) -> Vec<Vec<FaceVertex>> {
    let mut polygons = vec![polygon];
    for axis in 0..2 {
        let minimum = polygons[0]
            .iter()
            .map(|vertex| vertex.texture_space[axis])
            .fold(f32::MAX, f32::min);
        let maximum = polygons[0]
            .iter()
            .map(|vertex| vertex.texture_space[axis])
            .fold(f32::MIN, f32::max);
        let mut boundary = (minimum / cell_size).floor() * cell_size + cell_size;
        while boundary < maximum - 0.001 {
            let mut split = Vec::with_capacity(polygons.len() * 2);
            for current in polygons {
                let lower = clip_polygon_axis(&current, axis, boundary, true);
                let upper = clip_polygon_axis(&current, axis, boundary, false);
                if lower.len() >= 3 {
                    split.push(lower);
                }
                if upper.len() >= 3 {
                    split.push(upper);
                }
            }
            polygons = split;
            boundary += cell_size;
        }
    }
    polygons
}

fn clip_polygon_axis(
    polygon: &[FaceVertex],
    axis: usize,
    boundary: f32,
    keep_lower: bool,
) -> Vec<FaceVertex> {
    let mut output = Vec::with_capacity(polygon.len() + 1);
    let Some(mut previous) = polygon.last().copied() else {
        return output;
    };
    let mut previous_inside = if keep_lower {
        previous.texture_space[axis] <= boundary
    } else {
        previous.texture_space[axis] >= boundary
    };
    for &current in polygon {
        let current_inside = if keep_lower {
            current.texture_space[axis] <= boundary
        } else {
            current.texture_space[axis] >= boundary
        };
        if current_inside != previous_inside {
            let denominator = current.texture_space[axis] - previous.texture_space[axis];
            if denominator.abs() > f32::EPSILON {
                let amount = (boundary - previous.texture_space[axis]) / denominator;
                output.push(interpolate_face_vertex(previous, current, amount));
            }
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    output
}

fn interpolate_face_vertex(a: FaceVertex, b: FaceVertex, amount: f32) -> FaceVertex {
    let mut output = a;
    for axis in 0..3 {
        output.position[axis] = a.position[axis] + (b.position[axis] - a.position[axis]) * amount;
    }
    for axis in 0..2 {
        output.texture_space[axis] =
            a.texture_space[axis] + (b.texture_space[axis] - a.texture_space[axis]) * amount;
    }
    output
}

fn sample_vertex_light(
    bsp: &Bsp<'_>,
    face: &[u8],
    texture_info: &[u8],
    texture: Option<MipTexture<'_>>,
    position: [f32; 3],
    light_min: [f32; 2],
    light_size: [f32; 2],
    output: &mut [u16; 4],
) -> Result<(), CookError> {
    if texture
        .map(|texture| {
            texture.name.starts_with('+')
                || texture.name.contains("*lava")
                || texture.name.contains("*tele")
        })
        .unwrap_or(false)
    {
        *output = [0x60; 4];
        return Ok(());
    }
    if i32_at(texture_info, 36)? & 7 != 0 {
        *output = [0x40; 4];
        return Ok(());
    }
    let light_offset = i32_at(face, 16)?;
    if light_offset < 0 {
        *output = [0; 4];
        return Ok(());
    }
    let lighting = bsp.lump(BspLump::Lighting);
    let mut coordinate = [0i32; 2];
    for axis in 0..2 {
        let base = axis * 16;
        coordinate[axis] = (dot3_host(
            position,
            [
                f32_at(texture_info, base)?,
                f32_at(texture_info, base + 4)?,
                f32_at(texture_info, base + 8)?,
            ],
        ) + f32_at(texture_info, base + 12)?) as i32;
    }
    let mut ds = (coordinate[0] - light_min[0] as i32).clamp(0, light_size[0] as i32);
    let mut dt = (coordinate[1] - light_min[1] as i32).clamp(0, light_size[1] as i32);
    ds >>= 4;
    dt >>= 4;
    let width = (light_size[0] as usize >> 4) + 1;
    let height = (light_size[1] as usize >> 4) + 1;
    let style_pixels = width * height;
    let mut sample = light_offset as usize + dt as usize * width + ds as usize;
    for style in 0..4 {
        if face[12 + style] == 0xff {
            break;
        }
        let value = *lighting
            .get(sample)
            .ok_or_else(|| CookError::new("face lightmap sample is out of bounds"))?
            as u32
            * 255;
        output[style] = (value >> 8) as u16;
        sample += style_pixels;
    }
    Ok(())
}

fn compare_swap_desc(order: &mut [usize; 4], a: usize, b: usize, values: &[u32; 4]) {
    if values[order[a]] < values[order[b]] {
        order.swap(a, b);
    }
}

fn cook_nodes_and_leaves(bsp: &Bsp<'_>) -> Result<(Vec<CookNode>, Vec<CookLeaf>), CookError> {
    let mut nodes = Vec::new();
    for source in bsp.lump(BspLump::Nodes).chunks_exact(24) {
        let plane = u16::try_from(i32_at(source, 0)?)
            .map_err(|_| CookError::new("BSP node plane exceeds u16"))?;
        nodes.push(CookNode {
            plane,
            children: [i16_at(source, 4)?, i16_at(source, 6)?],
        });
    }

    let mut leaves = Vec::new();
    for source in bsp.lump(BspLump::Leaves).chunks_exact(28) {
        let contents = i8::try_from(i32_at(source, 0)?)
            .map_err(|_| CookError::new("BSP leaf contents exceeds i8"))?;
        let visibility_offset = i32_at(source, 4)?;
        if visibility_offset < -1 {
            return Err(CookError::new("BSP leaf visibility offset is negative"));
        }
        let mark_surface_count = u16_at(source, 22)?;
        leaves.push(CookLeaf {
            contents,
            visibility_offset,
            first_mark_surface: u16_at(source, 20)?,
            mark_surface_count,
            mins: [
                u16_at(source, 8)? as i16,
                u16_at(source, 10)? as i16,
                u16_at(source, 12)? as i16,
            ],
            maxs: [
                u16_at(source, 14)? as i16,
                u16_at(source, 16)? as i16,
                u16_at(source, 18)? as i16,
            ],
            light: [0; 2],
            styles: [MAX_LIGHT_STYLES as u8; 2],
        });
    }
    Ok((nodes, leaves))
}

fn remap_mark_surfaces(
    bsp: &Bsp<'_>,
    face_offsets: &[usize],
    leaves: &mut [CookLeaf],
) -> Result<Vec<u16>, CookError> {
    let source_marks = bsp.lump(BspLump::MarkSurfaces);
    let mut output = Vec::new();
    for leaf in leaves {
        let source_first = leaf.first_mark_surface as usize;
        let source_count = leaf.mark_surface_count as usize;
        let output_first = output.len();
        for mark in source_first..source_first + source_count {
            let source_face = record_u16(source_marks, mark)? as usize;
            let first = *face_offsets
                .get(source_face)
                .ok_or_else(|| CookError::new("mark surface face is out of bounds"))?;
            let end = *face_offsets
                .get(source_face + 1)
                .ok_or_else(|| CookError::new("mark surface face end is out of bounds"))?;
            for cooked_face in first..end {
                output.push(
                    u16::try_from(cooked_face)
                        .map_err(|_| CookError::new("remapped mark surface face exceeds u16"))?,
                );
            }
        }
        leaf.first_mark_surface = u16::try_from(output_first)
            .map_err(|_| CookError::new("remapped leaf first mark surface exceeds u16"))?;
        leaf.mark_surface_count = u16::try_from(output.len() - output_first)
            .map_err(|_| CookError::new("remapped leaf mark-surface count exceeds u16"))?;
    }
    Ok(output)
}

fn update_leaf_lighting(
    marks: &[u16],
    faces: &[CookFace],
    vertices: &[CookVertex],
    leaves: &mut [CookLeaf],
) -> Result<(), CookError> {
    for leaf in leaves {
        let mut best_face = None;
        let mut best_light = 0u32;
        for mark in leaf.first_mark_surface as usize
            ..leaf.first_mark_surface as usize + leaf.mark_surface_count as usize
        {
            let face_index = *marks
                .get(mark)
                .ok_or_else(|| CookError::new("leaf mark surface is out of bounds"))?
                as usize;
            let face = *faces
                .get(face_index)
                .ok_or_else(|| CookError::new("leaf face index is out of bounds"))?;
            let mut light = 0u32;
            for vertex in
                face.first_vertex as usize..face.first_vertex as usize + face.vertex_count as usize
            {
                let contribution = vertices
                    .get(vertex)
                    .ok_or_else(|| CookError::new("leaf face vertex is out of bounds"))?
                    .light;
                light += contribution[0] as u32 + contribution[1] as u32;
            }
            light /= face.vertex_count as u32;
            if light > best_light {
                best_light = light;
                best_face = Some(face);
            }
        }
        let Some(face) = best_face else { continue };
        let mut average = [0u32; 2];
        for vertex in
            face.first_vertex as usize..face.first_vertex as usize + face.vertex_count as usize
        {
            let contribution = vertices[vertex].light;
            average[0] += contribution[0] as u32;
            average[1] += contribution[1] as u32;
        }
        leaf.styles = face.styles;
        leaf.light = [
            (average[0] / face.vertex_count as u32).min(255) as u8,
            (average[1] / face.vertex_count as u32).min(255) as u8,
        ];
    }
    Ok(())
}

fn bake_vertices(textures: &[CookTexture], faces: &mut [CookFace], vertices: &mut [CookVertex]) {
    for face in faces {
        let texture = textures[face.texture as usize];
        let bake_uv = texture.flags & TEXTURE_ANIMATED == 0;
        let bake_light = face.styles[0] == 0 && (face.styles[1] == 0 || face.styles[1] == 64);
        if bake_uv {
            face.flags |= FACE_BAKED_UV;
        }
        if bake_light {
            face.flags |= FACE_BAKED_LIGHT;
        }
        for vertex in &mut vertices
            [face.first_vertex as usize..face.first_vertex as usize + face.vertex_count as usize]
        {
            if bake_uv {
                vertex.uv[0] = vertex.uv[0].wrapping_add(texture.atlas[0]);
                vertex.uv[1] = vertex.uv[1].wrapping_add(texture.atlas[1]);
            }
            if bake_light {
                let mut value = vertex.light[0] as u32 * NORMAL_LIGHT_STYLE_VALUE;
                if face.styles[1] == 0 {
                    value += vertex.light[1] as u32 * NORMAL_LIGHT_STYLE_VALUE;
                }
                value >>= 8;
                let packed = value | (value << 8) | (value << 16);
                vertex.light = packed.to_le_bytes();
            }
        }
    }
}

fn cook_clip_nodes(bsp: &Bsp<'_>) -> Result<Vec<u8>, CookError> {
    let mut output = Vec::with_capacity(bsp.lump(BspLump::ClipNodes).len() / 8 * 6);
    for node in bsp.lump(BspLump::ClipNodes).chunks_exact(8) {
        output.extend_from_slice(&(i32_at(node, 0)? as i16).to_le_bytes());
        output.extend_from_slice(&i16_at(node, 4)?.to_le_bytes());
        output.extend_from_slice(&i16_at(node, 6)?.to_le_bytes());
    }
    Ok(output)
}

fn cook_models(bsp: &Bsp<'_>, face_offsets: &[usize]) -> Result<Vec<u8>, CookError> {
    let mut output = Vec::with_capacity(bsp.lump(BspLump::Models).len() / 64 * 32);
    for model in bsp.lump(BspLump::Models).chunks_exact(64) {
        // mins, maxs, then the model origin (always zero in Quake's BSPs; the
        // PSB5 record carries it for the editor's brush worlds).
        for offset in [0, 4, 8, 12, 16, 20, 24, 28, 32] {
            output.extend_from_slice(&(f32_at(model, offset)? as i16).to_le_bytes());
        }
        for hull in 0..4 {
            output.extend_from_slice(&(i32_at(model, 36 + hull * 4)? as i16).to_le_bytes());
        }
        output.extend_from_slice(&(i32_at(model, 52)? as i16).to_le_bytes());
        let source_first = nonnegative(i32_at(model, 56)?, "model first face")?;
        let source_count = nonnegative(i32_at(model, 60)?, "model face count")?;
        let cooked_first = *face_offsets
            .get(source_first)
            .ok_or_else(|| CookError::new("model first face is out of bounds"))?;
        let cooked_end = *face_offsets
            .get(source_first + source_count)
            .ok_or_else(|| CookError::new("model face end is out of bounds"))?;
        output.extend_from_slice(
            &u16::try_from(cooked_first)
                .map_err(|_| CookError::new("model first cooked face exceeds u16"))?
                .to_le_bytes(),
        );
        output.extend_from_slice(
            &u16::try_from(cooked_end - cooked_first)
                .map_err(|_| CookError::new("model cooked face count exceeds u16"))?
                .to_le_bytes(),
        );
    }
    Ok(output)
}

fn serialize_textures(textures: &[CookTexture]) -> Vec<u8> {
    let mut output = Vec::with_capacity(textures.len() * 14);
    for texture in textures {
        output.extend_from_slice(&texture.atlas);
        output.extend_from_slice(&texture.size[0].to_le_bytes());
        output.extend_from_slice(&texture.size[1].to_le_bytes());
        output.extend_from_slice(&texture.tpage.to_le_bytes());
        output.extend_from_slice(&[
            texture.flags,
            texture.animation_total as u8,
            texture.animation_min as u8,
            texture.animation_max as u8,
            texture.animation_next as u8,
            texture.animation_alt as u8,
        ]);
    }
    output
}

fn serialize_vertices(vertices: &[CookVertex]) -> Vec<u8> {
    const INDEXED_VERTEX_MAGIC: u32 = 0x3158_5649; // `IVX1`
    let corner_count =
        u16::try_from(vertices.len()).expect("validated cooked corner count fits the PSB4 header");
    let mut position_indices = Vec::with_capacity(vertices.len());
    let mut positions = Vec::<[i16; 3]>::new();
    let mut by_position = BTreeMap::<[i16; 3], u16>::new();
    for vertex in vertices {
        let position_index = if let Some(&index) = by_position.get(&vertex.position) {
            index
        } else {
            let index = u16::try_from(positions.len())
                .expect("validated cooked position count fits the PSB4 header");
            positions.push(vertex.position);
            by_position.insert(vertex.position, index);
            index
        };
        position_indices.push(position_index);
    }
    let position_count = u16::try_from(positions.len())
        .expect("validated cooked position count fits the PSB4 header");
    let mut output = Vec::with_capacity(8 + vertices.len() * 8 + positions.len() * 6);
    output.extend_from_slice(&INDEXED_VERTEX_MAGIC.to_le_bytes());
    output.extend_from_slice(&corner_count.to_le_bytes());
    output.extend_from_slice(&position_count.to_le_bytes());
    for (vertex, position_index) in vertices.iter().zip(position_indices) {
        output.extend_from_slice(&position_index.to_le_bytes());
        output.extend_from_slice(&vertex.uv);
        output.extend_from_slice(&vertex.light);
    }
    for position in positions {
        for value in position {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    output
}

fn serialize_faces(faces: &[CookFace]) -> Vec<u8> {
    let mut output = Vec::with_capacity(faces.len() * CookedDrawSurface::SIZE);
    for face in faces {
        output.extend_from_slice(
            &CookedDrawSurface {
                plane: face.plane,
                first_corner: face.first_vertex,
                material: face.texture,
                flags: face.flags,
                corner_count: face.vertex_count,
                light_styles: face.styles,
            }
            .encode(),
        );
    }
    output
}

fn serialize_mark_surfaces(marks: &[u16]) -> Vec<u8> {
    let mut output = Vec::with_capacity(marks.len() * 2);
    for mark in marks {
        output.extend_from_slice(&mark.to_le_bytes());
    }
    output
}

/// Preserve the source PVS byte-for-byte and append one outward-quantized
/// camera-cell AABB per leaf. PVS offsets continue to address the unchanged
/// prefix; the fixed footer makes the optional sidecar discoverable without
/// changing the shared PSB5 leaf record.
fn serialize_visibility(bsp: &Bsp<'_>, leaves: &[CookLeaf]) -> Result<Vec<u8>, CookError> {
    let count = u16::try_from(leaves.len())
        .map_err(|_| CookError::new("leaf-bounds sidecar exceeds u16"))?;
    let mut output = Vec::with_capacity(
        bsp.lump(BspLump::Visibility).len()
            + leaves.len() * LEAF_BOUNDS_RECORD_BYTES
            + LEAF_BOUNDS_FOOTER_BYTES,
    );
    output.extend_from_slice(bsp.lump(BspLump::Visibility));
    for leaf in leaves {
        output.extend(
            leaf.mins
                .map(encode_leaf_bound_min)
                .map(|value| value as u8),
        );
        output.extend(
            leaf.maxs
                .map(encode_leaf_bound_max)
                .map(|value| value as u8),
        );
    }
    output.extend_from_slice(&LEAF_BOUNDS_TRAILER_MAGIC.to_le_bytes());
    output.extend_from_slice(&count.to_le_bytes());
    output.extend_from_slice(&(LEAF_BOUNDS_RECORD_BYTES as u16).to_le_bytes());
    Ok(output)
}

/// PSB5 leaf: contents i8, pad, mark count u16, visibility offset i32,
/// first mark u16, lightmap, light styles (14 bytes).
fn serialize_leaves(leaves: &[CookLeaf]) -> Vec<u8> {
    let mut output = Vec::with_capacity(leaves.len() * 14);
    for leaf in leaves {
        output.push(leaf.contents as u8);
        output.push(0);
        output.extend_from_slice(&leaf.mark_surface_count.to_le_bytes());
        output.extend_from_slice(&leaf.visibility_offset.to_le_bytes());
        output.extend_from_slice(&leaf.first_mark_surface.to_le_bytes());
        output.extend_from_slice(&leaf.light);
        output.extend_from_slice(&leaf.styles);
    }
    output
}

fn serialize_nodes(nodes: &[CookNode]) -> Vec<u8> {
    let mut output = Vec::with_capacity(nodes.len() * 6);
    for node in nodes {
        output.extend_from_slice(&node.plane.to_le_bytes());
        for value in node.children {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    output
}

fn record<'a>(
    bytes: &'a [u8],
    size: usize,
    index: usize,
    context: &str,
) -> Result<&'a [u8], CookError> {
    let start = index
        .checked_mul(size)
        .ok_or_else(|| CookError::new(format!("{context} index overflow")))?;
    bytes
        .get(start..start + size)
        .ok_or_else(|| CookError::new(format!("{context} index is out of bounds")))
}

fn record_i32(bytes: &[u8], index: usize) -> Result<i32, CookError> {
    i32_at(record(bytes, 4, index, "i32 record")?, 0)
}

fn record_u16(bytes: &[u8], index: usize) -> Result<u16, CookError> {
    u16_at(record(bytes, 2, index, "u16 record")?, 0)
}

fn nonnegative(value: i32, context: &str) -> Result<usize, CookError> {
    usize::try_from(value).map_err(|_| CookError::new(format!("negative {context}")))
}

fn compact_face_index(value: usize, context: &str) -> Result<u16, CookError> {
    let value = u16::try_from(value)
        .map_err(|_| CookError::new(format!("cooked face {context} index exceeds u16")))?;
    if value > i16::MAX as u16 {
        return Err(CookError::new(format!(
            "cooked face {context} index exceeds the legacy semantic i16 domain"
        )));
    }
    Ok(value)
}

fn i16_at(bytes: &[u8], offset: usize) -> Result<i16, CookError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| CookError::new("truncated i16"))?;
    Ok(i16::from_le_bytes(value.try_into().unwrap()))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, CookError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| CookError::new("truncated u16"))?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn i32_at(bytes: &[u8], offset: usize) -> Result<i32, CookError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| CookError::new("truncated i32"))?;
    Ok(i32::from_le_bytes(value.try_into().unwrap()))
}

fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, CookError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| CookError::new("truncated f32"))?;
    Ok(f32::from_le_bytes(value.try_into().unwrap()))
}

fn float_to_fixed16(value: f32) -> i16 {
    (value * 4096.0) as i16
}

fn float_to_fixed32(value: f32) -> i32 {
    (value * 4096.0) as i32
}

fn float_to_position(value: f32) -> i16 {
    value.round() as i16
}

fn float_to_u8(value: f32) -> u8 {
    (value as i32) as u8
}

fn signed_fraction(value: f32) -> f32 {
    value - value.trunc()
}

fn dot3_host(left: [f32; 3], right: [f32; 3]) -> f32 {
    let xy = left[0].mul_add(right[0], left[1] * right[1]);
    left[2].mul_add(right[2], xy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(width: usize, height: usize) -> MipTexture<'static> {
        MipTexture {
            name: "test",
            width,
            height,
            levels: [&[], &[], &[], &[]],
        }
    }

    #[test]
    fn layered_sky_keeps_full_resolution_pair() {
        assert_eq!(selected_mip(texture(256, 128), TEXTURE_SKY), (0, 256, 128));
    }

    #[test]
    fn ordinary_texture_still_selects_psx_sized_mip() {
        assert_eq!(selected_mip(texture(256, 128), 0), (2, 64, 32));
    }

    #[test]
    fn compact_psb3_records_have_pinned_wire_widths_and_order() {
        let face = CookFace {
            plane: 0x1234,
            flags: 5,
            first_vertex: 0x2345,
            vertex_count: 24,
            texture: 0x3456,
            styles: [7, 8],
        };
        assert_eq!(
            serialize_faces(&[face]),
            [0x34, 0x12, 0x45, 0x23, 0x56, 0x34, 5, 24, 7, 8]
        );

        let leaf = CookLeaf {
            contents: -4,
            visibility_offset: -1,
            first_mark_surface: 0x4567,
            mark_surface_count: 0x0178,
            mins: [-33, -1, 0],
            maxs: [1, 33, i16::MAX],
            light: [9, 10],
            styles: [11, 12],
        };
        assert_eq!(
            serialize_leaves(&[leaf]),
            [0xfc, 0, 0x78, 0x01, 0xff, 0xff, 0xff, 0xff, 0x67, 0x45, 9, 10, 11, 12]
        );

        let node = CookNode {
            plane: 0x5678,
            children: [-1, 0x1234],
        };
        assert_eq!(
            serialize_nodes(&[node]),
            [0x78, 0x56, 0xff, 0xff, 0x34, 0x12]
        );
    }

    #[test]
    fn compact_face_indices_preserve_the_proven_semantic_domain() {
        assert_eq!(
            compact_face_index(i16::MAX as usize, "plane").unwrap(),
            i16::MAX as u16
        );
        assert!(compact_face_index(i16::MAX as usize + 1, "plane").is_err());
        assert!(compact_face_index(u16::MAX as usize + 1, "texture").is_err());
    }

    #[test]
    fn oversized_liquid_polygon_is_clipped_on_shared_128_texel_boundaries() {
        let vertex = |x: f32, y: f32| FaceVertex {
            position: [x, y, 7.0],
            texture_space: [x, y],
        };
        let pieces = subdivide_liquid_polygon(
            vec![
                vertex(0.0, 0.0),
                vertex(300.0, 0.0),
                vertex(300.0, 128.0),
                vertex(0.0, 128.0),
            ],
            128.0,
        );
        assert_eq!(pieces.len(), 3);
        for piece in &pieces {
            assert!(piece.len() >= 3);
            for axis in 0..2 {
                let minimum = piece
                    .iter()
                    .map(|vertex| vertex.texture_space[axis])
                    .fold(f32::MAX, f32::min);
                let maximum = piece
                    .iter()
                    .map(|vertex| vertex.texture_space[axis])
                    .fold(f32::MIN, f32::max);
                assert!(maximum - minimum <= 128.001);
            }
        }
        let boundary_128 = pieces
            .iter()
            .flatten()
            .filter(|vertex| (vertex.texture_space[0] - 128.0).abs() < 0.001)
            .count();
        let boundary_256 = pieces
            .iter()
            .flatten()
            .filter(|vertex| (vertex.texture_space[0] - 256.0).abs() < 0.001)
            .count();
        assert_eq!((boundary_128, boundary_256), (4, 4));
    }

    /// A minimal version-29 BSP whose only content is a mip-texture lump of
    /// 64x64 textures, enough to drive `cook_textures` end to end.
    fn synthetic_bsp(names: &[&str]) -> Vec<u8> {
        const HEADER_BYTES: usize = 4 + 15 * 8;
        const SIDE: usize = 64;
        let table = 4 + names.len() * 4;
        let mut bodies = Vec::new();
        let mut offsets = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            offsets.push((table + bodies.len()) as i32);
            let mut header = [0u8; 40];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[16..20].copy_from_slice(&(SIDE as i32).to_le_bytes());
            header[20..24].copy_from_slice(&(SIDE as i32).to_le_bytes());
            let mut level_offset = 40usize;
            let mut side = SIDE;
            for level in 0..4 {
                let cell = 24 + level * 4;
                header[cell..cell + 4].copy_from_slice(&(level_offset as i32).to_le_bytes());
                level_offset += side * side;
                side /= 2;
            }
            bodies.extend_from_slice(&header);
            let mut side = SIDE;
            for _ in 0..4 {
                bodies.extend((0..side * side).map(|texel| (texel ^ index) as u8));
                side /= 2;
            }
        }
        let mut mip = Vec::with_capacity(table + bodies.len());
        mip.extend_from_slice(&(names.len() as i32).to_le_bytes());
        for offset in &offsets {
            mip.extend_from_slice(&offset.to_le_bytes());
        }
        mip.extend_from_slice(&bodies);

        let mut bsp = vec![0u8; HEADER_BYTES];
        bsp[..4].copy_from_slice(&29i32.to_le_bytes());
        for kind in BspLump::ALL {
            let header = 4 + kind as usize * 8;
            bsp[header..header + 4].copy_from_slice(&(HEADER_BYTES as i32).to_le_bytes());
        }
        let mip_header = 4 + BspLump::MipTextures as usize * 8;
        bsp[mip_header + 4..mip_header + 8].copy_from_slice(&(mip.len() as i32).to_le_bytes());
        bsp.extend_from_slice(&mip);
        bsp
    }

    /// VRAM rectangle in halfword units, same arithmetic as the guest loader.
    fn vram_rect(texture: quake_formats::TextureInfo) -> (usize, usize, usize, usize) {
        let tpage_x = usize::from(texture.texture_page & 0x000f) * 64;
        let tpage_y = usize::from((texture.texture_page >> 4) & 1) * 256;
        (
            tpage_x + usize::from(texture.atlas.x) / 2,
            tpage_y + usize::from(texture.atlas.y),
            texture.size.x as usize,
            texture.size.y as usize,
        )
    }

    #[test]
    fn liquid_textures_cook_disjoint_double_buffered_tiles_inside_the_atlas() {
        let bytes = synthetic_bsp(&["*water", "wall", "+0butn", "+1butn"]);
        let bsp = Bsp::parse(&bytes).unwrap();
        let (textures, _atlas) = cook_textures(&bsp, SkyEncoding::Layered).unwrap();
        let records = serialize_textures(&textures);
        let decode = |index: usize| {
            <quake_formats::TextureInfo as quake_formats::CookedRecord>::decode(
                &records[index * 14..(index + 1) * 14],
            )
        };

        let water = decode(0);
        assert_eq!(water.animation_total, LIQUID_DOUBLE_BUFFER_MARKER);
        let alternate =
            quake_formats::liquid_alternate_texture(water).expect("double buffered liquid");
        let (ax, ay, aw, ah) = vram_rect(water);
        let (bx, by, bw, bh) = vram_rect(alternate);
        assert_eq!((aw, ah), (32, 64));
        assert_eq!((bw, bh), (32, 64));
        for &(x, y, w, h) in &[(ax, ay, aw, ah), (bx, by, bw, bh)] {
            assert!(x >= VRAM_X_START && x + w <= VRAM_X_START + VRAM_PAGE_WORDS);
            assert!(y + h <= VRAM_PAGE_COUNT * VRAM_PAGE_HEIGHT);
        }
        // Rewriting the inactive tile must never touch the active one.
        let disjoint = ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay;
        assert!(
            disjoint,
            "primary {:?} overlaps alternate {:?}",
            (ax, ay, aw, ah),
            (bx, by, bw, bh)
        );
    }

    #[test]
    fn non_liquid_and_authored_animation_chains_are_unchanged_by_double_buffering() {
        let bytes = synthetic_bsp(&["*water", "wall", "+0butn", "+1butn"]);
        let bsp = Bsp::parse(&bytes).unwrap();
        let (textures, _atlas) = cook_textures(&bsp, SkyEncoding::Layered).unwrap();
        let records = serialize_textures(&textures);
        let decode = |index: usize| {
            <quake_formats::TextureInfo as quake_formats::CookedRecord>::decode(
                &records[index * 14..(index + 1) * 14],
            )
        };

        let wall = decode(1);
        assert_eq!(wall.animation_total, 0);
        assert_eq!(wall.animation_next, -1);
        assert_eq!(wall.animation_alt, -1);
        assert_eq!(quake_formats::liquid_alternate_texture(wall), None);

        // The `+` ring keeps Quake's authored two-frame sequence untouched.
        let (frame0, frame1) = (decode(2), decode(3));
        assert_eq!(frame0.animation_total, 4);
        assert_eq!(frame1.animation_total, 4);
        assert_eq!((frame0.animation_min, frame0.animation_max), (0, 2));
        assert_eq!((frame1.animation_min, frame1.animation_max), (2, 4));
        assert_eq!(frame0.animation_next, 3);
        assert_eq!(frame1.animation_next, 2);
        assert_eq!(frame0.animation_alt, -1);
        assert_eq!(quake_formats::liquid_alternate_texture(frame0), None);
    }
}
