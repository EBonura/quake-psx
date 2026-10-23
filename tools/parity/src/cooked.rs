//! The cooked side: PSB5 maps on disk, the cooker's entity rules, and the
//! cfg tables (`entmap.txt`, `reslist.txt`, `sfxmap.txt`) the cooker reads.

use std::collections::BTreeMap;
use std::path::Path;

use quake_formats::{CookedRecord, MapEntity};

use crate::pak::{Entity, Result};

/// `PSB5_MAGIC` followed by `(kind: i32, len: i32, bytes)` lumps in `LumpKind` order.
const LUMP_SOUND: usize = 1;
const LUMP_STRINGS: usize = 13;
const LUMP_ENTITIES: usize = 14;
const LUMP_COUNT: usize = 15;
const PSB5_MAGIC: u32 = 0x3558_5350;
const SOUND_HEADER_BYTES: usize = 40;
const SOUND_RECORD_BYTES: usize = 8;

/// One cooked map read back from `id1psx/maps/<name>.psb`.
pub struct CookedMap {
    pub records: Vec<MapEntity>,
    pub strings: Vec<u8>,
    /// Sound ids in the map-local bank, in bank order.
    pub local_sounds: Vec<u16>,
    pub local_payload_bytes: u32,
    pub spu_high_water: u32,
}

pub fn read_psb(bytes: &[u8]) -> Result<CookedMap> {
    if bytes.len() < 4 || u32::from_le_bytes(bytes[..4].try_into().unwrap()) != PSB5_MAGIC {
        return Err("not a PSB5 file".into());
    }
    let mut cursor = 4usize;
    let mut lumps: Vec<(i32, &[u8])> = Vec::new();
    while cursor + 8 <= bytes.len() {
        let kind = i32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let len = i32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let body = bytes
            .get(cursor + 8..cursor + 8 + len)
            .ok_or("truncated lump")?;
        lumps.push((kind, body));
        cursor += 8 + len;
    }
    if lumps.len() != LUMP_COUNT {
        return Err(format!(
            "expected {LUMP_COUNT} lumps, found {}",
            lumps.len()
        ));
    }
    for (index, (kind, _)) in lumps.iter().enumerate() {
        if *kind != index as i32 {
            return Err(format!("lump {index} has kind {kind}"));
        }
    }
    let entity_bytes = lumps[LUMP_ENTITIES].1;
    if entity_bytes.len() % MapEntity::SIZE != 0 {
        return Err("entity lump is not a whole number of records".into());
    }
    let records = entity_bytes
        .chunks_exact(MapEntity::SIZE)
        .map(MapEntity::decode)
        .collect();
    let sound = lumps[LUMP_SOUND].1;
    let (mut local_sounds, mut payload, mut high) = (Vec::new(), 0u32, 0u32);
    if sound.len() >= SOUND_HEADER_BYTES {
        let count = u16::from_le_bytes(sound[8..10].try_into().unwrap()) as usize;
        payload = u32::from_le_bytes(sound[16..20].try_into().unwrap());
        high = u32::from_le_bytes(sound[20..24].try_into().unwrap());
        for i in 0..count {
            let at = SOUND_HEADER_BYTES + i * SOUND_RECORD_BYTES;
            let id = i16::from_le_bytes(sound[at..at + 2].try_into().unwrap());
            local_sounds.push(id as u16);
        }
    }
    Ok(CookedMap {
        records,
        strings: lumps[LUMP_STRINGS].1.to_vec(),
        local_sounds,
        local_payload_bytes: payload,
        spu_high_water: high,
    })
}

/// `id name` lines of a cfg table, keyed by the hex id.
pub fn read_id_table(text: &str) -> BTreeMap<u16, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(id), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !id.as_bytes()[0].is_ascii_alphanumeric() {
            continue;
        }
        if let Ok(id) = u16::from_str_radix(id, 16) {
            out.insert(id, name.to_owned());
        }
    }
    out
}

/// One `sfx` / `sfx_if_sounds` line of the resource list.
#[derive(Clone, Debug)]
pub struct SfxRule {
    /// `Some(n)` for `sfx_if_sounds n`.
    pub when_sounds: Option<i32>,
    /// Up to three names: default, then the two worldtype variants.
    pub names: Vec<String>,
}

#[derive(Default)]
pub struct Resources {
    pub sfx: BTreeMap<String, Vec<SfxRule>>,
    pub models: BTreeMap<String, Vec<String>>,
}

pub fn read_resources(text: &str) -> Resources {
    let mut out = Resources::default();
    let mut class: Option<String> = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else { continue };
        if kind.starts_with('#') {
            continue;
        }
        match kind {
            "ent" => {
                class = fields.next().map(str::to_owned);
                if let Some(c) = &class {
                    out.sfx.entry(c.clone()).or_default();
                    out.models.entry(c.clone()).or_default();
                }
            }
            "sfx" | "sfx_if_sounds" => {
                let Some(c) = &class else { continue };
                let when = if kind == "sfx_if_sounds" {
                    fields.next().and_then(|v| v.parse().ok())
                } else {
                    None
                };
                let names: Vec<String> = fields.take(3).map(str::to_owned).collect();
                out.sfx.entry(c.clone()).or_default().push(SfxRule {
                    when_sounds: when,
                    names,
                });
            }
            "mdl" => {
                let Some(c) = &class else { continue };
                out.models
                    .entry(c.clone())
                    .or_default()
                    .extend(fields.take(3).map(str::to_owned));
            }
            _ => {}
        }
    }
    out
}

/// Classes whose spawn cooks to an alias model by default (the cooker's
/// `class_has_default_model`).
pub fn has_default_model(res: &Resources, class: &str) -> bool {
    res.models.get(class).is_some_and(|m| !m.is_empty())
}

/// Why the cooker emits no record for an original entity, if it does not.
pub fn drop_reason(entity: &Entity, res: &Resources) -> Option<&'static str> {
    match entity.class() {
        "info_player_deathmatch" | "info_player_coop" => {
            Some("deathmatch and coop starts: no multiplayer in the port")
        }
        "info_null" => Some("info_null: removed by the original at spawn (light aim target)"),
        class
            if class.starts_with("light")
                && entity.get("targetname").is_none()
                && entity.get("model").is_none()
                && !has_default_model(res, class) =>
        {
            Some("static light: baked into the lightmaps, no runtime entity")
        }
        _ => None,
    }
}

/// The cooker's `StringTable::add` for target names: a name is found if its
/// bytes plus the terminator occur anywhere in the table, even as the tail of
/// a longer name. Returns the id the cooker would give each name.
pub struct NameTable {
    bytes: Vec<u8>,
}

impl NameTable {
    pub fn new() -> Self {
        Self { bytes: vec![0] }
    }

    pub fn add(&mut self, value: &str) -> u16 {
        if value.is_empty() {
            return 0;
        }
        let mut needle = value.as_bytes().to_vec();
        needle.push(0);
        if let Some(at) = self.bytes.windows(needle.len()).position(|w| w == needle) {
            return at as u16;
        }
        let at = self.bytes.len();
        self.bytes.extend_from_slice(&needle);
        at as u16
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
