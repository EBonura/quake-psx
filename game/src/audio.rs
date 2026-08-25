//! Persistent-global plus map-local Quake sound banks over PSoXide's SPU API.

use alloc::vec::Vec;
use core::mem::MaybeUninit;

use psx_math::int32::isqrt_i32;
use psx_sfx::{LoopingSample, OneShot, Player, Sample};
use psx_spu::{SpuAddr, Voice, Volume};
use quake_core::menu::{DEFAULT_SOUND_VOLUME, VOLUME_STEPS};
use quake_formats::{
    sound_hash_extend, validate_sound_records, LumpKind, SoundBankHeader, SoundBankKind, Vec3I32,
    SOUND_BANK_HEADER_BYTES, SOUND_GLOBAL_EFFECTS, SOUND_HASH_OFFSET, SOUND_MAX_EFFECTS,
    SOUND_SPU_BASE, SOUND_SPU_END,
};

use crate::asset::{ResidentMap, STREAM_SCRATCH_BYTES};
use crate::entity::EntityScene;
use crate::platform::{self, StorageError};

const GLOBAL_SOUND_CHUNK: u32 = 3;
const VIDEO_TICKS_HZ: u32 = 60;
const SAMPLES_PER_ADPCM_BLOCK: u32 = 28;
const MAX_AMBIENT_VOICES: usize = 11;
const AMBIENT_FIRST_VOICE: u8 = 1;
// E1M4 authors 31 ordinary ambience points. Equal samples share one hardware
// voice which follows the nearest source, so all positions remain represented
// without consuming the dynamic voice pool.
const MAX_AMBIENT_SOURCES: usize = 32;
// Shareware's authored maximum is E1M5's fifteen teleporters. Keep one spare
// slot while avoiding a second copy of the gameplay pool's unused capacity.
const MAX_TELEPORT_HUMS: usize = 16;
const TELEPORT_HUM_SOUND: i16 = 0x07;
const TELEPORT_HUM_VOLUME: i32 = QUAKE_MAX_VOLUME / 2;
// Voice 0 is music, 1..=11 are static loops, and 12..=23 remain dynamic.
const _: () = assert!(AMBIENT_FIRST_VOICE as usize + MAX_AMBIENT_VOICES == 12);
const Q12_ONE: i32 = 4096;
/// `S_StaticSound` hands `ambientsound`'s `ATTN_STATIC` to the same ramp a
/// one-shot uses, so a map ambient fades out over `1000 / 3` units rather
/// than the ad-hoc clip this port used to give it.
const AMBIENT_ATTENUATION_Q20: i32 = Attenuation::Static.q20();
/// `sound_nominal_clip_dist`: an `ATTN_NORM` voice is silent 1000 units out.
const NOMINAL_CLIP_DISTANCE: i32 = 1000;
const QUAKE_MAX_VOLUME: i32 = 250;
/// Calibrated full slider level of a one-shot, out of 256. At the original
/// `volume 0.7` default this becomes 128: the port's historic half-scale mix.
const ONE_SHOT_MAX_VOLUME: i32 = 183;
const DYNAMIC_VOICES: [Voice; 12] = [
    Voice::new(12),
    Voice::new(13),
    Voice::new(14),
    Voice::new(15),
    Voice::new(16),
    Voice::new(17),
    Voice::new(18),
    Voice::new(19),
    Voice::new(20),
    Voice::new(21),
    Voice::new(22),
    Voice::new(23),
];

/// `S_StartSound` channels. `CHAN_AUTO` never overrides a sound already
/// playing; every other channel cuts the same owner's sound on that channel.
pub const CHAN_AUTO: u16 = 0;
pub const CHAN_WEAPON: u16 = 1;
pub const CHAN_VOICE: u16 = 2;
pub const CHAN_ITEM: u16 = 3;
pub const CHAN_BODY: u16 = 4;

/// `world`: doors, plats, triggers, and every impact with no live emitter.
pub const OWNER_WORLD: u16 = 0;
/// `cl.viewentity`. The player is not a cooked entity in this port, so it
/// takes the top id the thirteen-bit owner field can hold; no map authors
/// that many entities, so it cannot collide with a real one.
pub const OWNER_PLAYER: u16 = 0x1fff;

/// `S_StartSound`'s `(entnum, entchannel)` pair in one halfword, so a
/// `SoundEvent` can carry it without a third word.
pub const fn sound_key(owner: u16, channel: u16) -> u16 {
    ((owner & 0x1fff) << 3) | (channel & 0x0007)
}

/// `S_StartSound` attenuation for a positional one-shot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Attenuation {
    /// `ATTN_NORM`: gone at 1000 units.
    Norm,
    /// `ATTN_IDLE`: monster idle voices, gone at 500 units.
    Idle,
    /// `ATTN_STATIC`: looping map ambience, gone at about 333 units.
    Static,
}

impl Attenuation {
    const fn q20(self) -> i32 {
        let attenuation = match self {
            Self::Norm => 1,
            Self::Idle => 2,
            Self::Static => 3,
        };
        attenuation * (Q12_ONE << 8) / NOMINAL_CLIP_DISTANCE
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AudioLoadError {
    Storage(StorageError),
    Format,
    TooManySounds,
    TooManyAmbients,
    MissingAmbient(i16),
    MissingGlobal,
    DependencyMismatch,
    DuplicateSound(i16),
    BadAddress,
    BankTooLarge,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AudioPhaseTelemetry {
    pub source_bytes: u32,
    pub uploaded_bytes: u32,
    pub read_sessions: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AudioLoadOutcome {
    pub phase: AudioPhaseTelemetry,
    pub resident_hit: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Exact immutable-disc generation key. Identical suffix bytes in a different
/// chunk intentionally miss: cross-map hits need a future desired-hash index
/// which can be consulted without touching the CD.
struct LocalBankSource {
    chunk_id: u32,
    offset: u32,
    len: u32,
    global_hash: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct LocalBankIdentity {
    source: LocalBankSource,
    content_hash: u64,
}

/// One resident bank record plus its cooked playback rate.
#[derive(Copy, Clone)]
struct LoadedEffect {
    id: i16,
    frames: u16,
    spu_address: u32,
    rate_hz: u32,
}

pub struct AudioBank {
    effects: Vec<LoadedEffect>,
    global_effect_count: usize,
    global_content_hash: u64,
    global_spu_high_water: u32,
    local_identity: Option<LocalBankIdentity>,
    player: Player<12>,
    /// `channels[]`: what owns each dynamic voice and the tick its sample
    /// runs out on, which is all `SND_PickChannel` needs. One word each:
    /// the `sound_key` in the low half, the cutoff tick in the high half.
    channels: [u32; DYNAMIC_VOICES.len()],
    ambients: Vec<AmbientChannel>,
    ambient_sources: [MaybeUninit<AmbientSource>; MAX_AMBIENT_SOURCES],
    ambient_source_count: u8,
    /// Fixed map-lifetime reservation for authored hum centers. This mirrors
    /// the already bounded gameplay teleporter pool without a heap allocation.
    teleporter_hums: [MaybeUninit<Vec3I32>; MAX_TELEPORT_HUMS],
    teleporter_hum_count: u8,
    /// Last `spatialize` listener, shared by ambience and positional shots.
    listener: Vec3I32,
    listener_yaw: i16,
    /// Original `volume`, in the Options menu's `0..=10` slider steps.
    sfx_volume: u8,
}

#[derive(Copy, Clone)]
struct AmbientChannel {
    voice: Voice,
    sound_id: i16,
    origin: Vec3I32,
    volume: i32,
}

#[derive(Copy, Clone)]
struct AmbientSource {
    sound_id: i16,
    origin: Vec3I32,
    volume: i32,
}

impl AudioBank {
    pub fn new() -> Self {
        Self {
            effects: Vec::with_capacity(SOUND_MAX_EFFECTS),
            global_effect_count: 0,
            global_content_hash: 0,
            global_spu_high_water: 0,
            local_identity: None,
            player: Player::new(DYNAMIC_VOICES, VIDEO_TICKS_HZ),
            channels: [0; DYNAMIC_VOICES.len()],
            ambients: Vec::with_capacity(MAX_AMBIENT_VOICES),
            ambient_sources: [MaybeUninit::uninit(); MAX_AMBIENT_SOURCES],
            ambient_source_count: 0,
            teleporter_hums: [MaybeUninit::uninit(); MAX_TELEPORT_HUMS],
            teleporter_hum_count: 0,
            listener: Vec3I32::default(),
            listener_yaw: 0,
            sfx_volume: DEFAULT_SOUND_VOLUME,
        }
    }

    /// Load the versioned global catalog exactly once at boot.
    #[optimize(size)]
    pub fn load_global(
        &mut self,
        scratch: &mut Vec<u8>,
    ) -> Result<AudioLoadOutcome, AudioLoadError> {
        let len = platform::chunk_size(GLOBAL_SOUND_CHUNK).map_err(AudioLoadError::Storage)?;
        self.reset_voices();
        self.effects.clear();
        self.local_identity = None;
        let loaded = load_versioned_bank(
            GLOBAL_SOUND_CHUNK,
            0,
            len,
            SoundBankKind::Global,
            SOUND_SPU_BASE,
            0,
            0,
            &mut self.effects,
            scratch,
        );
        let (header, phase) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.effects.clear();
                self.global_effect_count = 0;
                self.global_content_hash = 0;
                self.global_spu_high_water = 0;
                return Err(error);
            }
        };
        if self.effects.len() != SOUND_GLOBAL_EFFECTS {
            self.effects.clear();
            return Err(AudioLoadError::Format);
        }
        self.global_effect_count = self.effects.len();
        self.global_content_hash = header.content_hash;
        self.global_spu_high_water = header.spu_high_water;
        trace_phase("global", phase, false);
        Ok(AudioLoadOutcome {
            phase,
            resident_hit: false,
        })
    }

    /// Load a map-local suffix or reuse the exact suffix already in SPU RAM.
    /// A resident hit skips all CD and SPU payload work but deliberately resets
    /// voices, the dynamic player, and authored ambient channels.
    #[optimize(size)]
    pub fn load_map(
        &mut self,
        map: &ResidentMap,
        listener: Vec3I32,
        yaw: i16,
        entities: &EntityScene,
        scratch: &mut Vec<u8>,
    ) -> Result<AudioLoadOutcome, AudioLoadError> {
        // The map loader retains this 30 KiB transfer buffer after its VRAM
        // uploads. Reusing it avoids a second 16 KiB heap allocation and lets
        // audio issue fewer, larger reads while still stopping the CD before
        // each SPU DMA.
        if scratch.capacity() < STREAM_SCRATCH_BYTES {
            return Err(AudioLoadError::Format);
        }
        if self.global_effect_count != SOUND_GLOBAL_EFFECTS
            || self.global_content_hash == 0
            || self.global_spu_high_water <= SOUND_SPU_BASE
        {
            return Err(AudioLoadError::MissingGlobal);
        }
        let episode = map.map();
        let lump = map
            .source_lump(LumpKind::SoundData)
            .ok_or(AudioLoadError::Format)?;
        if lump.len < SOUND_BANK_HEADER_BYTES as u32 {
            return Err(AudioLoadError::Format);
        }
        let source = LocalBankSource {
            chunk_id: episode.chunk_id(),
            offset: lump.offset,
            len: lump.len,
            global_hash: self.global_content_hash,
        };
        self.reset_voices();
        self.teleporter_hum_count = entities.copy_teleporter_hums(&mut self.teleporter_hums) as u8;
        if self
            .local_identity
            .is_some_and(|identity| identity.source == source && identity.content_hash != 0)
        {
            self.rebuild_ambients(map, listener, yaw)?;
            let phase = AudioPhaseTelemetry::default();
            trace_phase("local", phase, true);
            return Ok(AudioLoadOutcome {
                phase,
                resident_hit: true,
            });
        }

        // From this point onward the old suffix may be overwritten. Invalidate
        // its generation before any upload so a recoverable failure cannot
        // falsely reuse partially replaced SPU bytes.
        self.local_identity = None;
        self.effects.truncate(self.global_effect_count);
        let loaded = load_versioned_bank(
            episode.chunk_id(),
            lump.offset,
            lump.len,
            SoundBankKind::Local,
            self.global_spu_high_water,
            self.global_content_hash,
            self.global_effect_count,
            &mut self.effects,
            scratch,
        );
        let (header, phase) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.effects.truncate(self.global_effect_count);
                return Err(error);
            }
        };
        if header.spu_high_water > SOUND_SPU_END {
            self.effects.truncate(self.global_effect_count);
            return Err(AudioLoadError::BankTooLarge);
        }
        if let Err(error) = self.rebuild_ambients(map, listener, yaw) {
            self.effects.truncate(self.global_effect_count);
            return Err(error);
        }
        self.local_identity = Some(LocalBankIdentity {
            source,
            content_hash: header.content_hash,
        });
        trace_phase("local", phase, false);
        Ok(AudioLoadOutcome {
            phase,
            resident_hit: false,
        })
    }

    #[optimize(size)]
    fn reset_voices(&mut self) {
        Voice::key_off(0x00ff_ffff);
        self.player.silence_all();
        self.player = Player::new(DYNAMIC_VOICES, VIDEO_TICKS_HZ);
        self.channels = [0; DYNAMIC_VOICES.len()];
        self.ambients.clear();
        self.ambient_source_count = 0;
        self.teleporter_hum_count = 0;
    }

    #[optimize(size)]
    fn rebuild_ambients(
        &mut self,
        map: &ResidentMap,
        listener: Vec3I32,
        yaw: i16,
    ) -> Result<(), AudioLoadError> {
        self.ambient_source_count = 0;
        for entity in map.entities().iter().skip(2) {
            let Some((sound_id, volume)) = ambient_sound(entity.class_name) else {
                continue;
            };
            let slot = usize::from(self.ambient_source_count);
            if slot == self.ambient_sources.len() {
                return Err(AudioLoadError::TooManyAmbients);
            }
            self.ambient_sources[slot].write(AmbientSource {
                sound_id,
                origin: entity.origin,
                volume,
            });
            self.ambient_source_count += 1;
        }

        for index in 0..usize::from(self.ambient_source_count) {
            // `rebuild_ambients` initialized exactly this counted prefix.
            let source = unsafe { *self.ambient_sources[index].assume_init_ref() };
            if self.ambient_sources[..index].iter().any(|other| {
                // The preceding prefix is initialized by the same loop.
                unsafe { other.assume_init_ref().sound_id == source.sound_id }
            }) {
                continue;
            }
            if self.ambients.len() == MAX_AMBIENT_VOICES {
                return Err(AudioLoadError::TooManyAmbients);
            }
            let effect = self
                .effects
                .iter()
                .find(|effect| effect.id == source.sound_id)
                .copied()
                .ok_or(AudioLoadError::MissingAmbient(source.sound_id))?;
            let voice = Voice::new(AMBIENT_FIRST_VOICE + self.ambients.len() as u8);
            LoopingSample::new(
                Sample::resident(SpuAddr::new(effect.spu_address), effect.rate_hz, 0),
                Volume::SILENCE,
            )
            .play(voice);
            self.ambients.push(AmbientChannel {
                voice,
                sound_id: source.sound_id,
                origin: self
                    .nearest_ambient_origin(source.sound_id, listener)
                    .unwrap_or(source.origin),
                volume: source.volume,
            });
        }
        let teleporter_hum = self.nearest_teleporter_hum(listener);
        if let Some(origin) = teleporter_hum {
            if self.ambients.len() == MAX_AMBIENT_VOICES {
                return Err(AudioLoadError::TooManyAmbients);
            }
            let effect = self
                .effects
                .iter()
                .find(|effect| effect.id == TELEPORT_HUM_SOUND)
                .copied()
                .ok_or(AudioLoadError::MissingAmbient(TELEPORT_HUM_SOUND))?;
            let voice = Voice::new(AMBIENT_FIRST_VOICE + self.ambients.len() as u8);
            LoopingSample::new(
                Sample::resident(SpuAddr::new(effect.spu_address), effect.rate_hz, 0),
                Volume::SILENCE,
            )
            .play(voice);
            self.ambients.push(AmbientChannel {
                voice,
                sound_id: TELEPORT_HUM_SOUND,
                origin,
                volume: TELEPORT_HUM_VOLUME,
            });
        }
        self.spatialize(listener, yaw);
        Ok(())
    }

    pub fn contains(&self, id: i16) -> bool {
        self.effects.iter().any(|effect| effect.id == id)
    }

    /// Apply the original sound-volume slider. Existing static voices are
    /// re-spatialized immediately; the next one-shot uses the new gain.
    #[optimize(size)]
    pub fn set_sfx_volume(&mut self, level: u8) {
        let level = level.min(VOLUME_STEPS);
        if level == self.sfx_volume {
            return;
        }
        self.sfx_volume = level;
        self.spatialize(self.listener, self.listener_yaw);
    }

    #[optimize(size)]
    const fn scaled_sfx_volume(&self, volume: i32) -> i32 {
        volume * self.sfx_volume as i32 / VOLUME_STEPS as i32
    }

    /// Retire completed voices using the SDK's silicon-safe cutoff policy.
    pub fn tick(&mut self, video_tick: u32) {
        self.player.tick(video_tick);
    }

    /// Update looping map ambience after the listener actually moves or yaws.
    #[inline(never)]
    pub fn spatialize(&mut self, listener: Vec3I32, yaw: i16) {
        for index in 0..self.ambients.len() {
            let sound_id = self.ambients[index].sound_id;
            let origin = self.nearest_ambient_origin(sound_id, listener).or_else(|| {
                (sound_id == TELEPORT_HUM_SOUND)
                    .then(|| self.nearest_teleporter_hum(listener))
                    .flatten()
            });
            if let Some(origin) = origin {
                let ambient = &mut self.ambients[index];
                ambient.origin = origin;
            }
        }
        self.listener = listener;
        self.listener_yaw = yaw;
        for ambient in &self.ambients {
            let (left, right) = spatial_volumes(
                ambient.origin,
                self.scaled_sfx_volume(ambient.volume),
                AMBIENT_ATTENUATION_Q20,
                listener,
                yaw,
            )
            .unwrap_or((Volume::SILENCE, Volume::SILENCE));
            ambient.voice.set_volume(left, right);
        }
    }

    #[optimize(size)]
    #[inline(never)]
    fn nearest_ambient_origin(&self, sound_id: i16, listener: Vec3I32) -> Option<Vec3I32> {
        let mut selected = None;
        let mut selected_distance = u32::MAX;
        for source in &self.ambient_sources[..usize::from(self.ambient_source_count)] {
            // `rebuild_ambients` initialized exactly this counted prefix.
            let source = unsafe { *source.assume_init_ref() };
            if source.sound_id != sound_id {
                continue;
            }
            let distance = quake_core::teleport::teleporter_hum_distance(listener, source.origin);
            if distance < selected_distance {
                selected = Some(source.origin);
                selected_distance = distance;
            }
        }
        selected
    }

    #[optimize(size)]
    #[inline(never)]
    fn nearest_teleporter_hum(&self, listener: Vec3I32) -> Option<Vec3I32> {
        let mut selected = None;
        let mut selected_distance = u32::MAX;
        for origin in &self.teleporter_hums[..usize::from(self.teleporter_hum_count)] {
            // `copy_teleporter_hums` initialized exactly this counted prefix.
            let origin = unsafe { *origin.assume_init_ref() };
            let distance = quake_core::teleport::teleporter_hum_distance(listener, origin);
            if distance < selected_distance {
                selected = Some(origin);
                selected_distance = distance;
            }
        }
        selected
    }

    #[cfg(feature = "ambient-regression")]
    pub fn regression_ambient_origin(&self) -> Option<Vec3I32> {
        self.ambients
            .iter()
            .find(|ambient| ambient.sound_id == 0x03)
            .or_else(|| self.ambients.first())
            .map(|ambient| ambient.origin)
    }

    /// `SND_PickChannel`. A sound on a real channel always cuts the same
    /// owner's sound on that channel; otherwise the voice with the least time
    /// left to play is taken, except that a monster never steals one still
    /// playing a player-owned sound. `None` when every voice is protected,
    /// which is the original's `first_to_die == -1` drop.
    #[inline(never)]
    fn pick_channel(&self, key: u16, video_tick: u32) -> Option<usize> {
        // Channel 7 is never authored, so `0xffff` is a key no voice can
        // hold and `0x1fff` shifted is an owner none can match. Folding the
        // two rules into unmatchable sentinels keeps the loop body single:
        // as plain flags LLVM unswitches this into four copies of itself.
        let reuse_key = if key & 0x0007 == 0 { 0xffff } else { key };
        let guarded = if key >> 3 == OWNER_PLAYER {
            0xffff
        } else {
            OWNER_PLAYER
        };
        let now = video_tick as u16;
        let mut chosen = usize::MAX;
        let mut life_left = i32::MAX;
        // The bound is opaque so this stays a loop: unrolled over twelve
        // voices it costs three kilobytes of I-cache for nothing.
        let count = core::hint::black_box(self.channels.len());
        for (slot, &channel) in self.channels[..count].iter().enumerate() {
            let owner = channel as u16;
            if owner == reuse_key {
                return Some(slot);
            }
            // A wrapping halfword difference read as signed: negative once
            // the sample has finished, so a free slot always sorts first.
            let remaining = i32::from(((channel >> 16) as u16).wrapping_sub(now) as i16);
            if remaining < life_left && !(remaining > 0 && owner >> 3 == guarded) {
                life_left = remaining;
                chosen = slot;
            }
        }
        if chosen == usize::MAX {
            None
        } else {
            Some(chosen)
        }
    }

    /// Play a non-positional one-shot on the player's own `CHAN_AUTO` and
    /// return the voice mask it used.
    pub fn play_one_shot(&mut self, id: i16, video_tick: u32) -> Option<u32> {
        self.play_one_shot_on(id, sound_key(OWNER_PLAYER, CHAN_AUTO), video_tick)
    }

    /// The same, keyed by [`sound_key`] so a later sound from the same owner
    /// and channel cuts this one instead of taking a second voice.
    #[inline(never)]
    pub fn play_one_shot_on(&mut self, id: i16, key: u16, video_tick: u32) -> Option<u32> {
        let volume = self.scaled_sfx_volume(ONE_SHOT_MAX_VOLUME) as u16;
        self.play_one_shot_volumes(
            id,
            Volume::linear(volume, 256),
            Volume::linear(volume, 256),
            key,
            video_tick,
        )
    }

    /// `S_Spatialize` for a world-owned one-shot: distance ramp from the last
    /// `spatialize` listener and a stereo pan by its yaw. A shot that would be
    /// silent on both sides takes no voice at all.
    #[inline(never)]
    pub fn play_one_shot_at(
        &mut self,
        id: i16,
        origin: Vec3I32,
        attenuation: Attenuation,
        key: u16,
        video_tick: u32,
    ) -> Option<u32> {
        let (left, right) = spatial_volumes(
            origin,
            self.scaled_sfx_volume(ONE_SHOT_MAX_VOLUME),
            attenuation.q20(),
            self.listener,
            self.listener_yaw,
        )?;
        self.play_one_shot_volumes(id, left, right, key, video_tick)
    }

    fn play_one_shot_volumes(
        &mut self,
        id: i16,
        left: Volume,
        right: Volume,
        key: u16,
        video_tick: u32,
    ) -> Option<u32> {
        let Some(effect) = self.effects.iter().find(|effect| effect.id == id).copied() else {
            return None;
        };

        // The legacy wire record stores duration in 60 Hz ticks. Recover a
        // conservative block count for psx-sfx's cutoff clock. Its two-tick
        // safety margin covers the sub-tick precision discarded by cooking.
        let samples = u32::from(effect.frames)
            .saturating_mul(effect.rate_hz)
            .div_ceil(VIDEO_TICKS_HZ);
        let blocks = samples.div_ceil(SAMPLES_PER_ADPCM_BLOCK).max(1);
        let shot = OneShot::new(
            Sample::resident(SpuAddr::new(effect.spu_address), effect.rate_hz, blocks),
            left,
        );
        let slot = self.pick_channel(key, video_tick)?;
        self.player.play_on(slot, &shot, video_tick);
        let end = (video_tick as u16).wrapping_add(shot.ticks(VIDEO_TICKS_HZ).unwrap_or(0) as u16);
        self.channels[slot] = (u32::from(end) << 16) | u32::from(key);
        let voice = self.player.voice(slot);
        if right != left {
            // `OneShot` keys on with one level for both ears; the pan lands
            // before the envelope has left its attack.
            voice.set_volume(left, right);
        }
        Some(voice.mask())
    }
}

#[optimize(size)]
#[allow(clippy::too_many_arguments)]
fn load_versioned_bank(
    chunk_id: u32,
    offset: u32,
    len: u32,
    expected_kind: SoundBankKind,
    expected_base: u32,
    dependency_hash: u64,
    retain_count: usize,
    effects: &mut Vec<LoadedEffect>,
    scratch: &mut Vec<u8>,
) -> Result<(SoundBankHeader, AudioPhaseTelemetry), AudioLoadError> {
    if scratch.capacity() < STREAM_SCRATCH_BYTES || len < SOUND_BANK_HEADER_BYTES as u32 {
        return Err(AudioLoadError::Format);
    }
    let first_read_bytes = (len as usize).min(STREAM_SCRATCH_BYTES);
    scratch.clear();
    scratch.resize(first_read_bytes, 0);
    platform::read_chunk_exact(chunk_id, offset, scratch).map_err(AudioLoadError::Storage)?;
    let mut phase = AudioPhaseTelemetry {
        source_bytes: first_read_bytes as u32,
        uploaded_bytes: 0,
        read_sessions: 1,
    };
    let header = SoundBankHeader::decode(scratch).map_err(|_| AudioLoadError::Format)?;
    if header.kind != expected_kind
        || header.payload_base != expected_base
        || header.dependency_hash != dependency_hash
        || header.file_bytes().map_err(|_| AudioLoadError::Format)? != len as usize
    {
        return Err(if header.dependency_hash != dependency_hash {
            AudioLoadError::DependencyMismatch
        } else {
            AudioLoadError::Format
        });
    }
    if header.spu_high_water > SOUND_SPU_END {
        return Err(AudioLoadError::BankTooLarge);
    }
    let payload_offset = header
        .payload_offset()
        .map_err(|_| AudioLoadError::Format)?;
    let table = scratch
        .get(SOUND_BANK_HEADER_BYTES..payload_offset)
        .ok_or(AudioLoadError::Format)?;
    let (records, rates) = header
        .split_table(table)
        .map_err(|_| AudioLoadError::Format)?;
    validate_sound_records(header, records, rates).map_err(|_| AudioLoadError::BadAddress)?;
    let combined_count = retain_count
        .checked_add(records.len())
        .ok_or(AudioLoadError::TooManySounds)?;
    if combined_count > SOUND_MAX_EFFECTS || retain_count > effects.len() {
        return Err(AudioLoadError::TooManySounds);
    }
    for effect in records.iter() {
        if effects
            .iter()
            .take(retain_count)
            .any(|resident| resident.id == effect.id)
        {
            return Err(AudioLoadError::DuplicateSound(effect.id));
        }
    }
    effects.truncate(retain_count);
    for (index, effect) in records.iter().enumerate() {
        effects.push(LoadedEffect {
            id: effect.id,
            frames: effect.frames,
            spu_address: effect.spu_address,
            rate_hz: rates.get(index).ok_or(AudioLoadError::Format)?,
        });
    }

    let payload_len = header.payload_bytes as usize;
    let first_available = scratch
        .len()
        .checked_sub(payload_offset)
        .ok_or(AudioLoadError::Format)?
        .min(payload_len);
    let first_payload = scratch
        .get(payload_offset..payload_offset + first_available)
        .ok_or(AudioLoadError::Format)?;
    let mut hash = sound_hash_extend(SOUND_HASH_OFFSET, table);
    hash = sound_hash_extend(hash, first_payload);
    let mut hashed = first_available;

    // SPU DMA accepts whole ADPCM blocks. Any partial prefix caused by the
    // variable table size is reread at the start of the next bounded session.
    let first_upload_bytes = first_available & !15;
    if first_upload_bytes != 0 {
        psx_spu::upload_adpcm(
            SpuAddr::new(header.payload_base),
            &first_payload[..first_upload_bytes],
        );
    }
    let payload_source = offset
        .checked_add(payload_offset as u32)
        .ok_or(AudioLoadError::Format)?;
    let mut uploaded = first_upload_bytes;
    while uploaded < payload_len {
        let byte_count = (payload_len - uploaded).min(STREAM_SCRATCH_BYTES);
        scratch.clear();
        scratch.resize(byte_count, 0);
        platform::read_chunk_exact(chunk_id, payload_source + uploaded as u32, scratch)
            .map_err(AudioLoadError::Storage)?;
        phase.source_bytes = phase.source_bytes.saturating_add(byte_count as u32);
        phase.read_sessions = phase.read_sessions.saturating_add(1);
        let already_hashed = hashed.saturating_sub(uploaded).min(byte_count);
        hash = sound_hash_extend(hash, &scratch[already_hashed..]);
        hashed = hashed.max(uploaded + byte_count);
        psx_spu::upload_adpcm(SpuAddr::new(header.payload_base + uploaded as u32), scratch);
        uploaded += byte_count;
    }
    if uploaded != payload_len || hashed != payload_len || hash != header.content_hash {
        return Err(AudioLoadError::Format);
    }
    phase.uploaded_bytes = header.payload_bytes;
    Ok((header, phase))
}

#[cfg(any(feature = "emulator-telemetry", feature = "audio-residency-regression"))]
fn trace_phase(label: &str, phase: AudioPhaseTelemetry, resident_hit: bool) {
    let line = alloc::format!(
        "quake-psx: audio-{label} source-bytes=0x{:08X} upload-bytes=0x{:08X} sessions=0x{:08X} hit={}",
        phase.source_bytes,
        phase.uploaded_bytes,
        u32::from(phase.read_sessions),
        u8::from(resident_hit),
    );
    psx_telemetry::emit::debug_log(&line);
}

#[cfg(not(any(feature = "emulator-telemetry", feature = "audio-residency-regression")))]
fn trace_phase(_label: &str, _phase: AudioPhaseTelemetry, _resident_hit: bool) {}

/// The sound and `master_vol` each `ambient_*` class asks `ambientsound` for.
/// `misc.qc` writes those levels into the call itself (1 or 0.5), so there is
/// no authored `volume` key on the entity for cooking to carry.
fn ambient_sound(class_name: u8) -> Option<(i16, i32)> {
    Some(match class_name {
        0x03 => (0x02, QUAKE_MAX_VOLUME),
        0x04 => (0x03, QUAKE_MAX_VOLUME / 2),
        0x05 => (0x04, QUAKE_MAX_VOLUME / 2),
        0x06 => (0x08, QUAKE_MAX_VOLUME),
        0x07 | 0x08 => (0x09, QUAKE_MAX_VOLUME / 2),
        _ => return None,
    })
}

/// `S_Spatialize`: `(1 - dist) * (1 -+ dot(right, source))` per ear, from a
/// `volume` out of 256, or `None` once the source is beyond its clip.
#[inline(never)]
fn spatial_volumes(
    origin: Vec3I32,
    volume: i32,
    attenuation_q20: i32,
    listener: Vec3I32,
    yaw: i16,
) -> Option<(Volume, Volume)> {
    let dx = (origin.x.saturating_sub(listener.x) >> 12) as i64;
    let dy = (origin.y.saturating_sub(listener.y) >> 12) as i64;
    let dz = (origin.z.saturating_sub(listener.z) >> 12) as i64;
    let distance_squared = dx
        .saturating_mul(dx)
        .saturating_add(dy.saturating_mul(dy))
        .saturating_add(dz.saturating_mul(dz));
    let clip_distance = i64::from((Q12_ONE << 8) / attenuation_q20);
    if distance_squared >= clip_distance * clip_distance {
        return None;
    }
    let distance = isqrt_i32(distance_squared as i32);
    let distance_scale = Q12_ONE
        .saturating_sub((distance.saturating_mul(attenuation_q20)) >> 8)
        .max(0);
    if distance_scale == 0 {
        return None;
    }

    let angle = yaw as u16 & 0x0fff;
    let right_x = -psx_math::sin_q12(angle);
    let right_y = psx_math::cos_q12(angle);
    let pan = if distance == 0 {
        0
    } else {
        ((dx * i64::from(right_x) + dy * i64::from(right_y)) / i64::from(distance))
            .clamp(i64::from(-Q12_ONE), i64::from(Q12_ONE)) as i32
    };
    let left_scale = ((i64::from(distance_scale) * i64::from(Q12_ONE - pan)) >> 12) as i32;
    let right_scale = ((i64::from(distance_scale) * i64::from(Q12_ONE + pan)) >> 12) as i32;
    let scale_volume = |scale: i32| {
        let value = ((volume * scale) >> 12).clamp(0, QUAKE_MAX_VOLUME) as u16;
        Volume::linear(value, 256)
    };
    Some((scale_volume(left_scale), scale_volume(right_scale)))
}
