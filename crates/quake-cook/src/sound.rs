use std::collections::{BTreeMap, BTreeSet};

use psx_sfx::PARKING_TAIL;
use psxed_audio::cook_spu_adpcm_from_wav;
use quake_formats::{
    sound_content_hash, SoundBankHeader, SoundBankKind, SOUND_BANK_HEADER_BYTES,
    SOUND_BANK_RATE_BYTES, SOUND_BANK_RECORD_BYTES, SOUND_GLOBAL_EFFECTS, SOUND_MAX_EFFECTS,
    SOUND_SPU_BASE, SOUND_SPU_END,
};

use super::entities::SourceEntity;
use super::{CookError, CookedEntities, PakArchive};

const LEAD_IN_BYTES: usize = 16;

/// SPU playback rate per source category (the directory after `sound/`).
/// Looping ambience and the misc cues (teleport flash, secret, talk, splashes)
/// survive the lowest rate; item, door, plat, and button cues take the middle
/// one; weapons, the player, and every monster keep the full 11,025 Hz.
fn category_rate_hz(source_name: &str) -> u32 {
    // These long, low-frequency loops remain intelligible below the ordinary
    // ambience rate. Keeping their complete source timelines is preferable
    // to replacing either one with a short, conspicuously repeating crop.
    if source_name == "sound/ambience/drip1.wav" {
        return 3_000;
    }
    if matches!(
        source_name,
        "sound/ambience/hum1.wav" | "sound/ambience/swamp1.wav"
    ) {
        return 3_000;
    }
    if matches!(
        source_name,
        "sound/misc/r_tele1.wav"
            | "sound/misc/r_tele2.wav"
            | "sound/misc/r_tele3.wav"
            | "sound/misc/r_tele4.wav"
            | "sound/misc/r_tele5.wav"
    ) {
        return 4_000;
    }
    // The two default secret-door samples are unusually long. At the normal
    // 8 kHz door rate they push E1M3's otherwise-complete local bank past the
    // 512 KiB SPU ceiling; a dedicated 5 kHz rate keeps the pair intact and
    // leaves the runtime voice layout unchanged.
    if matches!(
        source_name,
        "sound/doors/basesec1.wav"
            | "sound/doors/basesec2.wav"
            | "sound/doors/latch2.wav"
            | "sound/doors/winch2.wav"
            | "sound/doors/drclos4.wav#secret"
    ) {
        return 5_000;
    }
    if source_name == "sound/items/inv2.wav" {
        return 6_000;
    }
    let category = source_name
        .strip_prefix("sound/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    match category {
        "ambience" | "misc" => 5_512,
        "plats" => 6_500,
        "items" | "doors" | "buttons" => 8_000,
        // Keep every shareware monster voice within five percent of the
        // 11,025 Hz source. Spreading this tiny reduction across the local
        // bestiary bank makes room for the three original menu cues without
        // sacrificing a sound or audibly crushing one long sample.
        "soldier" | "dog" | "ogre" | "demon" | "wizard" | "zombie" | "knight"
        | "shambler" => 10_500,
        _ => 11_025,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundCookStats {
    pub sound_count: usize,
    pub payload_bytes: usize,
    pub looping_sounds: usize,
    pub combined_sound_count: usize,
    pub combined_payload_bytes: usize,
    pub spu_high_water: u32,
    pub omitted_for_space: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedGlobalSounds {
    pub data: Vec<u8>,
    pub stats: SoundCookStats,
    content_hash: u64,
    spu_high_water: u32,
    sources: BTreeMap<u16, String>,
    encoded: Vec<EncodedSound>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedSounds {
    pub data: Vec<u8>,
    pub stats: SoundCookStats,
    encoded: Vec<EncodedSound>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SoundResource {
    primary_id: u16,
    variants: [Option<String>; 3],
    required_sounds: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EncodedSound {
    id: u16,
    frames: u16,
    address: u32,
    rate_hz: u32,
    bytes: Vec<u8>,
    looping: bool,
}

/// Cook the 37 non-variant `worldspawn` sounds once at stable SPU addresses.
pub fn cook_global_sounds(
    pak: &PakArchive<'_>,
    sound_map: &str,
    resource_list: &str,
) -> Result<CookedGlobalSounds, CookError> {
    let sounds = parse_sound_map(sound_map)?;
    let ids = sounds
        .iter()
        .enumerate()
        .filter_map(|(id, name)| name.as_ref().map(|name| (name.clone(), id as u16)))
        .collect::<BTreeMap<_, _>>();
    let resources = parse_sound_resources(resource_list, &ids)?;
    let world = resources
        .get("worldspawn")
        .ok_or_else(|| CookError::new("sound resources omit worldspawn"))?;
    if world.len() != SOUND_GLOBAL_EFFECTS {
        return Err(CookError::new(format!(
            "worldspawn has {} sounds, expected {SOUND_GLOBAL_EFFECTS}",
            world.len()
        )));
    }
    let mut selected = Vec::with_capacity(SOUND_GLOBAL_EFFECTS);
    let mut sources = BTreeMap::new();
    for resource in world {
        if resource.variants[1].is_some() || resource.variants[2].is_some() {
            return Err(CookError::new(format!(
                "worldspawn sound {:#04x} is variant-dependent and cannot be global",
                resource.primary_id
            )));
        }
        let source = resource.variants[0]
            .as_ref()
            .ok_or_else(|| CookError::new("worldspawn sound has no primary variant"))?
            .clone();
        if sources
            .insert(resource.primary_id, source.clone())
            .is_some()
        {
            return Err(CookError::new(format!(
                "duplicate global sound ID {:#04x}",
                resource.primary_id
            )));
        }
        selected.push((resource.primary_id, source));
    }
    let encoded = encode_selected(pak, selected, SOUND_SPU_BASE, SOUND_GLOBAL_EFFECTS)?;
    let (data, content_hash, spu_high_water) =
        encode_versioned_bank(SoundBankKind::Global, &encoded, SOUND_SPU_BASE, 0)?;
    let payload_bytes = encoded.iter().map(|sound| sound.bytes.len()).sum();
    Ok(CookedGlobalSounds {
        data,
        stats: SoundCookStats {
            sound_count: encoded.len(),
            payload_bytes,
            looping_sounds: encoded.iter().filter(|sound| sound.looping).count(),
            combined_sound_count: encoded.len(),
            combined_payload_bytes: payload_bytes,
            spu_high_water,
            omitted_for_space: Vec::new(),
        },
        content_hash,
        spu_high_water,
        sources,
        encoded,
    })
}

/// Cook only the selected map's suffix after the persistent global prefix.
pub fn cook_sounds(
    pak: &PakArchive<'_>,
    entities: &CookedEntities,
    sound_map: &str,
    resource_list: &str,
    global: &CookedGlobalSounds,
) -> Result<CookedSounds, CookError> {
    let sounds = parse_sound_map(sound_map)?;
    let ids = sounds
        .iter()
        .enumerate()
        .filter_map(|(id, name)| name.as_ref().map(|name| (name.clone(), id as u16)))
        .collect::<BTreeMap<_, _>>();
    let resources = parse_sound_resources(resource_list, &ids)?;
    let selected = select_sounds(&entities.source, entities.world_type, &resources)?;
    let mut saw_global = BTreeSet::new();
    let mut local = Vec::new();
    for (id, source) in selected {
        if let Some(expected) = global.sources.get(&id) {
            if expected != &source {
                return Err(CookError::new(format!(
                    "global sound {id:#04x} selected {source}, expected non-variant {expected}"
                )));
            }
            saw_global.insert(id);
        } else {
            local.push((id, source));
        }
    }
    if saw_global.len() != global.sources.len() {
        return Err(CookError::new(format!(
            "map selected {} of {} global sounds",
            saw_global.len(),
            global.sources.len()
        )));
    }
    let combined_count = global
        .encoded
        .len()
        .checked_add(local.len())
        .ok_or_else(|| CookError::new("combined sound count overflow"))?;
    if combined_count > SOUND_MAX_EFFECTS {
        return Err(CookError::new(format!(
            "map needs {combined_count} combined sounds, maximum is {SOUND_MAX_EFFECTS}"
        )));
    }
    let local_count = local.len();
    let encoded = encode_selected(pak, local, global.spu_high_water, local_count)?;
    let (data, _, spu_high_water) = encode_versioned_bank(
        SoundBankKind::Local,
        &encoded,
        global.spu_high_water,
        global.content_hash,
    )?;
    let payload_bytes = encoded.iter().map(|sound| sound.bytes.len()).sum::<usize>();
    Ok(CookedSounds {
        data,
        stats: SoundCookStats {
            sound_count: encoded.len(),
            payload_bytes,
            looping_sounds: encoded.iter().filter(|sound| sound.looping).count(),
            combined_sound_count: combined_count,
            combined_payload_bytes: global.stats.payload_bytes + payload_bytes,
            spu_high_water,
            omitted_for_space: Vec::new(),
        },
        encoded,
    })
}

fn encode_selected(
    pak: &PakArchive<'_>,
    selected: Vec<(u16, String)>,
    payload_base: u32,
    expected_max: usize,
) -> Result<Vec<EncodedSound>, CookError> {
    let mut encoded = Vec::new();
    let mut seen = BTreeSet::new();
    let mut payload_bytes = 0usize;
    for (id, source_name) in selected {
        if !seen.insert(id) {
            return Err(CookError::new(format!(
                "duplicate selected sound ID {id:#04x}"
            )));
        }
        // A suffix gives one source sample a second stable sound ID. E1M3
        // needs this because its ordinary doors already occupy drclos4's
        // legacy ID with the world-variant sample, while medieval secret
        // doors require the real drclos4 on their own callback channel.
        let pak_source_name = source_name.split_once('#').map_or(source_name.as_str(), |pair| pair.0);
        let wav = pak.require(pak_source_name)?;
        let loop_start = wav_cue_loop_start(wav).map_err(|error| {
            CookError::new(format!(
                "could not read loop metadata for {source_name}: {error}"
            ))
        })?;
        let rate_hz = category_rate_hz(&source_name);
        let cooked = cook_spu_adpcm_from_wav(wav, rate_hz, loop_start)
            .map_err(|error| CookError::new(format!("could not cook {source_name}: {error}")))?;
        if cooked.source_channels != 1 {
            return Err(CookError::new(format!(
                "Quake sound {source_name} has {} channels; expected mono",
                cooked.source_channels
            )));
        }
        let bytes_len = LEAD_IN_BYTES
            .checked_add(cooked.adpcm.len())
            .and_then(|len| len.checked_add(PARKING_TAIL.len()))
            .ok_or_else(|| CookError::new("SPU sound size overflow"))?;
        let address = payload_base
            .checked_add(payload_bytes as u32)
            .ok_or_else(|| CookError::new("SPU address overflow"))?;
        let high_water = address
            .checked_add(bytes_len as u32)
            .ok_or_else(|| CookError::new("SPU sound high-water overflow"))?;
        if high_water > SOUND_SPU_END {
            return Err(CookError::new(format!(
                "combined SPU bank reaches {high_water:#x}, exceeding {SOUND_SPU_END:#x} by {:#x} while adding {source_name}",
                high_water - SOUND_SPU_END,
            )));
        }
        if encoded.len() >= expected_max {
            return Err(CookError::new("sound selection exceeds its declared bound"));
        }

        let frames_60hz = u64::from(cooked.source_sample_count).saturating_mul(60)
            / cooked.source_sample_rate_hz as u64;
        if frames_60hz == 0 {
            return Err(CookError::new(format!(
                "Quake sound {source_name} is shorter than one tick"
            )));
        }
        let mut bytes = vec![0; LEAD_IN_BYTES];
        bytes.extend_from_slice(&cooked.adpcm);
        bytes.extend_from_slice(&PARKING_TAIL);
        payload_bytes += bytes.len();
        encoded.push(EncodedSound {
            id,
            frames: frames_60hz.min(u16::MAX as u64) as u16,
            address,
            rate_hz,
            bytes,
            looping: cooked.loop_start.is_some(),
        });
    }

    Ok(encoded)
}

fn encode_versioned_bank(
    kind: SoundBankKind,
    encoded: &[EncodedSound],
    payload_base: u32,
    dependency_hash: u64,
) -> Result<(Vec<u8>, u64, u32), CookError> {
    if encoded.is_empty() && kind == SoundBankKind::Global {
        return Err(CookError::new("global sound bank is empty"));
    }
    let table_bytes = encoded
        .len()
        .checked_mul(SOUND_BANK_RECORD_BYTES + SOUND_BANK_RATE_BYTES)
        .ok_or_else(|| CookError::new("sound table size overflow"))?;
    let payload_bytes = encoded.iter().try_fold(0usize, |total, sound| {
        total
            .checked_add(sound.bytes.len())
            .ok_or_else(|| CookError::new("sound payload overflow"))
    })?;
    let spu_high_water = payload_base
        .checked_add(payload_bytes as u32)
        .ok_or_else(|| CookError::new("SPU high-water overflow"))?;
    if spu_high_water > SOUND_SPU_END {
        return Err(CookError::new("combined SPU high-water exceeds SPU RAM"));
    }
    let mut table = Vec::with_capacity(table_bytes);
    let mut payload = Vec::with_capacity(payload_bytes);
    for sound in encoded {
        table.extend_from_slice(&(sound.id as i16).to_le_bytes());
        table.extend_from_slice(&sound.frames.to_le_bytes());
        table.extend_from_slice(&sound.address.to_le_bytes());
        payload.extend_from_slice(&sound.bytes);
    }
    for sound in encoded {
        let rate = u16::try_from(sound.rate_hz)
            .map_err(|_| CookError::new("sound rate does not fit the u16 rate table"))?;
        table.extend_from_slice(&rate.to_le_bytes());
    }
    let content_hash = sound_content_hash(&table, &payload);
    let header = SoundBankHeader {
        kind,
        record_count: encoded.len() as u16,
        payload_base,
        payload_bytes: payload_bytes as u32,
        spu_high_water,
        dependency_hash,
        content_hash,
    };
    let mut data = Vec::with_capacity(SOUND_BANK_HEADER_BYTES + table_bytes + payload_bytes);
    data.extend_from_slice(&header.encode());
    data.extend_from_slice(&table);
    data.extend_from_slice(&payload);
    Ok((data, content_hash, spu_high_water))
}

fn encode_legacy_bank(encoded: &[EncodedSound]) -> Result<Vec<u8>, CookError> {
    let table_bytes = encoded
        .len()
        .checked_mul(SOUND_BANK_RECORD_BYTES)
        .ok_or_else(|| CookError::new("sound table size overflow"))?;
    let payload_bytes = encoded.iter().map(|sound| sound.bytes.len()).sum::<usize>();
    let mut data = Vec::with_capacity(4 + table_bytes + payload_bytes);
    data.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    for sound in encoded {
        data.extend_from_slice(&(sound.id as i16).to_le_bytes());
        data.extend_from_slice(&sound.frames.to_le_bytes());
        data.extend_from_slice(&sound.address.to_le_bytes());
    }
    for sound in encoded {
        data.extend_from_slice(&sound.bytes);
    }
    Ok(data)
}

/// Reconstruct the former per-map bank for corpus parity validation.
pub fn merge_sound_banks_for_validation(
    global: &CookedGlobalSounds,
    local: &CookedSounds,
) -> Result<Vec<u8>, CookError> {
    let mut combined = global.encoded.clone();
    combined.extend_from_slice(&local.encoded);
    encode_legacy_bank(&combined)
}

/// Cook the former monolithic selection independently for host parity tests.
pub fn cook_monolithic_sounds_for_validation(
    pak: &PakArchive<'_>,
    entities: &CookedEntities,
    sound_map: &str,
    resource_list: &str,
) -> Result<Vec<u8>, CookError> {
    let sounds = parse_sound_map(sound_map)?;
    let ids = sounds
        .iter()
        .enumerate()
        .filter_map(|(id, name)| name.as_ref().map(|name| (name.clone(), id as u16)))
        .collect::<BTreeMap<_, _>>();
    let resources = parse_sound_resources(resource_list, &ids)?;
    let selected = select_sounds(&entities.source, entities.world_type, &resources)?;
    let encoded = encode_selected(pak, selected, SOUND_SPU_BASE, SOUND_MAX_EFFECTS)?;
    encode_legacy_bank(&encoded)
}

fn parse_sound_map(input: &str) -> Result<Vec<Option<String>>, CookError> {
    let mut output = vec![None; 256];
    for (line_index, line) in input.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next() else { continue };
        if id.starts_with('#') {
            continue;
        }
        let id = usize::from_str_radix(id, 16)
            .map_err(|_| CookError::new(format!("sound map line {} has bad ID", line_index + 1)))?;
        let name = fields.next().ok_or_else(|| {
            CookError::new(format!("sound map line {} has no name", line_index + 1))
        })?;
        if id == 0 || id >= output.len() {
            return Err(CookError::new(format!(
                "sound map line {} has out-of-range ID {id:#x}",
                line_index + 1
            )));
        }
        if output[id].replace(name.to_owned()).is_some() {
            return Err(CookError::new(format!("duplicate sound ID {id:#x}")));
        }
    }
    Ok(output)
}

fn parse_sound_resources(
    input: &str,
    sound_ids: &BTreeMap<String, u16>,
) -> Result<BTreeMap<String, Vec<SoundResource>>, CookError> {
    let mut output = BTreeMap::<String, Vec<SoundResource>>::new();
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
        let required_sounds = if kind == "sfx" {
            None
        } else if kind == "sfx_if_sounds" {
            Some(
                fields
                    .next()
                    .ok_or_else(|| {
                        CookError::new(format!(
                            "resource line {} has no sounds selector",
                            line_index + 1
                        ))
                    })?
                    .parse::<i32>()
                    .map_err(|_| {
                        CookError::new(format!(
                            "resource line {} has a bad sounds selector",
                            line_index + 1
                        ))
                    })?,
            )
        } else {
            continue;
        };
        let class_name = active_class.as_ref().ok_or_else(|| {
            CookError::new(format!(
                "resource line {} declares sound before an entity class",
                line_index + 1
            ))
        })?;
        let names = fields.take(3).map(str::to_owned).collect::<Vec<_>>();
        let Some(primary_name) = names.first() else {
            return Err(CookError::new(format!(
                "resource line {} has no sound name",
                line_index + 1
            )));
        };
        let primary_id = sound_ids.get(primary_name).copied().ok_or_else(|| {
            CookError::new(format!(
                "resource line {} references unknown sound {primary_name}",
                line_index + 1
            ))
        })?;
        for name in &names {
            if !sound_ids.contains_key(name) {
                return Err(CookError::new(format!(
                    "resource line {} references unknown sound {name}",
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
            .push(SoundResource {
                primary_id,
                variants,
                required_sounds,
            });
    }
    Ok(output)
}

fn select_sounds(
    entities: &[SourceEntity],
    world_type: i32,
    resources: &BTreeMap<String, Vec<SoundResource>>,
) -> Result<Vec<(u16, String)>, CookError> {
    let world_slot = usize::try_from(world_type)
        .ok()
        .filter(|&slot| slot < 3)
        .ok_or_else(|| CookError::new(format!("worldtype {world_type} is outside 0..=2")))?;
    let mut output = Vec::new();
    let mut loaded = BTreeSet::new();
    for allow_ambient in [false, true] {
        for entity in entities {
            let Some(class_resources) = resources.get(&entity.class_name) else {
                continue;
            };
            for resource in class_resources {
                if resource.required_sounds.is_some_and(|required| {
                    entity
                        .get("sounds")
                        .and_then(|value| value.parse::<i32>().ok())
                        .unwrap_or(0)
                        != required
                }) {
                    continue;
                }
                if loaded.contains(&resource.primary_id) {
                    continue;
                }
                let selected = resource.variants[world_slot]
                    .as_ref()
                    .or(resource.variants[0].as_ref())
                    .expect("sound resource always has a primary variant");
                if !allow_ambient && selected.contains("ambience") {
                    continue;
                }
                loaded.insert(resource.primary_id);
                output.push((resource.primary_id, selected.clone()));
            }
        }
    }
    Ok(output)
}

fn wav_cue_loop_start(wav: &[u8]) -> Result<Option<u32>, CookError> {
    if wav.len() < 12 || wav.get(..4) != Some(b"RIFF") || wav.get(8..12) != Some(b"WAVE") {
        return Err(CookError::new("sound is not a RIFF/WAVE file"));
    }
    let mut offset = 12usize;
    let mut saw_data = false;
    while offset + 8 <= wav.len() {
        let id = &wav[offset..offset + 4];
        let len = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(len)
            .ok_or_else(|| CookError::new("WAV chunk size overflow"))?;
        if end > wav.len() {
            if saw_data {
                // A few original Quake sounds have malformed editor metadata
                // after valid PCM. The historical decoder ignored it too.
                return Ok(None);
            }
            return Err(CookError::new("WAV chunk extends past EOF"));
        }
        if id == b"cue " {
            if len < 4 {
                return Err(CookError::new("WAV cue chunk is truncated"));
            }
            let count = u32::from_le_bytes(wav[start..start + 4].try_into().unwrap()) as usize;
            if count == 0 {
                return Ok(None);
            }
            if len < 4 + 24 {
                return Err(CookError::new("WAV cue point is truncated"));
            }
            let block_start = u32::from_le_bytes(wav[start + 20..start + 24].try_into().unwrap());
            if block_start != 0 {
                return Err(CookError::new("compressed WAV cue points are unsupported"));
            }
            let sample_offset = u32::from_le_bytes(wav[start + 24..start + 28].try_into().unwrap());
            return Ok(Some(sample_offset));
        }
        saw_data |= id == b"data";
        offset = end
            .checked_add(len & 1)
            .ok_or_else(|| CookError::new("WAV chunk alignment overflow"))?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_select_world_variant_and_defer_ambient() {
        let ids = BTreeMap::from([
            ("sound/ambience/hum.wav".to_owned(), 1),
            ("sound/door/med.wav".to_owned(), 2),
            ("sound/door/rune.wav".to_owned(), 3),
        ]);
        let resources = parse_sound_resources(
            "ent worldspawn\nsfx sound/ambience/hum.wav\nent func_door\nsfx sound/door/med.wav sound/door/rune.wav\n",
            &ids,
        )
        .unwrap();
        let entities = [
            SourceEntity {
                class_name: "worldspawn".to_owned(),
                fields: Vec::new(),
                class_id: 0,
            },
            SourceEntity {
                class_name: "func_door".to_owned(),
                fields: Vec::new(),
                class_id: 1,
            },
        ];

        assert_eq!(
            select_sounds(&entities, 1, &resources).unwrap(),
            vec![
                (2, "sound/door/rune.wav".to_owned()),
                (1, "sound/ambience/hum.wav".to_owned()),
            ]
        );
    }

    #[test]
    fn conditional_resources_follow_the_authored_sounds_selector() {
        let ids = BTreeMap::from([
            ("sound/door/base.wav".to_owned(), 1),
            ("sound/door/latch.wav".to_owned(), 2),
        ]);
        let resources = parse_sound_resources(
            "ent func_door_secret\nsfx sound/door/base.wav\nsfx_if_sounds 1 sound/door/latch.wav\n",
            &ids,
        )
        .unwrap();
        let entity = |sounds: Option<&str>| SourceEntity {
            class_name: "func_door_secret".to_owned(),
            fields: sounds
                .map(|value| vec![("sounds".to_owned(), value.to_owned())])
                .unwrap_or_default(),
            class_id: 0x0d,
        };

        assert_eq!(
            select_sounds(&[entity(None)], 0, &resources).unwrap(),
            vec![(1, "sound/door/base.wav".to_owned())]
        );
        assert_eq!(
            select_sounds(&[entity(Some("1"))], 0, &resources).unwrap(),
            vec![
                (1, "sound/door/base.wav".to_owned()),
                (2, "sound/door/latch.wav".to_owned()),
            ]
        );
    }

    #[test]
    fn many_secret_doors_and_teleporters_cook_each_shared_effect_once() {
        let ids = BTreeMap::from([
            ("sound/doors/basesec1.wav".to_owned(), 0x2e),
            ("sound/doors/basesec2.wav".to_owned(), 0x2f),
            ("sound/ambience/hum1.wav".to_owned(), 0x07),
            ("sound/ambience/swamp1.wav".to_owned(), 0x09),
        ]);
        let resources = parse_sound_resources(
            "ent func_door_secret\nsfx sound/doors/basesec1.wav\nsfx sound/doors/basesec2.wav\nent trigger_teleport\nsfx sound/ambience/hum1.wav\nent ambient_swamp1\nsfx sound/ambience/swamp1.wav\n",
            &ids,
        )
        .unwrap();
        let entity = |class_name: &str, class_id| SourceEntity {
            class_name: class_name.to_owned(),
            fields: Vec::new(),
            class_id,
        };
        let mut entities = vec![entity("worldspawn", 0)];
        // All shareware Episode 1 instances across all nine maps. Repetition
        // must not duplicate either ADPCM payload in a map-local bank.
        entities.extend((0..18).map(|_| entity("func_door_secret", 0x0d)));
        entities.extend((0..77).map(|_| entity("trigger_teleport", 0x52)));
        entities.extend((0..10).map(|_| entity("ambient_swamp1", 0x07)));

        assert_eq!(
            select_sounds(&entities, 0, &resources).unwrap(),
            vec![
                (0x2e, "sound/doors/basesec1.wav".to_owned()),
                (0x2f, "sound/doors/basesec2.wav".to_owned()),
                (0x07, "sound/ambience/hum1.wav".to_owned()),
                (0x09, "sound/ambience/swamp1.wav".to_owned()),
            ]
        );
    }

    #[test]
    fn reads_first_uncompressed_cue_point() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&40u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEcue ");
        wav.extend_from_slice(&28u32.to_le_bytes());
        wav.extend_from_slice(&1u32.to_le_bytes());
        wav.extend_from_slice(&7u32.to_le_bytes());
        wav.extend_from_slice(&7u32.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav.extend_from_slice(&4_730u32.to_le_bytes());

        assert_eq!(wav_cue_loop_start(&wav).unwrap(), Some(4_730));
    }
}
