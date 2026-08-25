use std::collections::BTreeMap;

use super::{Bsp, BspLump, CookError};

const ENTITY_RECORD_BYTES: usize = 50;
const MAX_ENTITY_CLASSES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEntity {
    pub(crate) class_name: String,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) class_id: u8,
}

impl SourceEntity {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
    }

    pub fn class_name(&self) -> &str {
        &self.class_name
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedEntities {
    pub strings: Vec<u8>,
    pub entities: Vec<u8>,
    pub world_type: i32,
    pub cd_track: i8,
    pub(crate) source: Vec<SourceEntity>,
}

impl CookedEntities {
    pub fn source_entities(&self) -> &[SourceEntity] {
        &self.source
    }

    pub fn runtime_entity_count(&self) -> usize {
        self.entities.len() / ENTITY_RECORD_BYTES
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct EntityRecord {
    class_name: u8,
    noise: i8,
    spawn_flags: u16,
    model: i16,
    health: i16,
    damage: i16,
    speed: i16,
    count: i16,
    height: i16,
    target: u16,
    kill_target: u16,
    target_name: u16,
    string: u16,
    wait: i32,
    delay: i32,
    angles: [i16; 3],
    origin: [i32; 3],
}

pub fn cook_entities(
    bsp: &Bsp<'_>,
    entity_map: &str,
    model_map: &str,
    resource_list: &str,
) -> Result<CookedEntities, CookError> {
    let class_ids = parse_resource_map(entity_map, MAX_ENTITY_CLASSES)?;
    let model_ids = reverse_resource_map(model_map, 128)?;
    let text = std::str::from_utf8(bsp.lump(BspLump::Entities))
        .map_err(|_| CookError::new("BSP entity lump is not UTF-8"))?;
    let source = parse_entities(text, &class_ids)?;
    if source.first().map(|entity| entity.class_name.as_str()) != Some("worldspawn") {
        return Err(CookError::new("first BSP entity is not worldspawn"));
    }

    let mut strings = StringTable::new();
    let mut targets = StringTable::new();
    let mut output = Vec::<EntityRecord>::new();
    let mut world = EntityRecord::default();
    world.class_name = 0;
    let world_type = source[0]
        .get("worldtype")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let cd_track = source[0]
        .get("sounds")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0) as i8;
    world.noise = cd_track;
    output.push(world);
    output.push(EntityRecord {
        class_name: 1,
        ..EntityRecord::default()
    });

    for entity in source.iter().skip(1) {
        if matches!(
            entity.class_name.as_str(),
            "info_player_deathmatch" | "info_player_coop" | "info_null"
        ) {
            continue;
        }
        let is_light = entity.class_name.starts_with("light");
        if is_light
            && entity.get("targetname").is_none()
            && entity.get("model").is_none()
            && !class_has_default_model(&entity.class_name, resource_list)
        {
            continue;
        }

        let player_start = entity.class_name == "info_player_start";
        let mut record = if player_start {
            output[1]
        } else {
            EntityRecord {
                class_name: entity.class_id,
                ..EntityRecord::default()
            }
        };
        if let Some(value) = entity.get("origin") {
            record.origin = parse_vector(value)?.map(float_to_fixed32);
        }
        if let Some(value) = entity.get("angles").or_else(|| entity.get("mangle")) {
            record.angles = parse_vector(value)?.map(float_to_angle);
        } else if let Some(value) = entity.get("angle") {
            let angle = parse_float(value, "entity angle")?;
            record.angles[1] = if angle == -1.0 || angle == -2.0 {
                angle as i16
            } else {
                float_to_angle(angle)
            };
        }
        if let Some(value) = entity.get("model") {
            record.model = if let Some(brush) = value.strip_prefix('*') {
                -(parse_i32(brush, "brush model")? as i16)
            } else {
                model_ids.get(value).copied().unwrap_or(0) as i16
            };
        }
        if let Some(value) = entity.get("message") {
            record.string = strings.add(&unescape_map_string(value))?;
        }
        if let Some(value) = entity.get("map") {
            record.string = strings.add(&value.to_ascii_uppercase())?;
        }
        if let Some(value) = entity.get("target") {
            record.target = targets.add(value)?;
        }
        if let Some(value) = entity.get("killtarget") {
            record.kill_target = targets.add(value)?;
        }
        if let Some(value) = entity.get("targetname") {
            record.target_name = targets.add(value)?;
        }
        record.spawn_flags = optional_i32(entity, "spawnflags")?.unwrap_or(0) as u16;
        record.noise = optional_i32(entity, "sounds")?.unwrap_or(record.noise as i32) as i8;
        if let Some(value) = optional_i32(entity, "count")? {
            record.count = clamped_i16(value);
        }
        if let Some(value) = optional_i32(entity, "lip")? {
            record.count = clamped_i16(value);
        }
        if is_light {
            if let Some(value) = optional_i32(entity, "style")? {
                record.count = clamped_i16(value);
            }
        }
        record.damage = clamped_i16(optional_i32(entity, "dmg")?.unwrap_or(0));
        record.speed = clamped_i16(optional_i32(entity, "speed")?.unwrap_or(0));
        record.height = clamped_i16(optional_i32(entity, "height")?.unwrap_or(0));
        record.health = clamped_i16(optional_i32(entity, "health")?.unwrap_or(0));
        record.wait = optional_float(entity, "wait")?
            .map(float_to_fixed32)
            .unwrap_or(0);
        record.delay = optional_float(entity, "delay")?
            .map(float_to_fixed32)
            .unwrap_or(0);
        if player_start {
            output[1] = record;
        } else {
            output.push(record);
        }
    }
    let entities = serialize_entities(&output);
    debug_assert_eq!(entities.len(), output.len() * ENTITY_RECORD_BYTES);
    Ok(CookedEntities {
        strings: strings.bytes,
        entities,
        world_type,
        cd_track,
        source,
    })
}

fn parse_entities(
    input: &str,
    class_ids: &[Option<String>],
) -> Result<Vec<SourceEntity>, CookError> {
    let mut tokens = Tokenizer::new(input);
    let mut output = Vec::new();
    while let Some(token) = tokens.next()? {
        if token != "{" {
            return Err(CookError::new("entity does not begin with '{'"));
        }
        let mut fields = Vec::new();
        loop {
            let key = tokens
                .next()?
                .ok_or_else(|| CookError::new("unterminated entity"))?;
            if key == "}" {
                break;
            }
            let value = tokens
                .next()?
                .ok_or_else(|| CookError::new("entity field has no value"))?;
            if value == "}" {
                return Err(CookError::new("entity field has no value"));
            }
            fields.push((key, value));
        }
        let class_name = fields
            .iter()
            .find_map(|(key, value)| (key == "classname").then_some(value.clone()))
            .ok_or_else(|| CookError::new("entity has no classname"))?;
        let class_id = class_ids
            .iter()
            .position(|candidate| candidate.as_deref() == Some(&class_name))
            .ok_or_else(|| CookError::new(format!("unknown entity class {class_name}")))?
            as u8;
        output.push(SourceEntity {
            class_name,
            fields,
            class_id,
        });
    }
    Ok(output)
}

struct Tokenizer<'a> {
    rest: &'a str,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self { rest: input }
    }

    fn next(&mut self) -> Result<Option<String>, CookError> {
        loop {
            self.rest = self
                .rest
                .trim_start_matches(|character: char| character <= ' ');
            if self.rest.is_empty() {
                return Ok(None);
            }
            if self.rest.starts_with("//") {
                self.rest = self
                    .rest
                    .split_once('\n')
                    .map(|(_, rest)| rest)
                    .unwrap_or("");
                continue;
            }
            break;
        }
        if let Some(quoted) = self.rest.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or_else(|| CookError::new("unterminated quoted entity token"))?;
            let token = quoted[..end].to_owned();
            self.rest = &quoted[end + 1..];
            return Ok(Some(token));
        }
        let first = self.rest.as_bytes()[0] as char;
        if "{}()':".contains(first) {
            self.rest = &self.rest[first.len_utf8()..];
            return Ok(Some(first.to_string()));
        }
        let end = self
            .rest
            .char_indices()
            .find_map(|(index, character)| {
                (character <= ' ' || "{}()':".contains(character)).then_some(index)
            })
            .unwrap_or(self.rest.len());
        let token = self.rest[..end].to_owned();
        self.rest = &self.rest[end..];
        Ok(Some(token))
    }
}

struct StringTable {
    bytes: Vec<u8>,
}

impl StringTable {
    fn new() -> Self {
        Self { bytes: vec![0] }
    }

    fn add(&mut self, value: &str) -> Result<u16, CookError> {
        if value.is_empty() {
            return Ok(0);
        }
        let mut needle = value.as_bytes().to_vec();
        needle.push(0);
        if let Some(offset) = self
            .bytes
            .windows(needle.len())
            .position(|candidate| candidate == needle)
        {
            return Ok(offset as u16);
        }
        let offset = self.bytes.len();
        let end = offset
            .checked_add(needle.len())
            .ok_or_else(|| CookError::new("string table overflow"))?;
        if end > 8192 {
            return Err(CookError::new("string table exceeds 8192 bytes"));
        }
        self.bytes.extend_from_slice(&needle);
        Ok(offset as u16)
    }
}

fn parse_resource_map(input: &str, maximum: usize) -> Result<Vec<Option<String>>, CookError> {
    let mut output = vec![None; maximum];
    for (line_index, line) in input.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next() else { continue };
        if !id.as_bytes()[0].is_ascii_alphanumeric() {
            continue;
        }
        let id = usize::from_str_radix(id, 16).map_err(|_| {
            CookError::new(format!("resource map line {} has bad ID", line_index + 1))
        })?;
        let Some(name) = fields.next() else { continue };
        if id < maximum {
            output[id] = Some(name.to_owned());
        }
    }
    Ok(output)
}

fn reverse_resource_map(input: &str, maximum: usize) -> Result<BTreeMap<String, u8>, CookError> {
    let forward = parse_resource_map(input, maximum)?;
    Ok(forward
        .into_iter()
        .enumerate()
        .filter_map(|(id, name)| name.map(|name| (name, id as u8)))
        .collect())
}

fn class_has_default_model(class_name: &str, resources: &str) -> bool {
    let mut active = None;
    for line in resources.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("ent") => active = fields.next(),
            Some("mdl") if active == Some(class_name) => return true,
            _ => {}
        }
    }
    false
}

fn optional_i32(entity: &SourceEntity, key: &str) -> Result<Option<i32>, CookError> {
    entity
        .get(key)
        .map(|value| parse_i32(value, key))
        .transpose()
}

fn optional_float(entity: &SourceEntity, key: &str) -> Result<Option<f32>, CookError> {
    entity
        .get(key)
        .map(|value| parse_float(value, key))
        .transpose()
}

fn parse_i32(value: &str, context: &str) -> Result<i32, CookError> {
    value
        .parse()
        .map_err(|_| CookError::new(format!("bad {context}: {value}")))
}

fn parse_float(value: &str, context: &str) -> Result<f32, CookError> {
    value
        .parse()
        .map_err(|_| CookError::new(format!("bad {context}: {value}")))
}

fn parse_vector(value: &str) -> Result<[f32; 3], CookError> {
    let values = value
        .split_whitespace()
        .map(|part| parse_float(part, "entity vector"))
        .collect::<Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| CookError::new(format!("bad entity vector: {value}")))
}

fn clamped_i16(value: i32) -> i16 {
    value.min(i16::MAX as i32) as i16
}

fn float_to_fixed32(value: f32) -> i32 {
    (value * 4096.0) as i32
}

fn float_to_angle(value: f32) -> i16 {
    (value * 4096.0 / 360.0) as i16
}

fn unescape_map_string(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('r') => output.push('\r'),
                Some('t') => output.push('\t'),
                Some(next) => {
                    output.push(character);
                    output.push(next);
                }
                None => output.push(character),
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn serialize_entities(entities: &[EntityRecord]) -> Vec<u8> {
    let mut output = Vec::with_capacity(entities.len() * ENTITY_RECORD_BYTES);
    for entity in entities {
        output.extend_from_slice(&[entity.class_name, entity.noise as u8]);
        output.extend_from_slice(&entity.spawn_flags.to_le_bytes());
        for value in [
            entity.model,
            entity.health,
            entity.damage,
            entity.speed,
            entity.count,
            entity.height,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in [
            entity.target,
            entity.kill_target,
            entity.target_name,
            entity.string,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        output.extend_from_slice(&entity.wait.to_le_bytes());
        output.extend_from_slice(&entity.delay.to_le_bytes());
        for value in entity.angles {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in entity.origin {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_matches_quoted_quake_entities() {
        let mut tokenizer = Tokenizer::new("{ \"classname\" \"worldspawn\" }");
        assert_eq!(tokenizer.next().unwrap().as_deref(), Some("{"));
        assert_eq!(tokenizer.next().unwrap().as_deref(), Some("classname"));
        assert_eq!(tokenizer.next().unwrap().as_deref(), Some("worldspawn"));
        assert_eq!(tokenizer.next().unwrap().as_deref(), Some("}"));
        assert_eq!(tokenizer.next().unwrap(), None);
    }

    #[test]
    fn string_table_deduplicates_exact_strings() {
        let mut table = StringTable::new();
        assert_eq!(table.add("door").unwrap(), 1);
        assert_eq!(table.add("door").unwrap(), 1);
        assert_eq!(table.add("or").unwrap(), 3);
    }
}
