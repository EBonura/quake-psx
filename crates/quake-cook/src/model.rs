use core::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::entities::SourceEntity;
use super::geometry::{cook_geometry_staged, TextureAtlas};
use super::{
    checked_slice, read_i32, Bsp, BspLump, CookError, CookedEntities, GeometryLumps, PakArchive,
    SkyEncoding,
};
use quake_formats::ALIAS_MODEL_SPRITE;

const ALIAS_VERSION: i32 = 6;
const ALIAS_HEADER_BYTES: usize = 84;
const ALIAS_TEXCOORD_BYTES: usize = 12;
const ALIAS_TRIANGLE_BYTES: usize = 16;
const ALIAS_VERTEX_BYTES: usize = 4;
const SPRITE_IDENT: &[u8; 4] = b"IDSP";
const SPRITE_VERSION: i32 = 1;
const SPRITE_HEADER_BYTES: usize = 36;
const COOKED_HEADER_BYTES: usize = 68;
const COOKED_TRIANGLE_BYTES: usize = 12;
const MAX_VERTICES: usize = 512;
const MAX_TRIANGLES: usize = 1024;
const MAX_FRAMES: usize = 256;
const MAX_SKINS: usize = 3;
const MAX_MODELS: usize = 128;
const FIXED_ONE: i16 = 4096;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelCookStats {
    pub model_count: usize,
    pub alias_models: usize,
    pub brush_models: usize,
    pub texture_bytes: usize,
    pub model_bytes: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedModels {
    pub data: Vec<u8>,
    pub stats: ModelCookStats,
}

#[derive(Clone, Debug)]
struct ModelResource {
    primary_id: i16,
    variants: [Option<String>; 3],
}

#[derive(Clone, Copy, Debug, Default)]
struct CookedSkin {
    tpage: u16,
    base: [u8; 2],
}

#[derive(Clone, Debug)]
struct CookedHeader {
    model_type: u8,
    flags: u8,
    id: i16,
    frame_count: u16,
    vertex_count: u16,
    triangle_count: u16,
    skin_count: u16,
    scale: [i16; 3],
    offset: [i16; 3],
    mins: [i32; 3],
    maxs: [i32; 3],
    skins: [CookedSkin; MAX_SKINS],
    triangle_offset: u32,
    frame_offset: u32,
}

#[derive(Clone, Copy, Debug)]
struct AliasTexCoord {
    on_seam: bool,
    s: i32,
    t: i32,
}

#[derive(Clone, Copy, Debug)]
struct AliasTriangle {
    front: bool,
    vertices: [usize; 3],
}

#[derive(Clone, Debug)]
struct AliasFrame {
    vertices: Vec<[u8; 3]>,
}

#[derive(Clone, Debug)]
struct AliasModel {
    scale: [f32; 3],
    translate: [f32; 3],
    flags: i32,
    skin_width: usize,
    skin_height: usize,
    skins: Vec<Vec<u8>>,
    texcoords: Vec<AliasTexCoord>,
    triangles: Vec<AliasTriangle>,
    frames: Vec<AliasFrame>,
}

#[derive(Clone, Debug)]
struct SpriteFrame {
    left: i16,
    up: i16,
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SpriteModel {
    kind: u8,
    radius: f32,
    beam_length: i16,
    max_width: usize,
    max_height: usize,
    frames: Vec<SpriteFrame>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ModelProps {
    view_model: bool,
    delete: Option<(usize, usize)>,
}

/// Cook world geometry and entity models against one shared VRAM atlas.
pub fn cook_geometry_and_models(
    bsp: &Bsp<'_>,
    pak: &PakArchive<'_>,
    entities: &CookedEntities,
    model_map: &str,
    resource_list: &str,
    model_props: &str,
    sky: SkyEncoding,
) -> Result<(GeometryLumps, CookedModels), CookError> {
    let (mut geometry, mut atlas) = cook_geometry_staged(bsp, sky)?;
    let models = cook_models(
        pak,
        entities,
        model_map,
        resource_list,
        model_props,
        &mut atlas,
    )?;
    geometry.texture_data = atlas.finish();
    Ok((geometry, models))
}

fn cook_models(
    pak: &PakArchive<'_>,
    entities: &CookedEntities,
    model_map: &str,
    resource_list: &str,
    model_props: &str,
    atlas: &mut TextureAtlas,
) -> Result<CookedModels, CookError> {
    let models = parse_model_map(model_map)?;
    let ids = models
        .iter()
        .enumerate()
        .filter_map(|(id, name)| name.as_ref().map(|name| (name.clone(), id as i16)))
        .collect::<BTreeMap<_, _>>();
    let resources = parse_model_resources(resource_list, &ids)?;
    let props = parse_model_props(model_props)?;
    let selected = select_models(&entities.source, entities.world_type, &resources, &ids)?;

    let mut headers = Vec::new();
    let mut model_data = Vec::new();
    let mut alias_models = 0usize;
    let mut brush_models = 0usize;
    let mut loaded = BTreeSet::new();
    for (id, source_name) in selected {
        if loaded.contains(&id) {
            continue;
        }
        let source = pak.require(&source_name)?;
        let header = if source_name.ends_with(".bsp") {
            let brush = Bsp::parse(source)?;
            brush_models += 1;
            cook_brush_model(&brush, id, atlas, &mut model_data)?
        } else if source_name.ends_with(".spr") {
            let sprite = parse_sprite_model(source).map_err(|error| {
                CookError::new(format!("could not parse {source_name}: {error}"))
            })?;
            alias_models += 1;
            cook_sprite_model(&sprite, id, atlas, &mut model_data)?
        } else {
            let mut alias = parse_alias_model(source).map_err(|error| {
                CookError::new(format!("could not parse {source_name}: {error}"))
            })?;
            let model_props = props.get(&source_name).copied().unwrap_or_default();
            apply_model_props(&mut alias, model_props, &source_name)?;
            if model_props.view_model {
                sort_view_model_triangles(&mut alias, &source_name)?;
            }
            alias_models += 1;
            cook_alias_model(&source_name, &alias, id, atlas, &mut model_data)?
        };
        loaded.insert(id);
        headers.push(header);
        if headers.len() >= MAX_MODELS {
            return Err(CookError::new("map needs too many entity models"));
        }
    }

    let mut data = Vec::with_capacity(4 + headers.len() * COOKED_HEADER_BYTES + model_data.len());
    data.extend_from_slice(&(headers.len() as u32).to_le_bytes());
    for header in &headers {
        serialize_header(header, &mut data);
    }
    data.extend_from_slice(&model_data);
    debug_assert_eq!(
        data.len(),
        4 + headers.len() * COOKED_HEADER_BYTES + model_data.len()
    );
    Ok(CookedModels {
        stats: ModelCookStats {
            model_count: headers.len(),
            alias_models,
            brush_models,
            texture_bytes: 0,
            model_bytes: data.len(),
        },
        data,
    })
}

fn parse_sprite_model(bytes: &[u8]) -> Result<SpriteModel, CookError> {
    let header = checked_slice(bytes, 0, SPRITE_HEADER_BYTES, "sprite header")?;
    if &header[..4] != SPRITE_IDENT {
        return Err(CookError::new("sprite has bad magic"));
    }
    let version = i32_at(header, 4, "sprite version")?;
    if version != SPRITE_VERSION {
        return Err(CookError::new(format!(
            "unsupported sprite version {version}"
        )));
    }
    let kind = i32_at(header, 8, "sprite type")?;
    if !(0..=4).contains(&kind) {
        return Err(CookError::new(format!("unsupported sprite type {kind}")));
    }
    let radius = f32_at(header, 12)?;
    let max_width = positive_usize(i32_at(header, 16, "sprite width")?, "sprite width")?;
    let max_height = positive_usize(i32_at(header, 20, "sprite height")?, "sprite height")?;
    let declared_frames = positive_usize(
        i32_at(header, 24, "sprite frame count")?,
        "sprite frame count",
    )?;
    if max_width > u8::MAX as usize || max_height > u8::MAX as usize || declared_frames > MAX_FRAMES
    {
        return Err(CookError::new(
            "sprite exceeds PSX dimensions or frame limit",
        ));
    }
    let beam_length = f32_at(header, 28)?
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    let mut cursor = SPRITE_HEADER_BYTES;
    let mut frames = Vec::new();
    for _ in 0..declared_frames {
        let frame_type = i32_at(bytes, cursor, "sprite frame type")?;
        cursor += 4;
        if frame_type == 0 {
            frames.push(parse_sprite_frame(
                bytes,
                &mut cursor,
                max_width,
                max_height,
            )?);
            continue;
        }
        if frame_type != 1 {
            return Err(CookError::new(format!(
                "unsupported sprite frame type {frame_type}"
            )));
        }
        let group_frames = positive_usize(
            i32_at(bytes, cursor, "sprite group count")?,
            "sprite group count",
        )?;
        cursor += 4;
        let interval_bytes = group_frames
            .checked_mul(4)
            .ok_or_else(|| CookError::new("sprite interval size overflow"))?;
        let intervals = checked_slice(bytes, cursor, interval_bytes, "sprite intervals")?;
        for interval in intervals.chunks_exact(4) {
            let value = f32::from_le_bytes(interval.try_into().unwrap());
            if !value.is_finite() || value <= 0.0 {
                return Err(CookError::new("sprite group has a bad interval"));
            }
        }
        cursor += interval_bytes;
        for _ in 0..group_frames {
            frames.push(parse_sprite_frame(
                bytes,
                &mut cursor,
                max_width,
                max_height,
            )?);
        }
        if frames.len() > MAX_FRAMES {
            return Err(CookError::new("sprite exceeds PSX frame limit"));
        }
    }
    if frames.is_empty() || cursor != bytes.len() {
        return Err(CookError::new("sprite has no frames or trailing data"));
    }
    Ok(SpriteModel {
        kind: kind as u8,
        radius,
        beam_length,
        max_width,
        max_height,
        frames,
    })
}

fn parse_sprite_frame(
    bytes: &[u8],
    cursor: &mut usize,
    max_width: usize,
    max_height: usize,
) -> Result<SpriteFrame, CookError> {
    let header = checked_slice(bytes, *cursor, 16, "sprite frame header")?;
    let left = i32_at(header, 0, "sprite frame left")?;
    let up = i32_at(header, 4, "sprite frame up")?;
    let width = positive_usize(
        i32_at(header, 8, "sprite frame width")?,
        "sprite frame width",
    )?;
    let height = positive_usize(
        i32_at(header, 12, "sprite frame height")?,
        "sprite frame height",
    )?;
    if width > max_width
        || height > max_height
        || left < i16::MIN as i32
        || left > i16::MAX as i32
        || up < i16::MIN as i32
        || up > i16::MAX as i32
    {
        return Err(CookError::new("sprite frame exceeds its declared bounds"));
    }
    *cursor += 16;
    let pixel_bytes = width
        .checked_mul(height)
        .ok_or_else(|| CookError::new("sprite frame size overflow"))?;
    let pixels = checked_slice(bytes, *cursor, pixel_bytes, "sprite frame pixels")?.to_vec();
    *cursor += pixel_bytes;
    Ok(SpriteFrame {
        left: left as i16,
        up: up as i16,
        width,
        height,
        pixels,
    })
}

fn parse_model_map(input: &str) -> Result<Vec<Option<String>>, CookError> {
    let mut output = vec![None; MAX_MODELS];
    for (line_index, line) in input.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next() else { continue };
        if id.starts_with('#') {
            continue;
        }
        let id = usize::from_str_radix(id, 16)
            .map_err(|_| CookError::new(format!("model map line {} has bad ID", line_index + 1)))?;
        let name = fields.next().ok_or_else(|| {
            CookError::new(format!("model map line {} has no name", line_index + 1))
        })?;
        if id == 0 || id >= output.len() {
            return Err(CookError::new(format!("model ID {id:#x} is out of range")));
        }
        if output[id].replace(name.to_owned()).is_some() {
            return Err(CookError::new(format!("duplicate model ID {id:#x}")));
        }
    }
    Ok(output)
}

fn parse_model_resources(
    input: &str,
    model_ids: &BTreeMap<String, i16>,
) -> Result<BTreeMap<String, Vec<ModelResource>>, CookError> {
    let mut output = BTreeMap::<String, Vec<ModelResource>>::new();
    let mut active_class: Option<String> = None;
    for (line_index, line) in input.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else { continue };
        if kind.starts_with('#') {
            continue;
        }
        if kind == "ent" {
            let class_name = fields.next().ok_or_else(|| {
                CookError::new(format!(
                    "resource line {} has no class name",
                    line_index + 1
                ))
            })?;
            active_class = Some(class_name.to_owned());
            output.entry(class_name.to_owned()).or_default();
            continue;
        }
        if kind != "mdl" {
            continue;
        }
        let class_name = active_class.as_ref().ok_or_else(|| {
            CookError::new(format!(
                "resource line {} declares model before an entity class",
                line_index + 1
            ))
        })?;
        let names = fields.take(3).map(str::to_owned).collect::<Vec<_>>();
        let Some(primary_name) = names.first() else {
            return Err(CookError::new(format!(
                "resource line {} has no model name",
                line_index + 1
            )));
        };
        let primary_id = model_ids.get(primary_name).copied().ok_or_else(|| {
            CookError::new(format!(
                "resource line {} references unknown model {primary_name}",
                line_index + 1
            ))
        })?;
        for name in &names {
            if !model_ids.contains_key(name) {
                return Err(CookError::new(format!(
                    "resource line {} references unknown model {name}",
                    line_index + 1
                )));
            }
        }
        let mut variants: [Option<String>; 3] = [None, None, None];
        for (slot, name) in names.into_iter().enumerate() {
            variants[slot] = Some(name);
        }
        output
            .entry(class_name.clone())
            .or_default()
            .push(ModelResource {
                primary_id,
                variants,
            });
    }
    Ok(output)
}

fn parse_model_props(input: &str) -> Result<BTreeMap<String, ModelProps>, CookError> {
    let mut output = BTreeMap::new();
    let mut active: Option<String> = None;
    for (line_index, line) in input.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else { continue };
        if kind.starts_with('#') {
            continue;
        }
        match kind {
            "mdl" => {
                let name = fields.next().ok_or_else(|| {
                    CookError::new(format!("model props line {} has no name", line_index + 1))
                })?;
                active = Some(name.to_owned());
                output
                    .entry(name.to_owned())
                    .or_insert(ModelProps::default());
            }
            "viewmodel" => {
                let name = active.as_ref().ok_or_else(|| {
                    CookError::new(format!("model props line {} has no model", line_index + 1))
                })?;
                output.entry(name.clone()).or_default().view_model = true;
            }
            "delete" => {
                let name = active.as_ref().ok_or_else(|| {
                    CookError::new(format!("model props line {} has no model", line_index + 1))
                })?;
                let range = fields.next().ok_or_else(|| {
                    CookError::new(format!(
                        "model props line {} has no frame range",
                        line_index + 1
                    ))
                })?;
                let (first, last) = if let Some((first, last)) = range.split_once("..") {
                    (
                        parse_usize(first, "deleted frame")?,
                        parse_usize(last, "deleted frame")?,
                    )
                } else {
                    let frame = parse_usize(range, "deleted frame")?;
                    (frame, frame)
                };
                output.entry(name.clone()).or_default().delete = Some((first, last));
            }
            _ => {}
        }
    }
    Ok(output)
}

fn select_models(
    entities: &[SourceEntity],
    world_type: i32,
    resources: &BTreeMap<String, Vec<ModelResource>>,
    model_ids: &BTreeMap<String, i16>,
) -> Result<Vec<(i16, String)>, CookError> {
    let world_slot = usize::try_from(world_type)
        .ok()
        .filter(|&slot| slot < 3)
        .ok_or_else(|| CookError::new(format!("worldtype {world_type} is outside 0..=2")))?;
    let mut output = Vec::new();
    let mut loaded = BTreeSet::new();
    for entity in entities {
        if let Some(class_resources) = resources.get(&entity.class_name) {
            for resource in class_resources {
                if loaded.contains(&resource.primary_id) {
                    continue;
                }
                let selected = resource.variants[world_slot]
                    .as_ref()
                    .or(resource.variants[0].as_ref())
                    .expect("model resource always has a primary variant");
                loaded.insert(resource.primary_id);
                output.push((resource.primary_id, selected.clone()));
            }
        }
        if let Some(name) = entity.get("model") {
            if !name.is_empty() && !name.starts_with('*') {
                if let Some(&id) = model_ids.get(name) {
                    if loaded.insert(id) {
                        output.push((id, name.to_owned()));
                    }
                }
            }
        }
    }
    Ok(output)
}

fn parse_alias_model(bytes: &[u8]) -> Result<AliasModel, CookError> {
    let header = checked_slice(bytes, 0, ALIAS_HEADER_BYTES, "alias model header")?;
    let version = i32_at(header, 4, "alias version")?;
    if version != ALIAS_VERSION {
        return Err(CookError::new(format!(
            "unsupported alias model version {version}"
        )));
    }
    let scale = [f32_at(header, 8)?, f32_at(header, 12)?, f32_at(header, 16)?];
    let translate = [
        f32_at(header, 20)?,
        f32_at(header, 24)?,
        f32_at(header, 28)?,
    ];
    let skin_count = positive_usize(i32_at(header, 48, "alias skin count")?, "alias skin count")?;
    let skin_width = positive_usize(i32_at(header, 52, "alias skin width")?, "alias skin width")?;
    let skin_height = positive_usize(
        i32_at(header, 56, "alias skin height")?,
        "alias skin height",
    )?;
    let vertex_count = positive_usize(
        i32_at(header, 60, "alias vertex count")?,
        "alias vertex count",
    )?;
    let triangle_count = positive_usize(
        i32_at(header, 64, "alias triangle count")?,
        "alias triangle count",
    )?;
    let declared_frames = positive_usize(
        i32_at(header, 68, "alias frame count")?,
        "alias frame count",
    )?;
    if vertex_count >= MAX_VERTICES || triangle_count >= MAX_TRIANGLES {
        return Err(CookError::new(
            "alias model exceeds PSX vertex or triangle limit",
        ));
    }
    if skin_width < 2 || skin_height < 2 {
        return Err(CookError::new("alias skin dimensions are too small"));
    }
    let skin_bytes = skin_width
        .checked_mul(skin_height)
        .ok_or_else(|| CookError::new("alias skin size overflow"))?;
    let mut cursor = ALIAS_HEADER_BYTES;
    let mut skins = Vec::with_capacity(skin_count);
    for skin in 0..skin_count {
        let group = i32_at(bytes, cursor, "alias skin type")?;
        cursor += 4;
        if group != 0 {
            return Err(CookError::new(format!(
                "grouped alias skin {skin} is unsupported"
            )));
        }
        skins.push(checked_slice(bytes, cursor, skin_bytes, "alias skin pixels")?.to_vec());
        cursor += skin_bytes;
    }

    let mut texcoords = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        let record = checked_slice(bytes, cursor, ALIAS_TEXCOORD_BYTES, "alias texcoord")?;
        texcoords.push(AliasTexCoord {
            on_seam: i32_at(record, 0, "alias seam flag")? != 0,
            s: i32_at(record, 4, "alias texture S")?,
            t: i32_at(record, 8, "alias texture T")?,
        });
        cursor += ALIAS_TEXCOORD_BYTES;
    }

    let mut triangles = Vec::with_capacity(triangle_count);
    for _ in 0..triangle_count {
        let record = checked_slice(bytes, cursor, ALIAS_TRIANGLE_BYTES, "alias triangle")?;
        let mut vertices = [0usize; 3];
        for corner in 0..3 {
            vertices[corner] = nonnegative_usize(
                i32_at(record, 4 + corner * 4, "alias triangle vertex")?,
                "alias triangle vertex",
            )?;
            if vertices[corner] >= vertex_count {
                return Err(CookError::new("alias triangle vertex is out of range"));
            }
        }
        triangles.push(AliasTriangle {
            front: i32_at(record, 0, "alias triangle side")? != 0,
            vertices,
        });
        cursor += ALIAS_TRIANGLE_BYTES;
    }

    let first_type = i32_at(bytes, cursor, "alias frame type")?;
    let (frame_count, grouped) = if first_type != 0 {
        cursor += 4;
        let count = positive_usize(
            i32_at(bytes, cursor, "alias group frame count")?,
            "alias group frame count",
        )?;
        cursor += 4 + 4 + 4;
        cursor = cursor
            .checked_add(
                count
                    .checked_mul(4)
                    .ok_or_else(|| CookError::new("alias intervals overflow"))?,
            )
            .ok_or_else(|| CookError::new("alias intervals overflow"))?;
        (count, true)
    } else {
        (declared_frames, false)
    };
    if frame_count >= MAX_FRAMES {
        return Err(CookError::new("alias model exceeds PSX frame limit"));
    }
    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        if !grouped {
            let frame_type = i32_at(bytes, cursor, "alias frame type")?;
            cursor += 4;
            if frame_type != 0 {
                return Err(CookError::new("nested alias frame groups are unsupported"));
            }
        }
        checked_slice(bytes, cursor, 4 + 4 + 16, "alias frame header")?;
        cursor += 24;
        let vertex_bytes = vertex_count
            .checked_mul(ALIAS_VERTEX_BYTES)
            .ok_or_else(|| CookError::new("alias frame size overflow"))?;
        let source = checked_slice(bytes, cursor, vertex_bytes, "alias frame vertices")?;
        let vertices = source
            .chunks_exact(ALIAS_VERTEX_BYTES)
            .map(|vertex| [vertex[0], vertex[1], vertex[2]])
            .collect();
        frames.push(AliasFrame { vertices });
        cursor += vertex_bytes;
    }
    Ok(AliasModel {
        scale,
        translate,
        flags: i32_at(header, 76, "alias flags")?,
        skin_width,
        skin_height,
        skins,
        texcoords,
        triangles,
        frames,
    })
}

fn apply_model_props(
    model: &mut AliasModel,
    props: ModelProps,
    name: &str,
) -> Result<(), CookError> {
    if let Some((first, last)) = props.delete {
        if first > last || last >= model.frames.len() {
            return Err(CookError::new(format!(
                "bad deleted frame range for {name}"
            )));
        }
        model.frames.drain(first..=last);
        if model.frames.is_empty() {
            return Err(CookError::new(format!("frame deletion emptied {name}")));
        }
    }
    Ok(())
}

fn sort_view_model_triangles(model: &mut AliasModel, name: &str) -> Result<(), CookError> {
    let first = model
        .frames
        .first()
        .ok_or_else(|| CookError::new("view model has no frames"))?;
    let scale = model.scale[0];
    let translate = model.translate[0];
    let muzzle_hack = !name.contains("v_axe") && !name.contains("v_light");
    let mode = if name.contains("v_axe") {
        1
    } else if name.contains("v_shot2") || name.contains("v_nail") {
        2
    } else {
        0
    };
    model.triangles.sort_by(|left, right| {
        let weight = |triangle: &AliasTriangle| {
            let x = triangle
                .vertices
                .map(|index| (first.vertices[index][0] as f32).mul_add(scale, translate));
            let mut value = match mode {
                1 => x[0].max(x[1]).max(x[2]),
                2 => x[0].min(x[1]).min(x[2]),
                _ => (x[0] + x[1] + x[2]) / 3.0,
            };
            if muzzle_hack && x.into_iter().all(|x| x < -1.0) {
                value += 1000.0;
            }
            value
        };
        weight(right)
            .partial_cmp(&weight(left))
            .unwrap_or(Ordering::Equal)
    });
    Ok(())
}

fn cook_alias_model(
    source_name: &str,
    model: &AliasModel,
    id: i16,
    atlas: &mut TextureAtlas,
    model_data: &mut Vec<u8>,
) -> Result<CookedHeader, CookError> {
    align_model_data(model_data);
    let triangle_offset = model_data.len() as u32;
    let dst_width = model.skin_width / 2;
    let dst_height = model.skin_height / 2;
    let mut cooked_skins = [CookedSkin::default(); MAX_SKINS];
    let skin_count = model.skins.len().min(MAX_SKINS);
    for (skin_index, source) in model.skins.iter().take(skin_count).enumerate() {
        let pixels =
            downsample_alias_skin(source_name, source, model.skin_width, model.skin_height);
        let (base, tpage, x, y) = atlas.fit(dst_width, dst_height)?;
        atlas.store(x, y, dst_width, dst_height, &pixels);
        cooked_skins[skin_index] = CookedSkin { tpage, base };
    }
    if skin_count == 0 {
        return Err(CookError::new("alias model has no skins"));
    }

    for skin in cooked_skins.iter().take(skin_count) {
        for triangle in &model.triangles {
            for &vertex in &triangle.vertices {
                let texcoord = model.texcoords[vertex];
                let u = (texcoord.s as f32 + 0.5) / model.skin_width as f32;
                let v = (texcoord.t as f32 + 0.5) / model.skin_height as f32;
                let u2 = (texcoord.s as f32 + model.skin_width as f32 * 0.5 + 0.5)
                    / model.skin_width as f32;
                let relative_u = if texcoord.on_seam && !triangle.front {
                    float_to_u8_wrap(u2 * dst_width as f32)
                } else {
                    float_to_u8_wrap(u * dst_width as f32)
                };
                model_data.push(skin.base[0].wrapping_add(relative_u));
                model_data.push(skin.base[1].wrapping_add(float_to_u8_wrap(v * dst_height as f32)));
                model_data.extend_from_slice(&((vertex * 8) as u16).to_le_bytes());
            }
        }
    }
    let frame_offset = model_data.len() as u32;
    let mut mins = [1.0e10f32; 3];
    let mut maxs = [-1.0e10f32; 3];
    for frame in &model.frames {
        for vertex in &frame.vertices {
            model_data.extend_from_slice(vertex);
            for axis in 0..3 {
                let value = (vertex[axis] as f32).mul_add(model.scale[axis], model.translate[axis]);
                if value < mins[axis] {
                    mins[axis] = value;
                } else if value > maxs[axis] {
                    maxs[axis] = value;
                }
            }
        }
    }
    Ok(CookedHeader {
        model_type: 1,
        flags: model.flags as u8,
        id,
        frame_count: model.frames.len() as u16,
        vertex_count: model.texcoords.len() as u16,
        triangle_count: model.triangles.len() as u16,
        skin_count: skin_count as u16,
        scale: model.scale.map(float_to_fixed_i16),
        offset: model.translate.map(|value| value.round() as i16),
        mins: mins.map(float_to_fixed_i32),
        maxs: maxs.map(float_to_fixed_i32),
        skins: cooked_skins,
        triangle_offset,
        frame_offset,
    })
}

fn cook_sprite_model(
    model: &SpriteModel,
    id: i16,
    atlas: &mut TextureAtlas,
    model_data: &mut Vec<u8>,
) -> Result<CookedHeader, CookError> {
    let cell_width = (model.max_width + 1) & !1;
    let max_columns = (256 / cell_width).min(model.frames.len());
    let columns = (1..=max_columns)
        .rev()
        .find(|columns| model.frames.len().div_ceil(*columns) * model.max_height <= 256)
        .ok_or_else(|| CookError::new("sprite sheet does not fit one texture page"))?;
    let rows = model.frames.len().div_ceil(columns);
    let sheet_width = columns * cell_width;
    let sheet_height = rows * model.max_height;
    let (base, tpage, x, y) = atlas.fit(sheet_width, sheet_height)?;

    align_model_data(model_data);
    let triangle_offset = model_data.len() as u32;
    // Two valid triangles keep the shared alias table and projected-offset
    // validator authoritative. The sprite renderer reads only their bounded
    // count; frame-local UVs and geometry live in the twelve metadata bytes.
    for (u, v, offset) in [
        (0u8, 0u8, 0u16),
        (0, 0, 8),
        (0, 0, 16),
        (0, 0, 8),
        (0, 0, 24),
        (0, 0, 16),
    ] {
        model_data.extend_from_slice(&[u, v]);
        model_data.extend_from_slice(&offset.to_le_bytes());
    }
    let frame_offset = model_data.len() as u32;
    for (index, frame) in model.frames.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let frame_x = x + column * cell_width / 2;
        let frame_y = y + row * model.max_height;
        atlas.store(frame_x, frame_y, frame.width, frame.height, &frame.pixels);
        let u = base[0].wrapping_add((column * cell_width) as u8);
        let v = base[1].wrapping_add((row * model.max_height) as u8);
        model_data.extend_from_slice(&[u, v, frame.width as u8, frame.height as u8]);
        model_data.extend_from_slice(&frame.left.to_le_bytes());
        model_data.extend_from_slice(&frame.up.to_le_bytes());
        model_data.extend_from_slice(&model.beam_length.to_le_bytes());
        model_data.extend_from_slice(&[model.kind, 0]);
    }
    let radius = if model.radius.is_finite() {
        model.radius.max(1.0)
    } else {
        return Err(CookError::new("sprite has a bad radius"));
    };
    let radius_q12 = float_to_fixed_i32(radius);
    Ok(CookedHeader {
        model_type: 1,
        flags: ALIAS_MODEL_SPRITE | model.kind,
        id,
        frame_count: model.frames.len() as u16,
        vertex_count: 4,
        triangle_count: 2,
        skin_count: 1,
        scale: [0; 3],
        offset: [0; 3],
        mins: [-radius_q12; 3],
        maxs: [radius_q12; 3],
        skins: [
            CookedSkin { tpage, base },
            CookedSkin::default(),
            CookedSkin::default(),
        ],
        triangle_offset,
        frame_offset,
    })
}

fn downsample_alias_skin(source_name: &str, source: &[u8], width: usize, height: usize) -> Vec<u8> {
    let chroma_key = matches!(source_name, "progs/flame.mdl" | "progs/flame2.mdl")
        .then(|| source.first().copied())
        .flatten();
    let mut pixels = Vec::with_capacity((width / 2) * (height / 2));
    for y in 0..height / 2 {
        for x in 0..width / 2 {
            let pixel = source[(y * 2) * width + x * 2];
            pixels.push(if Some(pixel) == chroma_key {
                0xff
            } else {
                pixel
            });
        }
    }
    pixels
}

fn cook_brush_model(
    bsp: &Bsp<'_>,
    id: i16,
    atlas: &mut TextureAtlas,
    model_data: &mut Vec<u8>,
) -> Result<CookedHeader, CookError> {
    if bsp.record_count(BspLump::Vertices) != Some(8) || bsp.record_count(BspLump::Faces) != Some(6)
    {
        return Err(CookError::new(
            "entity BSP is not an 8-vertex, 6-face cuboid",
        ));
    }
    let mut textures = Vec::new();
    let mut total_width = 0usize;
    let mut total_height = 0usize;
    for index in 0..bsp.texture_count().min(2) {
        let texture = bsp
            .mip_texture(index)?
            .ok_or_else(|| CookError::new("entity BSP has a missing texture"))?;
        total_width = total_width.max(texture.width);
        total_height = total_height
            .checked_add(texture.height)
            .ok_or_else(|| CookError::new("entity BSP texture size overflow"))?;
        textures.push(texture);
    }
    if textures.is_empty() || total_width & 1 != 0 {
        return Err(CookError::new("entity BSP has no usable textures"));
    }
    let (base, tpage, x, y) = atlas.fit(total_width, total_height)?;
    let mut texture_y = y;
    for texture in &textures {
        atlas.store(
            x,
            texture_y,
            texture.width,
            texture.height,
            texture.levels[0],
        );
        texture_y += texture.height;
    }

    align_model_data(model_data);
    let triangle_offset = model_data.len() as u32;
    let vertices_source = bsp.lump(BspLump::Vertices);
    let mut source_vertices = [[0f32; 3]; 8];
    let mut cooked_vertices = [[0u8; 3]; 8];
    let mut mins = [i32::MAX; 3];
    let mut maxs = [i32::MIN; 3];
    for (index, source) in vertices_source.chunks_exact(12).enumerate() {
        for axis in 0..3 {
            let value = f32_at(source, axis * 4)?;
            source_vertices[index][axis] = value;
            let integer = value as i32;
            cooked_vertices[index][axis] = integer as u8;
            if integer < mins[axis] {
                mins[axis] = integer;
            } else if integer > maxs[axis] {
                maxs[axis] = integer;
            }
        }
    }

    let mut triangles = Vec::with_capacity(12 * COOKED_TRIANGLE_BYTES);
    for face in bsp.lump(BspLump::Faces).chunks_exact(20) {
        let first_edge = nonnegative_usize(
            i32_at(face, 4, "entity face first edge")?,
            "entity face first edge",
        )?;
        let edge_count = positive_usize(i16_at(face, 8)? as i32, "entity face edge count")?;
        if edge_count != 4 {
            return Err(CookError::new("entity BSP face is not a quad"));
        }
        let texture_info_index =
            nonnegative_usize(i16_at(face, 10)? as i32, "entity face texture info")?;
        let texture_info = record(
            bsp.lump(BspLump::TextureInfo),
            40,
            texture_info_index,
            "entity texture info",
        )?;
        let texture_index = nonnegative_usize(
            i32_at(texture_info, 32, "entity texture index")?,
            "entity texture index",
        )?;
        let texture = bsp
            .mip_texture(texture_index)?
            .ok_or_else(|| CookError::new("entity face has a missing texture"))?;
        let base_v = if texture_index != 0 && textures.len() > 1 {
            textures[0].height as u8
        } else {
            0
        };
        let mut face_vertices = [0usize; 4];
        let mut st = [[0f32; 2]; 4];
        let mut st_min = [1.0e10f32; 2];
        for corner in 0..4 {
            let surface_edge = record_i32(bsp.lump(BspLump::SurfaceEdges), first_edge + corner)?;
            let edge = record(
                bsp.lump(BspLump::Edges),
                4,
                surface_edge.unsigned_abs() as usize,
                "entity edge",
            )?;
            face_vertices[corner] = if surface_edge >= 0 {
                u16_at(edge, 0)? as usize
            } else {
                u16_at(edge, 2)? as usize
            };
            if face_vertices[corner] >= source_vertices.len() {
                return Err(CookError::new("entity face vertex is out of range"));
            }
            for axis in 0..2 {
                let base_offset = axis * 16;
                st[corner][axis] = dot3_host(
                    source_vertices[face_vertices[corner]],
                    [
                        f32_at(texture_info, base_offset)?,
                        f32_at(texture_info, base_offset + 4)?,
                        f32_at(texture_info, base_offset + 8)?,
                    ],
                ) + f32_at(texture_info, base_offset + 12)?;
                st_min[axis] = st_min[axis].min(st[corner][axis]);
            }
        }
        for corner in 0..4 {
            if st_min[0] < 0.0 {
                st[corner][0] += texture.width as f32;
            }
            if st_min[1] < 0.0 {
                st[corner][1] += texture.height as f32;
            }
            st[corner][0] = st[corner][0] / texture.width as f32 * (texture.width - 1) as f32;
            st[corner][1] = st[corner][1] / texture.height as f32 * (texture.height - 1) as f32;
        }
        for corners in [[0usize, 1, 2], [0, 2, 3]] {
            for corner in corners {
                triangles.push(base[0].wrapping_add(float_to_u8_wrap(st[corner][0])));
                triangles.push(
                    base[1]
                        .wrapping_add(base_v)
                        .wrapping_add(float_to_u8_wrap(st[corner][1])),
                );
                triangles.extend_from_slice(&((face_vertices[corner] * 8) as u16).to_le_bytes());
            }
        }
    }
    model_data.extend_from_slice(&triangles);
    let frame_offset = model_data.len() as u32;
    for vertex in cooked_vertices {
        model_data.extend_from_slice(&vertex);
    }
    Ok(CookedHeader {
        model_type: 1,
        flags: 0,
        id,
        frame_count: 1,
        vertex_count: 8,
        triangle_count: 12,
        skin_count: 1,
        scale: [FIXED_ONE; 3],
        offset: [0; 3],
        mins: mins.map(|value| value << 12),
        maxs: maxs.map(|value| value << 12),
        skins: [
            CookedSkin { tpage, base },
            CookedSkin::default(),
            CookedSkin::default(),
        ],
        triangle_offset,
        frame_offset,
    })
}

fn serialize_header(header: &CookedHeader, output: &mut Vec<u8>) {
    output.extend_from_slice(&[header.model_type, header.flags]);
    output.extend_from_slice(&header.id.to_le_bytes());
    for value in [
        header.frame_count,
        header.vertex_count,
        header.triangle_count,
        header.skin_count,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for value in header.scale.into_iter().chain(header.offset) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for value in header.mins.into_iter().chain(header.maxs) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for skin in header.skins {
        output.extend_from_slice(&skin.tpage.to_le_bytes());
        output.extend_from_slice(&skin.base);
    }
    output.extend_from_slice(&header.triangle_offset.to_le_bytes());
    output.extend_from_slice(&header.frame_offset.to_le_bytes());
}

fn align_model_data(data: &mut Vec<u8>) {
    let aligned = (data.len() + 3) & !3;
    data.resize(aligned, 0);
}

fn record<'a>(
    bytes: &'a [u8],
    size: usize,
    index: usize,
    context: &str,
) -> Result<&'a [u8], CookError> {
    let offset = index
        .checked_mul(size)
        .ok_or_else(|| CookError::new(format!("{context} index overflow")))?;
    checked_slice(bytes, offset, size, context)
}

fn record_i32(bytes: &[u8], index: usize) -> Result<i32, CookError> {
    i32_at(record(bytes, 4, index, "i32 record")?, 0, "i32 record")
}

fn i16_at(bytes: &[u8], offset: usize) -> Result<i16, CookError> {
    let value = checked_slice(bytes, offset, 2, "i16")?;
    Ok(i16::from_le_bytes(value.try_into().unwrap()))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, CookError> {
    let value = checked_slice(bytes, offset, 2, "u16")?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn i32_at(bytes: &[u8], offset: usize, context: &str) -> Result<i32, CookError> {
    read_i32(bytes, offset, context)
}

fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, CookError> {
    let value = checked_slice(bytes, offset, 4, "f32")?;
    Ok(f32::from_le_bytes(value.try_into().unwrap()))
}

fn positive_usize(value: i32, context: &str) -> Result<usize, CookError> {
    if value <= 0 {
        return Err(CookError::new(format!("{context} must be positive")));
    }
    Ok(value as usize)
}

fn nonnegative_usize(value: i32, context: &str) -> Result<usize, CookError> {
    usize::try_from(value).map_err(|_| CookError::new(format!("{context} is negative")))
}

fn parse_usize(value: &str, context: &str) -> Result<usize, CookError> {
    value
        .parse()
        .map_err(|_| CookError::new(format!("bad {context}: {value}")))
}

fn float_to_fixed_i16(value: f32) -> i16 {
    (value * 4096.0) as i16
}

fn float_to_fixed_i32(value: f32) -> i32 {
    (value * 4096.0) as i32
}

fn float_to_u8_wrap(value: f32) -> u8 {
    (value.floor() as i32) as u8
}

fn dot3_host(left: [f32; 3], right: [f32; 3]) -> f32 {
    let xy = left[0].mul_add(right[0], left[1] * right[1]);
    left[2].mul_add(right[2], xy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite_header(kind: i32, width: i32, height: i32, frames: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SPRITE_IDENT);
        for value in [
            SPRITE_VERSION,
            kind,
            12.0f32.to_bits() as i32,
            width,
            height,
            frames,
            0.0f32.to_bits() as i32,
            0,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn push_sprite_frame(bytes: &mut Vec<u8>, left: i32, up: i32, width: i32, height: i32) {
        for value in [left, up, width, height] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend((0..width * height).map(|pixel| pixel as u8));
    }

    #[test]
    fn model_props_parse_deletes_and_view_models() {
        let props =
            parse_model_props("mdl progs/a.mdl\n delete 3..7\nmdl progs/v_a.mdl\n viewmodel\n")
                .unwrap();
        assert_eq!(props["progs/a.mdl"].delete, Some((3, 7)));
        assert!(props["progs/v_a.mdl"].view_model);
    }

    #[test]
    fn cooked_header_has_wire_size() {
        let mut bytes = Vec::new();
        serialize_header(
            &CookedHeader {
                model_type: 1,
                flags: 0,
                id: 1,
                frame_count: 1,
                vertex_count: 1,
                triangle_count: 1,
                skin_count: 1,
                scale: [0; 3],
                offset: [0; 3],
                mins: [0; 3],
                maxs: [0; 3],
                skins: [CookedSkin::default(); 3],
                triangle_offset: 0,
                frame_offset: 0,
            },
            &mut bytes,
        );
        assert_eq!(bytes.len(), COOKED_HEADER_BYTES);
    }

    #[test]
    fn flame_skin_converts_first_palette_entry_to_chroma_key() {
        let source = [7, 1, 2, 3, 4, 5, 7, 6, 7, 8, 9, 10, 11, 12, 7, 13];
        assert_eq!(
            downsample_alias_skin("progs/flame.mdl", &source, 4, 4),
            vec![0xff, 2, 0xff, 9]
        );
        assert_eq!(
            downsample_alias_skin("progs/flame2.mdl", &source, 4, 4),
            vec![0xff, 2, 0xff, 9]
        );
    }

    #[test]
    fn ordinary_alias_skin_preserves_the_same_palette_entry() {
        let source = [7, 1, 2, 3, 4, 5, 7, 6, 7, 8, 9, 10, 11, 12, 7, 13];
        assert_eq!(
            downsample_alias_skin("progs/torch.mdl", &source, 4, 4),
            vec![7, 2, 7, 9]
        );
    }

    #[test]
    fn sprite_parser_retains_simple_frame_geometry_and_pixels() {
        let mut bytes = sprite_header(2, 16, 16, 1);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        push_sprite_frame(&mut bytes, -8, 8, 16, 16);
        let sprite = parse_sprite_model(&bytes).unwrap();
        assert_eq!(sprite.kind, 2);
        assert_eq!(sprite.frames.len(), 1);
        assert_eq!(sprite.frames[0].left, -8);
        assert_eq!(sprite.frames[0].up, 8);
        assert_eq!(sprite.frames[0].pixels.len(), 256);
    }

    #[test]
    fn sprite_parser_flattens_checked_group_frames_for_the_fixed_clock() {
        let mut bytes = sprite_header(4, 8, 8, 1);
        bytes.extend_from_slice(&1i32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        bytes.extend_from_slice(&0.1f32.to_le_bytes());
        bytes.extend_from_slice(&0.2f32.to_le_bytes());
        push_sprite_frame(&mut bytes, -4, 4, 8, 8);
        push_sprite_frame(&mut bytes, -3, 5, 6, 7);
        let sprite = parse_sprite_model(&bytes).unwrap();
        assert_eq!(sprite.kind, 4);
        assert_eq!(sprite.frames.len(), 2);
        assert_eq!((sprite.frames[1].width, sprite.frames[1].height), (6, 7));
    }

    #[test]
    fn cooked_sprite_uses_a_valid_alias_table_and_twelve_byte_frames() {
        let sprite = SpriteModel {
            kind: 2,
            radius: 12.0,
            beam_length: 0,
            max_width: 16,
            max_height: 16,
            frames: vec![
                SpriteFrame {
                    left: -8,
                    up: 8,
                    width: 16,
                    height: 16,
                    pixels: vec![1; 256],
                },
                SpriteFrame {
                    left: -8,
                    up: 8,
                    width: 16,
                    height: 16,
                    pixels: vec![2; 256],
                },
            ],
        };
        let mut atlas = TextureAtlas::new();
        let mut payload = Vec::new();
        let header = cook_sprite_model(&sprite, 0x5d, &mut atlas, &mut payload).unwrap();
        assert_eq!(header.flags, ALIAS_MODEL_SPRITE | 2);
        assert_eq!(header.frame_count, 2);
        assert_eq!(payload.len() - header.frame_offset as usize, 24);

        let mut table = 1u32.to_le_bytes().to_vec();
        serialize_header(&header, &mut table);
        table.extend_from_slice(&payload);
        let decoded = quake_formats::AliasModelTable::new(&table).unwrap();
        let frame = decoded.get(0x5d).unwrap().frame_bytes(1).unwrap();
        assert_eq!((frame[2], frame[3], frame[10]), (16, 16, 2));
    }
}
