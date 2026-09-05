//! Compact Rust-owned render state for cooked Quake map entities.

use alloc::vec::Vec;
use core::mem::MaybeUninit;

use psx_math::int32::{isqrt_i32, mul_q12_i32, square_i32_saturating};
use psx_math::{atan2_q12, cos_q12, sin_q12};
use quake_core::body::{Body, BodyBlockers, BroadPhaseRegion, SweptUnitBox, PLAYER_BODY_SOURCE};
use quake_core::collision::{
    trace_render_bsp_into, trace_translated_render_bsp_into, CollisionHull, RenderTraceScratch,
    Trace, TraceScratch, CONTENTS_SKY, Q12_ONE,
};
use quake_core::combat::{
    explosion_splash_points, grenade_tick, lightning_trace_geometry, monster_profile,
    pickup_for_entity, projectile_expires_this_tick, rocket_direct_points, rocket_elapsed_ticks,
    segment_aabb_fraction, segment_overlaps_i16_bounds, settle_grenade_motion, AmmoKind,
    AttackAdmission, ExplosionKind, GrenadeSpawn, GrenadeTick, HitscanAttack, LightningAttack,
    LightningDischarge, MonsterProfile, NailSpawn, Pickup, RocketSpawn, ShotgunAttack, WeaponState,
    GRENADE_MODEL_ID, MAX_SHOTGUN_PELLETS, NAIL_MODEL_ID, NAIL_POOL_CAPACITY, ROCKET_MODEL_ID,
};
use quake_core::door;
use quake_core::effects::{ImpactParticles, ParticleKind};
use quake_core::level::MonsterCounter;
use quake_core::lightstyle::{self, sample_leaf as leaf_light};
#[cfg(any(
    feature = "monster-regression",
    feature = "bestiary-regression",
    feature = "monsterjump-regression"
))]
use quake_core::monster::MonsterState;
use quake_core::monster::{
    predicted_target, BackpackAmmo, MonsterAttack, MonsterEnemy, MonsterKind, MonsterLeap,
    MonsterRuntime, MonsterThinkInput, CLASS_ARMY, CLASS_BOSS, CLASS_ZOMBIE, LAVA_BALL_MODEL_ID,
    ZOMBIE_GIB_MODEL_ID, ZOMBIE_HIT_SOUND, ZOMBIE_MISS_SOUND,
};
use quake_core::movement::{MovementTrace, MovementTraceResult};
use quake_core::mover::{
    move_direction, mover_sound_events, mover_state_admits_activation, translated_model_bounds,
    QuakeMover, QuakeMoverError, QuakeMoverState,
};
use quake_core::push::{rests_on, BlockCrush, RiderBody};
use quake_core::secrets::SecretCounter;
use quake_core::targets::{
    excluded_for_skill, parse_setskill, TargetAction, TargetActions, TargetActivator, TargetError,
    TargetGraph, CHANGELEVEL_NO_INTERMISSION, MAX_TARGET_ACTIONS,
};
use quake_core::teleport::{self, TeleportGate};
use quake_core::train::{
    find_path_corner_into, PathCorner, QuakeTrain, TrainState, TRAIN_BLOCK_COOLDOWN_TICKS,
};
use quake_core::traps::{self, TrapRandom};
use quake_core::trigger::{self, MultiTrigger};
use quake_formats::{alias_model_is_sprite, AliasModelHeader, MapEntity, Vec3I16, Vec3I32};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::audio::Attenuation;
use crate::pusher::{push_pass, PushBlocker, Rider};

// Host validation proves the Episode 1 high-water is 373 after every fixed
// projectile slot is installed. Keep eleven spares without paying for 128
// unreachable heap objects in the shipping image.
const MAX_RENDER_ENTITIES: usize = 384;
const MAX_CHANGE_LEVELS: usize = 4;
// The verified shareware corpus peaks at 57 supported brush movers and 26
// touch triggers in one map. These closed-corpus bounds leave seven and six
// slots respectively; the host cooker mirrors them and rejects any future map
// that would exceed either pool before a disc is built.
const MAX_MOVERS: usize = 64;
const MAX_TRIGGERS: usize = 32;
const MAX_TELEPORTS: usize = 32;
const MAX_TRAINS: usize = 8;
const MAX_ROCKETS: usize = 8;
const MAX_GRENADES: usize = 8;
const MAX_FIREBALL_EMITTERS: usize = 16;
/// Every spout re-arms after at least three seconds and each ball lives five,
/// so two of a spout's balls are the most that are ever in flight.
const FIREBALLS_PER_EMITTER: usize = 2;
const MAX_FIREBALLS: usize = MAX_FIREBALL_EMITTERS * FIREBALLS_PER_EMITTER;
const MAX_MONSTER_SOUNDS: usize = 16;
/// The largest authored Episode 1 teleport release is three monsters. Keep one
/// spare so a corrupt or extended map fails visually without growing a pool.
const MAX_MONSTER_TELEPORT_FOGS: usize = 4;
/// Every monster-launched projectile in flight on one map, across all four
/// authored kinds. Denial on full: an over-capacity launch is simply refused
/// and the animation frame plays without a missile.
const MAX_MONSTER_MISSILES: usize = 12;
/// First trail anchor belonging to the monster missile pool.
const MISSILE_TRAIL_BASE: usize = MAX_ROCKETS + MAX_GRENADES;
const SPAWNFLAG_ZOMBIE_CRUCIFIED: u16 = 1;
/// `SPAWNFLAG_AMBUSH` plus the bit the original's `FindTarget` treats the
/// same way (`!(self.spawnflags & 3)`).
const SPAWNFLAG_MONSTER_AMBUSH: u16 = 3;
const PROJECTILE_CLIP_RADIUS_UNITS: i16 = 32;
const ANIMATION_FRAME_DIVISOR: u8 = 3;
const CLASS_TRIGGER_CHANGELEVEL: u8 = 0x47;
const CLASS_TRIGGER_MONSTERJUMP: u8 = 0x4a;
const CLASS_TRIGGER_ONLY_REGISTERED: u8 = 0x4d;
const CLASS_TRIGGER_SETSKILL: u8 = 0x51;
/// `trigger_onlyregistered_touch` re-arms two seconds after each report.
const REGISTERED_RETRY_TICKS: u16 = 120;
const CLASS_TRIGGER_TELEPORT: u8 = 0x52;
const CLASS_MISC_FIREBALL: u8 = 0x34;
/// player/axhit2.wav
const AXE_WALL_SOUND: i16 = 0x8d;
/// weapons/tink1.wav
pub const SPIKE_TINK_SOUND: i16 = 0xd0;
/// weapons/ric1.wav .. ric3.wav
pub const SPIKE_RICOCHET_SOUNDS: [i16; 3] = [0xc9, 0xca, 0xcb];
const CLASS_MISC_EXPLOBOX: u8 = 0x32;
const CLASS_MISC_EXPLOBOX2: u8 = 0x33;
/// The one authored E1M6 monster-jump volume uses Quake's 200/200 defaults
/// and yaw zero. The high patrol bit carries the allocation-free marker while
/// leaving any path-corner source in the lower bits intact.
const FORCED_JUMP_SOURCE: u16 = 0x8000;
const E1M6_MONSTERJUMP_SOURCE: usize = 192;
/// `barrel_explode`: `T_RadiusDamage (self, self, 160, world)`.
const EXPLOBOX_SPLASH_DAMAGE: i16 = 160;
/// `T_MissileTouch` on a Chthon lava ball: `T_RadiusDamage (self, self.owner, 120, other)`.
const LAVA_BALL_SPLASH_DAMAGE: i16 = 120;
const CLASS_FUNC_BOSSGATE: u8 = 0x0a;
const CLASS_FUNC_EPISODEGATE: u8 = 0x0e;
const CLASS_FUNC_PLAT: u8 = 0x10;
const CLASS_ITEM_SIGIL: u8 = 0x27;
/// `progs/end1.mdl`, worn only by `item_sigil`.
const SIGIL_MODEL_ID: i16 = 0x18;
const CLASS_INFO_INTERMISSION: u8 = 0x13;
/// `serverflags` carries one bit per finished episode.
const RUNE_MASK: u8 = 0x0f;
/// `sigil_touch` plays misc/runekey.
const RUNE_SOUND_ID: i16 = 0x79;

const SPAWNFLAG_BIG_OR_SMALL: u16 = 1;
const SPAWNFLAG_SUPER_HEALTH: u16 = 2;

const EF_ROTATE: u8 = 8;

/// `MOVE_STEP`'s floor test, `normal_z > 0.7` in Q12.
const LEAP_FLOOR_NORMAL_Q12: i16 = 2_896;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EntityLoadError {
    TooMany,
    TooManyChangeLevels,
    MissingModel { entity: u16, model: i16 },
    BadLeaf { entity: u16 },
    BadChangeLevel { entity: u16 },
    BadMover { entity: u16, error: QuakeMoverError },
    BadTarget(TargetError),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RenderEntity {
    pub source_index: u16,
    pub origin: Vec3I32,
    pub angles: Vec3I16,
    pub model_id: i16,
    pub model_index: u8,
    pub frame: u16,
    pub skin: u8,
    pub clip_mins: [i16; 3],
    pub clip_maxs: [i16; 3],
    pub leaf_index: u16,
    pub light: u8,
    pub solid: bool,
    pub visible: bool,
    animation_start: u16,
    animation_end: u16,
    hit_mins: Vec3I32,
    hit_maxs: Vec3I32,
    health: i16,
    max_health: i16,
    damageable: bool,
    pickup: Option<Pickup>,
    projectile: bool,
    monster: Option<MonsterRuntime>,
    /// `movetarget`: cooked source index of the `path_corner` a resting
    /// monster walks toward, resolved at load and on every arrival. Zero is
    /// none (worldspawn is never a corner). The corner itself is decoded only
    /// on the frames a think is due.
    patrol: u16,
    /// A gib-level death happened to this entity since the scene last threw
    /// its gibs (`army_die` and friends: `ThrowGib` x3 after the head).
    pending_gib: bool,
    /// A `misc_explobox` died since the scene last ran `barrel_explode`.
    pending_explosion: bool,
}

impl RenderEntity {
    fn advance_animation(&mut self) {
        self.frame = if self.frame < self.animation_end {
            self.frame + 1
        } else {
            self.animation_start
        };
    }

    pub const fn is_projectile(&self) -> bool {
        self.projectile
    }
}

pub struct EntityScene {
    entities: Vec<RenderEntity>,
    /// Set when a death left a `pending_gib` or `pending_explosion` behind,
    /// so the per-frame passes can skip the entity scan on quiet frames.
    pending_scene_work: bool,
    /// `GibPlayer`: the player's origin and health at a gib-level death,
    /// thrown as three chunks on the next missile pass.
    pending_player_gib: Option<(Vec3I32, i16)>,
    /// Ascending render indexes that can ever participate in player movement
    /// collision. Pickups, lights, decoration and reserved projectile slots
    /// make up most of `entities`; filtering them once at load avoids scanning
    /// the whole render pool for every step/slide trace.
    collision_indices: Vec<u16>,
    movers: Vec<SceneMover>,
    trains: Vec<SceneTrain>,
    triggers: Vec<Trigger>,
    teleports: Vec<TeleportTrigger>,
    targets: TargetGraph,
    change_levels: [Option<ChangeLevel>; MAX_CHANGE_LEVELS],
    change_level_count: usize,
    animation_subtick: u8,
    frame: u32,
    rockets: [Option<RocketProjectile>; MAX_ROCKETS],
    rocket_render_start: u16,
    nails: [Option<NailProjectile>; NAIL_POOL_CAPACITY],
    nail_render_start: u16,
    grenades: [Option<GrenadeProjectile>; MAX_GRENADES],
    grenade_render_start: u16,
    missiles: [Option<MonsterMissile>; MAX_MONSTER_MISSILES],
    missile_render_start: u16,
    /// Where each live projectile last dropped a trail particle, rockets then
    /// grenades then monster missiles. `R_RocketTrail` walks from the
    /// previous origin to the current one, so the spacing has to follow
    /// distance flown and not whichever frame rate the port reaches.
    trail_anchors: [Vec3I32; MISSILE_TRAIL_BASE + MAX_MONSTER_MISSILES],
    fireball_emitters: Vec<FireballEmitter>,
    fireballs: [Option<FireballProjectile>; MAX_FIREBALLS],
    /// Slots this map actually needs, two per authored spout.
    fireball_slots: usize,
    fireball_render_start: Option<u16>,
    lightning_beam: Option<LightningBeam>,
    lightning_beam_frames: u8,
    secrets: SecretCounter,
    monsters: MonsterCounter,
    /// `SelectIntermissionPoint`: the camera the end-of-level panel uses.
    intermission: Option<IntermissionSpot>,
    /// `d_lightstylevalue`. Predefined styles animate at 10 Hz; switchable
    /// ones are owned by `light_use` through the target graph.
    light_styles: [u16; lightstyle::DUMMY_STYLE + 1],
    /// The animation tick the table was last written for, so a frame that
    /// lands inside the same tenth of a second does no work.
    light_style_tick: u32,
    /// One bit per cooked source index: this `light` is currently off, which
    /// is the original's own `START_OFF` spawnflag after `light_use` has
    /// flipped it. Two lights may share a style, so the state cannot live in
    /// the style value.
    light_off: [u32; LIGHT_STATE_WORDS],
    skill: u8,
    runes: u8,
    /// `sight_entity` / `sight_entity_time`: the monster that most recently
    /// found the player, and the ticks left in which its neighbours may wake
    /// with it.
    sight_index: u16,
    sight_alert_ticks: u16,
    /// `activator.items & IT_INVISIBILITY`, sampled each monster tick so
    /// `monster_use` can refuse a ringed player without threading the weapon
    /// state through every trigger path.
    player_invisible: bool,
}

/// `sight_entity_time >= time - 0.1`: how long a sighting alerts the pack.
const SIGHT_ALERT_TICKS: u16 = 6;

/// The current target of a monster's think: the player, or the monster it is
/// infighting with, in the same shape so the attack code does not care.
#[derive(Copy, Clone, Debug)]
struct EnemyTarget {
    origin: Vec3I32,
    velocity: Vec3I32,
    mins: Vec3I32,
    maxs: Vec3I32,
    alive: bool,
    /// Eye height above the origin in whole units, for the sight trace.
    view_height: i32,
    /// Scene entity index when the enemy is a monster; `None` for the player.
    index: Option<usize>,
}

/// Bits for [`EntityScene::light_off`], one per cooked entity slot.
const LIGHT_STATE_WORDS: usize = quake_core::targets::MAX_TARGET_ENTITIES / 32;
/// 60 Hz vblanks per `R_AnimateLight` step. The original samples the pattern
/// at `(int)(cl.time * 10)`.
const LIGHT_STYLE_VBLANKS: u32 = 60 / quake_core::lightstyle::ANIMATION_HZ;

/// Chthon's live runtime, read by the episode gate.
#[cfg(feature = "episode1-route-regression")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BossSnapshot {
    pub frame: u16,
    pub shocks: i16,
    pub active: bool,
    pub dead: bool,
    pub visible: bool,
    /// True while he is inside his authored `boss_missile` animation, which
    /// is the frame range that launches a lava ball.
    pub throwing: bool,
}

/// One authored `info_intermission` camera.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IntermissionSpot {
    pub origin: Vec3I32,
    pub angles: [i16; 3],
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ChangeLevel {
    source_index: u16,
    mins: Vec3I32,
    maxs: Vec3I32,
    destination: EpisodeMap,
    /// `NO_INTERMISSION`: skip the panel and load the next map at once.
    no_intermission: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TeleportDestination {
    pub source_index: u16,
    pub destination_index: u16,
    pub origin: Vec3I32,
    pub angles: Vec3I16,
    /// `teleport_touch`'s `v_forward * 300` exit push.
    pub exit_velocity: Vec3I32,
    /// Monsters killed by `spawn_tdeath` at the destination.
    pub telefragged: u8,
    /// Set by the authored SILENT spawnflag.
    pub silent: bool,
}

pub const MAX_PLAYER_ACTIVATIONS: usize = 8;
const MAX_MOVER_SOUNDS: usize = 8;

/// `SV_PushMove`'s downward ground probe, in Q20.12 units. The locomotion
/// motor snaps a grounded box onto the surface it rests on, so the entity that
/// answers this probe is Quake's `groundentity`.
const GROUND_PROBE_Q12: i32 = 2 << 12;
/// A plane this steep or steeper is floor, matching the motor's own cutoff.
const WALKABLE_NORMAL_Q12: i32 = 2_867;
/// `plat_crush` deals exactly one point of damage per crush.
const PLAT_CRUSH_DAMAGE: u16 = 1;

/// One `S_StartSound` request. A listener-owned sound (`entnum ==
/// cl.viewentity`) plays at full volume, centred; a world sound is
/// spatialised from `origin`. Kept narrow, since the frame results carry
/// arrays of these by value: the origin is whole world units (a ramp over
/// 1000 units never needs the Q12 fraction), the attenuation rides in the
/// two spare high bits of the sound id, and `(entnum, entchannel)` share one
/// halfword.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundEvent {
    origin: [i16; 3],
    /// `id | SOURCE_*`.
    tagged_id: u16,
    /// `SND_PickChannel`'s key; see [`crate::audio::sound_key`].
    key: u16,
}

const SOURCE_MASK: u16 = 0xc000;
const SOURCE_LISTENER: u16 = 0x0000;
const SOURCE_NORM: u16 = 0x4000;
const SOURCE_IDLE: u16 = 0x8000;

const fn pack_origin(origin: Vec3I32) -> [i16; 3] {
    [
        (origin.x >> 12) as i16,
        (origin.y >> 12) as i16,
        (origin.z >> 12) as i16,
    ]
}

impl SoundEvent {
    pub const fn listener(id: i16) -> Self {
        Self {
            origin: [0; 3],
            tagged_id: (id as u16 & !SOURCE_MASK) | SOURCE_LISTENER,
            key: crate::audio::sound_key(crate::audio::OWNER_PLAYER, crate::audio::CHAN_AUTO),
        }
    }

    pub const fn at(id: i16, origin: Vec3I32) -> Self {
        Self {
            origin: pack_origin(origin),
            tagged_id: (id as u16 & !SOURCE_MASK) | SOURCE_NORM,
            key: crate::audio::sound_key(crate::audio::OWNER_WORLD, crate::audio::CHAN_AUTO),
        }
    }

    /// `ATTN_IDLE`: monster idle voices carry half the reach.
    pub const fn idle_at(id: i16, origin: Vec3I32) -> Self {
        Self {
            origin: pack_origin(origin),
            tagged_id: (id as u16 & !SOURCE_MASK) | SOURCE_IDLE,
            key: crate::audio::sound_key(crate::audio::OWNER_WORLD, crate::audio::CHAN_AUTO),
        }
    }

    /// `PainSound` and `DeathSound`: centred on the listener and keyed to the
    /// player's own `CHAN_VOICE`, so a second cry cuts the first rather than
    /// stacking two voices of the same throat.
    pub const fn player_voice(id: i16) -> Self {
        Self::listener(id).on(crate::audio::OWNER_PLAYER, crate::audio::CHAN_VOICE)
    }

    /// Re-key this sound onto one entity's channel, so the same pair's next
    /// sound cuts it rather than taking a second voice. `entnum` is the
    /// cooked `source_index`; the player uses `audio::OWNER_PLAYER`.
    pub const fn on(mut self, owner: u16, channel: u16) -> Self {
        self.key = crate::audio::sound_key(owner, channel);
        self
    }

    pub const fn key(&self) -> u16 {
        self.key
    }

    /// `ATTN_NORM` at `origin` when the emitter's position is known,
    /// otherwise centred.
    pub const fn world(id: i16, origin: Option<Vec3I32>) -> Self {
        match origin {
            Some(origin) => Self::at(id, origin),
            None => Self::listener(id),
        }
    }

    pub const fn id(&self) -> i16 {
        (self.tagged_id & !SOURCE_MASK) as i16
    }

    pub const fn placement(&self) -> Option<(Vec3I32, Attenuation)> {
        let attenuation = match self.tagged_id & SOURCE_MASK {
            SOURCE_NORM => Attenuation::Norm,
            SOURCE_IDLE => Attenuation::Idle,
            _ => return None,
        };
        Some((
            Vec3I32 {
                x: (self.origin[0] as i32) << 12,
                y: (self.origin[1] as i32) << 12,
                z: (self.origin[2] as i32) << 12,
            },
            attenuation,
        ))
    }
}

impl Default for SoundEvent {
    fn default() -> Self {
        Self::listener(0)
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct GameplayResult {
    pub teleport: Option<TeleportDestination>,
    pub selected_skill: Option<u8>,
    pub fired_target_edges: u16,
    pub completed_counters: u16,
    pub message_source: Option<u16>,
    /// `counter_use`'s countdown line, from the last counter stepped.
    pub counter_message: Option<&'static str>,
    /// Trigger noise selected by the authored `sounds` field.
    pub message_sound: Option<SoundEvent>,
    /// A `trigger_secret` fired this frame.
    pub found_secret: bool,
    /// Damage a moving brush pushed into the player this frame.
    pub crush_damage: u16,
    /// Key bit a locked door just spent out of the inventory.
    pub consumed_key: Option<u8>,
    /// Key bit a locked door just refused the player for.
    pub needs_key: Option<u8>,
    /// `path_corner` arrivals across every train this frame.
    pub train_arrivals: u16,
    /// One authored train leg-start or arrival noise.
    pub train_sound: Option<SoundEvent>,
    /// Movers the player set in motion this frame, in activation order. One
    /// gameplay frame spans several fixed ticks, so a button press, its top
    /// arrival, and the mover chain it fires can all land here together.
    pub player_activated_movers: [Option<u16>; MAX_PLAYER_ACTIVATIONS],
    mover_sounds: [SoundEvent; MAX_MOVER_SOUNDS],
    mover_sound_count: u8,
    pub target_error: Option<TargetError>,
    /// Chthon's encounter trigger fired and raised him this frame.
    pub boss_awakened: bool,
    /// `event_lightning` shocks delivered this frame.
    pub boss_shocks: u8,
    /// `lightning_fire`'s zap and the voice Chthon answers it with, both
    /// placed on him because the trap and its target share the one room.
    pub boss_shock_sound: Option<SoundEvent>,
    /// `boss_death9`'s `TE_LAVASPLASH` origin, on the shock that kills him.
    pub boss_death_origin: Option<Vec3I32>,
}

impl GameplayResult {
    fn record_player_activation(&mut self, source_index: u16) {
        if let Some(slot) = self
            .player_activated_movers
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(source_index);
        }
    }

    #[inline(never)]
    fn push_mover_sound(&mut self, sound: SoundEvent) {
        if self.mover_sounds[..self.mover_sound_count as usize]
            .iter()
            .any(|queued| queued.id() == sound.id())
        {
            return;
        }
        let index = self.mover_sound_count as usize;
        if index < self.mover_sounds.len() {
            self.mover_sounds[index] = sound;
            self.mover_sound_count += 1;
        }
    }

    pub fn mover_sounds(&self) -> &[SoundEvent] {
        &self.mover_sounds[..self.mover_sound_count as usize]
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ShotgunResult {
    pub pellet_hits: u8,
    pub damaged_targets: u8,
    pub total_damage: u16,
    pub killed_targets: u8,
    pub last_source_index: Option<u16>,
    pub last_impact: Option<Vec3I32>,
    pub response_sound: Option<SoundEvent>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PickupResult {
    pub consumed: u8,
    pub switched_weapon: bool,
    pub last_source_index: Option<u16>,
    pub sound_id: Option<i16>,
    /// The accepted pickup's own id1 line.
    pub message: Option<&'static str>,
    /// `sigil_touch` ends in `SUB_UseTargets`, so a rune pickup can start a
    /// whole chain. In E1M7 that chain is Chthon himself.
    pub boss_awakened: bool,
    pub fired_target_edges: u16,
    pub target_error: Option<TargetError>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DamageResult {
    pub damaged_targets: u8,
    pub total_damage: u16,
    pub killed_targets: u8,
    pub last_source_index: Option<u16>,
    pub last_impact: Option<Vec3I32>,
    pub response_sound: Option<SoundEvent>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RocketResult {
    pub impacts: u8,
    /// Where the last explosion this frame went off. The presentation layer
    /// turns it into the screen-space stand-in for the original's `dlight`.
    pub last_impact: Option<Vec3I32>,
    pub direct_hits: u8,
    pub splash_hits: u8,
    pub total_damage: u16,
    pub killed_targets: u8,
    pub self_damage: u16,
    pub last_source_index: Option<u16>,
    pub response_sound: Option<SoundEvent>,
    pub sky_removals: u8,
    /// `T_Damage`'s knockback on the player, summed over the frame in Q12
    /// units per second; the caller adds it to the player's velocity.
    pub player_impulse: Vec3I32,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct NailResult {
    pub impacts: u8,
    pub damage: DamageResult,
    /// Trap-spike damage dealt to the player this frame.
    pub player_damage: u16,
    /// Knockback from those spikes, Q12 units per second.
    pub player_impulse: Vec3I32,
    /// Spikes that stopped on the world (`TE_SPIKE`), for the tink sound.
    pub world_impacts: u8,
    /// Where the last of those stopped, so the tink plays there.
    pub last_world_impact: Option<Vec3I32>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct GrenadeResult {
    pub bounces: u8,
    /// Where the last bounce landed, for the positional bounce noise.
    pub last_bounce: Option<Vec3I32>,
    pub rests: u8,
    pub explosions: u8,
    pub damage: RocketResult,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LightningResult {
    pub damage: DamageResult,
    pub discharge: bool,
    pub world_clipped: bool,
    pub trace_end: Vec3I32,
    pub side_end: Vec3I32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LightningBeam {
    pub start: Vec3I32,
    pub end: Vec3I32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonsterFrameResult {
    sound_ids: [SoundEvent; MAX_MONSTER_SOUNDS],
    sound_count: u8,
    teleport_fogs: [Vec3I32; MAX_MONSTER_TELEPORT_FOGS],
    teleport_fog_count: u8,
    pub activated: u8,
    pub moved: u8,
    pub attacks: u8,
    pub player_damage: u16,
    pub player_killed: bool,
    /// Knockback the player took this frame, Q12 units per second.
    pub player_impulse: Vec3I32,
    /// Where the last exploding monster missile (an ogre grenade) went off
    /// this frame: `OgreGrenadeExplode`'s `TE_EXPLOSION` for the presentation.
    pub last_explosion: Option<Vec3I32>,
    /// Where the last monster was gibbed this frame, for the blood burst.
    pub last_gib: Option<Vec3I32>,
    /// What `BackpackTouch` handed the player this frame, for the bonus flash
    /// and the line naming it.
    pub backpack_pickup: Option<BackpackAmmo>,
}

#[cfg(any(
    feature = "monster-regression",
    feature = "bestiary-regression",
    feature = "monsterjump-regression"
))]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonsterRegressionSnapshot {
    pub origin: Vec3I32,
    pub frame: u16,
    pub state: MonsterState,
    pub model_id: i16,
    pub health: i16,
    pub active: bool,
    pub corpse_finished: bool,
    pub leaping: bool,
    pub forced_jump: bool,
}

impl Default for MonsterFrameResult {
    fn default() -> Self {
        Self {
            sound_ids: [SoundEvent::listener(0); MAX_MONSTER_SOUNDS],
            sound_count: 0,
            teleport_fogs: [Vec3I32::default(); MAX_MONSTER_TELEPORT_FOGS],
            teleport_fog_count: 0,
            activated: 0,
            moved: 0,
            attacks: 0,
            player_damage: 0,
            player_killed: false,
            player_impulse: Vec3I32::default(),
            last_explosion: None,
            last_gib: None,
            backpack_pickup: None,
        }
    }
}

impl MonsterFrameResult {
    pub fn sound_ids(&self) -> &[SoundEvent] {
        &self.sound_ids[..self.sound_count as usize]
    }

    pub fn teleport_fogs(&self) -> &[Vec3I32] {
        &self.teleport_fogs[..self.teleport_fog_count as usize]
    }

    #[inline(never)]
    fn push_sound(&mut self, sound: SoundEvent) {
        if (self.sound_count as usize) < self.sound_ids.len() {
            self.sound_ids[self.sound_count as usize] = sound;
            self.sound_count += 1;
        }
    }

    #[inline(never)]
    fn push_teleport_fog(&mut self, origin: Vec3I32) {
        if (self.teleport_fog_count as usize) < self.teleport_fogs.len() {
            self.teleport_fogs[self.teleport_fog_count as usize] = origin;
            self.teleport_fog_count += 1;
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct RocketProjectile {
    origin: Vec3I32,
    step: Vec3I32,
    remaining_ticks: u16,
    direct_damage: i16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct NailProjectile {
    origin: Vec3I32,
    step: Vec3I32,
    remaining_ticks: u16,
    damage: i16,
    /// Launched by a `trap_spikeshooter`, so it damages the player.
    hostile: bool,
}

/// One `misc_fireball` lava spout.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FireballEmitter {
    source_index: u16,
    speed: i16,
    random: TrapRandom,
    remaining_ticks: u16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FireballProjectile {
    origin: Vec3I32,
    velocity: Vec3I32,
    remaining_ticks: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FireballResult {
    pub launched: u8,
    pub impacts: u8,
    pub player_damage: u16,
    /// Knockback from the balls that hit the player, Q12 units per second.
    pub player_impulse: Vec3I32,
}

/// The four authored monster projectile kinds. All share one pool and one
/// render slot range; the slot's alias model is swapped at launch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MonsterMissileKind {
    /// Ogre grenade: bounces, times out, explodes.
    Grenade,
    /// Zombie flesh gib: sticks where it lands, times out.
    Gib,
    /// Wizard acid spit: straight, dies on contact.
    Spit,
    /// Chthon lava ball: straight, dies on contact.
    LavaBall,
    /// A thrown gib chunk (`ThrowGib`): bounces, hurts nothing, times out.
    Debris,
    /// `DropBackpack`: tossed, settles where it lands, picked up by the
    /// player, gone after two minutes.
    Backpack(BackpackAmmo),
}

impl MonsterMissileKind {
    const fn ballistic(self) -> bool {
        matches!(
            self,
            Self::Grenade | Self::Gib | Self::Debris | Self::Backpack(_)
        )
    }

    /// Kinds that hurt whoever they touch; debris flies through everyone and
    /// the backpack only ever meets the player, tested separately.
    const fn touches_player(self) -> bool {
        !matches!(self, Self::Debris | Self::Backpack(_))
    }
}

/// What a monster missile touched first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MissileHit {
    Player,
    /// Scene entity index of a live monster.
    Monster(usize),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MonsterMissile {
    origin: Vec3I32,
    velocity: Vec3I32,
    angles: Vec3I16,
    angular_velocity: Vec3I16,
    kind: MonsterMissileKind,
    damage: i16,
    remaining_ticks: u16,
    resting: bool,
    /// `owner`: scene index of the monster that launched it. It never touches
    /// its owner, and it is the `T_Damage` attacker for whatever it hits.
    owner: u16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct GrenadeProjectile {
    origin: Vec3I32,
    velocity: Vec3I32,
    angles: Vec3I16,
    angular_velocity: Vec3I16,
    resting: bool,
    remaining_ticks: u16,
    damage: i16,
}

impl EntityScene {
    pub fn new() -> Self {
        Self {
            entities: Vec::with_capacity(MAX_RENDER_ENTITIES),
            pending_scene_work: false,
            pending_player_gib: None,
            collision_indices: Vec::with_capacity(MAX_RENDER_ENTITIES),
            movers: Vec::with_capacity(MAX_MOVERS),
            trains: Vec::with_capacity(MAX_TRAINS),
            triggers: Vec::with_capacity(MAX_TRIGGERS),
            teleports: Vec::with_capacity(MAX_TELEPORTS),
            targets: TargetGraph::new(),
            change_levels: [None; MAX_CHANGE_LEVELS],
            change_level_count: 0,
            animation_subtick: 0,
            frame: 0,
            rockets: [None; MAX_ROCKETS],
            rocket_render_start: 0,
            nails: [None; NAIL_POOL_CAPACITY],
            nail_render_start: 0,
            grenades: [None; MAX_GRENADES],
            grenade_render_start: 0,
            missiles: [None; MAX_MONSTER_MISSILES],
            missile_render_start: 0,
            trail_anchors: [Vec3I32 { x: 0, y: 0, z: 0 };
                MISSILE_TRAIL_BASE + MAX_MONSTER_MISSILES],
            fireball_emitters: Vec::with_capacity(MAX_FIREBALL_EMITTERS),
            fireballs: [None; MAX_FIREBALLS],
            fireball_slots: 0,
            fireball_render_start: None,
            lightning_beam: None,
            lightning_beam_frames: 0,
            secrets: SecretCounter::new(),
            monsters: MonsterCounter::new(),
            intermission: None,
            light_styles: lightstyle::initial_values(),
            light_style_tick: u32::MAX,
            light_off: [0; LIGHT_STATE_WORDS],
            skill: 0,
            runes: 0,
            sight_index: 0,
            sight_alert_ticks: 0,
            player_invisible: false,
        }
    }

    /// `d_lightstylevalue`, for the renderer's face and entity lighting.
    pub const fn light_styles(&self) -> &[u16; lightstyle::DUMMY_STYLE + 1] {
        &self.light_styles
    }

    /// `R_AnimateLight`. `tick` is the original's `(int)(time * 10)`.
    ///
    /// Returns true when the table actually changed, so the caller can skip
    /// the per-entity relight on the five frames out of six that land inside
    /// the same tenth of a second.
    pub fn animate_light_styles(&mut self, tick: u32) -> bool {
        if self.light_style_tick == tick {
            return false;
        }
        self.light_style_tick = tick;
        lightstyle::animate(&mut self.light_styles, tick);
        true
    }

    /// One frame of Quake's animated lighting: advance the style table on the
    /// ten-hertz boundary and re-sample every live entity's leaf so a model
    /// standing under a flickering torch flickers with it.
    ///
    /// The relight is skipped on the five frames in six that land inside the
    /// same tenth of a second, and it costs one table lookup per entity, so
    /// the whole feature is bounded by the render-entity pool.
    pub fn animate_lights(&mut self, map: &ResidentMap, vblank: u32) {
        if !self.animate_light_styles(vblank / LIGHT_STYLE_VBLANKS) {
            return;
        }
        let styles = self.light_styles;
        let leaves = map.leaves();
        for entity in &mut self.entities {
            let Some(leaf) = leaves.get(entity.leaf_index as usize) else {
                continue;
            };
            entity.light = leaf_light(leaf.lightmap, leaf.light_styles, &styles);
        }
    }

    const fn light_is_off(&self, source_index: u16) -> bool {
        let index = source_index as usize;
        if index >= quake_core::targets::MAX_TARGET_ENTITIES {
            return false;
        }
        self.light_off[index / 32] & (1 << (index % 32)) != 0
    }

    fn set_light_off(&mut self, source_index: u16, off: bool) {
        let index = source_index as usize;
        if index >= quake_core::targets::MAX_TARGET_ENTITIES {
            return;
        }
        let bit = 1u32 << (index % 32);
        if off {
            self.light_off[index / 32] |= bit;
        } else {
            self.light_off[index / 32] &= !bit;
        }
    }

    /// `light_use`: flip one authored light and write its style, exactly like
    /// the original's `lightstyle(self.style, "m" | "a")`.
    fn toggle_light(&mut self, map: &ResidentMap, source_index: u16) {
        let Some(source) = map.entities().get(source_index as usize) else {
            return;
        };
        let style = source.count;
        if style < 0 || style as usize >= lightstyle::STYLE_COUNT {
            return;
        }
        let on = self.light_is_off(source_index);
        self.set_light_off(source_index, !on);
        self.light_styles[style as usize] = lightstyle::switched_value(on);
    }

    #[optimize(size)]
    /// `skill` is the cvar the Options page holds; the entity loader reads it
    /// on every map from here, and Start's own skill doors overwrite it.
    pub fn reset_game(&mut self, skill: u8) {
        self.skill = skill;
        self.runes = 0;
    }

    /// `serverflags`: the episode runes carried across map loads.
    /// `skill == 3`. Nightmare monsters neither flinch nor reload; the flag
    /// rides the think input and the damage call rather than the runtime,
    /// which is exactly full at twenty-four bytes times five hundred slots.
    const fn nightmare(&self) -> bool {
        self.skill >= 3
    }

    pub const fn runes(&self) -> u8 {
        self.runes
    }

    #[optimize(size)]
    pub fn load(&mut self, map: &ResidentMap) -> Result<(), EntityLoadError> {
        self.entities.clear();
        self.collision_indices.clear();
        self.movers.clear();
        self.trains.clear();
        self.triggers.clear();
        self.teleports.clear();
        self.change_levels = [None; MAX_CHANGE_LEVELS];
        self.change_level_count = 0;
        self.animation_subtick = 0;
        self.frame = 0;
        self.rockets = [None; MAX_ROCKETS];
        self.rocket_render_start = 0;
        self.nails = [None; NAIL_POOL_CAPACITY];
        self.nail_render_start = 0;
        self.grenades = [None; MAX_GRENADES];
        self.grenade_render_start = 0;
        self.missiles = [None; MAX_MONSTER_MISSILES];
        self.missile_render_start = 0;
        self.fireball_emitters.clear();
        self.fireballs = [None; MAX_FIREBALLS];
        self.fireball_slots = 0;
        self.fireball_render_start = None;
        self.lightning_beam = None;
        self.lightning_beam_frames = 0;
        self.sight_index = 0;
        self.sight_alert_ticks = 0;
        self.player_invisible = false;
        // Every light's spawn function writes its own style before any
        // trigger runs: `"m"` normally, `"a"` when `START_OFF` is authored.
        // The predefined 1..31 channels are rewritten by the animator, so
        // only the switchable half is seeded here.
        self.light_styles = lightstyle::initial_values();
        self.light_style_tick = u32::MAX;
        self.light_off = [0; LIGHT_STATE_WORDS];
        for (source_index, source) in map.entities().iter().enumerate() {
            if !quake_core::targets::is_light_class(source.class_name) {
                continue;
            }
            let style = source.count;
            if style < lightstyle::FIRST_SWITCHABLE_STYLE as i16
                || style as usize >= lightstyle::STYLE_COUNT
            {
                continue;
            }
            let start_off = source.spawn_flags & lightstyle::SPAWNFLAG_LIGHT_START_OFF != 0;
            self.set_light_off(source_index as u16, start_off);
            self.light_styles[style as usize] = lightstyle::switched_value(!start_off);
        }
        self.targets
            .load(&map.entities())
            .map_err(EntityLoadError::BadTarget)?;
        self.secrets.load(&map.entities(), self.skill);
        self.monsters.load(&map.entities(), self.skill);
        self.intermission = map
            .entities()
            .iter()
            .find(|entity| entity.class_name == CLASS_INFO_INTERMISSION)
            .map(|entity| IntermissionSpot {
                origin: entity.origin,
                angles: [entity.angles.x, entity.angles.y, entity.angles.z],
            });
        for (source_index, source) in map.entities().iter().enumerate() {
            if excluded_for_skill(source.spawn_flags, self.skill) {
                self.targets
                    .disable_entity(source_index as u16)
                    .map_err(EntityLoadError::BadTarget)?;
            }
        }
        let models = map.alias_models();

        for (source_index, source) in map.entities().iter().enumerate().skip(2) {
            if excluded_for_skill(source.spawn_flags, self.skill) {
                continue;
            }
            // `func_episodegate` only exists once its episode is finished, and
            // `func_bossgate` disappears once all four runes are in hand. In
            // shareware only Episode 1 can ever be completed, so the boss gate
            // stays shut forever.
            if source.class_name == CLASS_FUNC_EPISODEGATE
                && self.runes & (source.spawn_flags as u8 & RUNE_MASK) == 0
            {
                continue;
            }
            if source.class_name == CLASS_FUNC_BOSSGATE && self.runes & RUNE_MASK == RUNE_MASK {
                continue;
            }
            if source.class_name == CLASS_TRIGGER_CHANGELEVEL {
                let destination_name =
                    map.string_at(source.string)
                        .ok_or(EntityLoadError::BadChangeLevel {
                            entity: source_index as u16,
                        })?;
                let Some(destination) = EpisodeMap::from_cooked_name(destination_name) else {
                    // Start also contains registered-episode and end triggers.
                    // They remain inert in the shareware Episode 1 runtime.
                    continue;
                };
                if self.change_level_count == MAX_CHANGE_LEVELS {
                    return Err(EntityLoadError::TooManyChangeLevels);
                }
                self.change_levels[self.change_level_count] = Some(
                    change_level(map, source_index as u16, source, destination).ok_or(
                        EntityLoadError::BadChangeLevel {
                            entity: source_index as u16,
                        },
                    )?,
                );
                self.change_level_count += 1;
                continue;
            }
            if touch_trigger(source.class_name) {
                if self.triggers.len() == self.triggers.capacity() {
                    return Err(EntityLoadError::TooMany);
                }
                if let Some((mins, maxs)) = entity_brush_bounds(map, source) {
                    let registered_only = source.class_name == CLASS_TRIGGER_ONLY_REGISTERED;
                    self.triggers.push(Trigger {
                        source_index: source_index as u16,
                        mins,
                        maxs,
                        once: source.class_name != 0x4b && !registered_only,
                        armed: true,
                        cooldown: 0,
                        wait_ticks: if registered_only {
                            REGISTERED_RETRY_TICKS
                        } else {
                            fixed_seconds_to_ticks(source.wait, 12)
                        },
                        multi: MultiTrigger::new(source.class_name, source.health),
                    });
                }
                continue;
            }
            if source.class_name == CLASS_TRIGGER_TELEPORT {
                if self.teleports.len() == self.teleports.capacity() {
                    return Err(EntityLoadError::TooMany);
                }
                if let Some((mins, maxs)) = entity_brush_bounds(map, source) {
                    self.teleports.push(TeleportTrigger {
                        source_index: source_index as u16,
                        spawn_flags: source.spawn_flags,
                        mins,
                        maxs,
                        gate: TeleportGate::new(source.target_name),
                        cooldown: 0,
                    });
                }
                continue;
            }
            if source.model < 0 && brush_model_is_visible(source.class_name) {
                let model_index =
                    source
                        .model
                        .checked_neg()
                        .ok_or(EntityLoadError::MissingModel {
                            entity: source_index as u16,
                            model: source.model,
                        })? as usize;
                let model =
                    map.brush_models()
                        .get(model_index)
                        .ok_or(EntityLoadError::MissingModel {
                            entity: source_index as u16,
                            model: source.model,
                        })?;
                if self.entities.len() == self.entities.capacity() || model_index > u8::MAX as usize
                {
                    return Err(EntityLoadError::TooMany);
                }
                let render_index = self.entities.len();
                let mover = QuakeMover::from_entity(source, model).map_err(|error| {
                    EntityLoadError::BadMover {
                        entity: source_index as u16,
                        error,
                    }
                })?;
                // A train is placed on its first authored corner at load, so
                // its render, broad-phase, and collision bounds all start on
                // the chain instead of at the cooked brush origin.
                let train = QuakeTrain::from_entity(source, model, &map.entities());
                let offset = train
                    .map(|train| train.origin())
                    .or_else(|| mover.as_ref().map(|mover| mover.transform().origin))
                    .unwrap_or(source.origin);
                if let Some(policy) = mover {
                    if self.movers.len() == self.movers.capacity() {
                        return Err(EntityLoadError::TooMany);
                    }
                    let link_group = self.movers.len().min(u8::MAX as usize) as u8;
                    self.movers.push(SceneMover {
                        render_index: render_index as u16,
                        source: MoverSource::from_entity(&source),
                        policy,
                        activator: TargetActivator::None,
                        link_group,
                        key_cooldown: 0,
                        key_spent: false,
                        crush: BlockCrush::new(),
                        health: button_health(source),
                        max_health: button_health(source),
                        shot_open: false,
                    });
                }
                if let Some(policy) = train {
                    if self.trains.len() == self.trains.capacity() {
                        return Err(EntityLoadError::TooMany);
                    }
                    self.trains.push(SceneTrain {
                        render_index: render_index as u16,
                        policy,
                        crush: BlockCrush::new(),
                    });
                }
                let clip_mins = translated_model_bounds(offset, model.mins);
                let clip_maxs = translated_model_bounds(offset, model.maxs);
                let center = bounds_center(clip_mins, clip_maxs);
                let Some(leaf_index) = map.point_leaf_index(center) else {
                    return Err(EntityLoadError::BadLeaf {
                        entity: source_index as u16,
                    });
                };
                self.entities.push(RenderEntity {
                    source_index: source_index as u16,
                    origin: offset,
                    angles: source.angles,
                    model_id: source.model,
                    model_index: model_index as u8,
                    frame: 0,
                    skin: 0,
                    clip_mins,
                    clip_maxs,
                    leaf_index: leaf_index as u16,
                    light: 0x80,
                    solid: brush_model_is_solid(source.class_name),
                    visible: true,
                    animation_start: 0,
                    animation_end: 0,
                    hit_mins: Vec3I32::default(),
                    hit_maxs: Vec3I32::default(),
                    health: 0,
                    max_health: 0,
                    damageable: false,
                    pickup: None,
                    projectile: false,
                    pending_gib: false,
                    pending_explosion: false,
                    monster: None,
                    patrol: 0,
                });
                continue;
            }
            if source.class_name == CLASS_MISC_FIREBALL {
                if self.fireball_emitters.len() == self.fireball_emitters.capacity() {
                    return Err(EntityLoadError::TooMany);
                }
                let mut random = TrapRandom::new(source_index as u16);
                let remaining_ticks = traps::fireball_first_delay_ticks(&mut random);
                self.fireball_emitters.push(FireballEmitter {
                    source_index: source_index as u16,
                    speed: source.speed,
                    random,
                    remaining_ticks,
                });
                continue;
            }
            let Some(spawn) = render_spawn(source) else {
                continue;
            };
            let Some(model_index) = (0..models.len())
                .find(|&index| models.model_at(index).unwrap().header().id == spawn.model_id)
            else {
                return Err(EntityLoadError::MissingModel {
                    entity: source_index as u16,
                    model: spawn.model_id,
                });
            };
            let model = models
                .model_at(model_index)
                .expect("model index came from table");
            if self.entities.len() == self.entities.capacity() {
                return Err(EntityLoadError::TooMany);
            }
            let header = model.header();
            let animation = if alias_model_is_sprite(header) {
                SpawnAnimation::All { initial: 0 }
            } else {
                spawn.animation
            };
            let (frame, animation_start, animation_end) = animation.resolve(header);
            let combat = monster_profile(source.class_name, source.health)
                .or_else(|| explobox_profile(source.class_name));
            let clip_radius = model_clip_radius(header);
            let origin = [
                source.origin.x >> 12,
                source.origin.y >> 12,
                source.origin.z >> 12,
            ];
            let clip_mins = origin.map(|axis| {
                axis.saturating_sub(clip_radius as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16
            });
            let clip_maxs = origin.map(|axis| {
                axis.saturating_add(clip_radius as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16
            });
            let pickup = pickup_for_entity(source.class_name, source.spawn_flags);
            let monster = MonsterKind::from_class_name(source.class_name).map(|kind| {
                if source.class_name == CLASS_ZOMBIE
                    && source.spawn_flags & SPAWNFLAG_ZOMBIE_CRUCIFIED != 0
                {
                    MonsterRuntime::new_crucified(source_index as u16)
                } else {
                    let mut runtime = MonsterRuntime::new(kind, source_index as u16);
                    // `FindTarget` tests `spawnflags & 3`: bit 1 is the
                    // authored ambush flag and bit 2 the original's own
                    // workaround for the crucified zombie sharing bit 1.
                    runtime.set_ambush(source.spawn_flags & SPAWNFLAG_MONSTER_AMBUSH != 0);
                    runtime
                }
            });
            // `walkmonster_start_go`: a monster with a `target` patrols the
            // `path_corner` chain it names.
            let mut patrol = 0;
            if monster.is_some_and(|runtime| !runtime.crucified() && !runtime.kind().is_boss()) {
                let mut corner = PathCorner::EMPTY;
                if find_path_corner_into(&map.entities(), source.target, &mut corner) {
                    patrol = corner.source_index;
                }
            }
            let Some((leaf_index, light)) =
                alias_leaf_light(map, source.origin, pickup.is_some(), &self.light_styles)
            else {
                return Err(EntityLoadError::BadLeaf {
                    entity: source_index as u16,
                });
            };
            self.entities.push(RenderEntity {
                source_index: source_index as u16,
                origin: source.origin,
                angles: source.angles,
                model_id: spawn.model_id,
                model_index: model_index as u8,
                frame,
                skin: spawn.skin.min(header.skin_count.saturating_sub(1) as u8),
                clip_mins,
                clip_maxs,
                leaf_index,
                light,
                // `misc_explobox` is the only authored solid alias model.
                // Reuse this bit so it joins dynamic-body collision without
                // growing every render entity.
                solid: is_explobox(source.class_name),
                visible: source.class_name != CLASS_BOSS,
                animation_start,
                animation_end,
                hit_mins: combat
                    .map(|profile| translated_q12(source.origin, profile.mins))
                    .unwrap_or_default(),
                hit_maxs: combat
                    .map(|profile| translated_q12(source.origin, profile.maxs))
                    .unwrap_or_default(),
                health: combat.map(|profile| profile.health).unwrap_or(0),
                max_health: combat.map(|profile| profile.health).unwrap_or(0),
                // Chthon carries a body box but no weapon can hurt him: his
                // authored `event_lightning` chain is the only kill. A
                // crucified zombie is authored decoration: the original never
                // runs `monster_start_go` for it, so it never gains
                // FL_TAKEDAMAGE either.
                //
                // Known deviation: `monster_init` does leave a crucified
                // zombie SOLID_SLIDEBOX, and this runtime keeps it
                // non-blocking. Making wall-mounted decoration solid needs
                // route evidence of its own and is not part of this change.
                damageable: combat.is_some()
                    && source.class_name != CLASS_BOSS
                    && !(source.class_name == CLASS_ZOMBIE
                        && source.spawn_flags & SPAWNFLAG_ZOMBIE_CRUCIFIED != 0),
                pickup,
                projectile: false,
                pending_gib: false,
                pending_explosion: false,
                monster,
                patrol,
            });
        }
        self.drop_spawns_to_floor(map);
        self.link_doors(map);
        self.rocket_render_start =
            self.install_projectile_render_slots(map, ROCKET_MODEL_ID, MAX_ROCKETS)?;
        self.nail_render_start =
            self.install_projectile_render_slots(map, NAIL_MODEL_ID, NAIL_POOL_CAPACITY)?;
        self.grenade_render_start =
            self.install_projectile_render_slots(map, GRENADE_MODEL_ID, MAX_GRENADES)?;
        // Monster missiles share one pool and swap their alias model at launch,
        // so a map without the zombie gib or lava ball model simply denies
        // those launches instead of failing to load.
        self.missile_render_start =
            self.install_projectile_render_slots(map, GRENADE_MODEL_ID, MAX_MONSTER_MISSILES)?;
        // Only the maps that author lava spouts cook progs/lavaball.mdl.
        self.fireball_slots =
            (self.fireball_emitters.len() * FIREBALLS_PER_EMITTER).min(MAX_FIREBALLS);
        self.fireball_render_start = if self.fireball_slots == 0 {
            None
        } else {
            Some(self.install_projectile_render_slots(
                map,
                traps::FIREBALL_MODEL_ID,
                self.fireball_slots,
            )?)
        };
        // Preserve the former full-slice traversal order exactly. These are
        // immutable class properties for the level session: monsters may die
        // and movers may translate, but neither changes collision class.
        for (index, entity) in self.entities.iter().enumerate() {
            if entity.monster.is_some() || entity.solid {
                self.collision_indices.push(index as u16);
            }
        }
        Ok(())
    }

    /// `PF_droptofloor` as its two spawn callers use it: raise the authored
    /// origin a little, sweep the entity's own hull 256 units straight down,
    /// and stand it on whatever it hits.
    ///
    /// `walkmonster_start_go` lifts one unit and drops; a monster that lands
    /// nowhere or in solid only earns a "walkmonster in wall at ..." dprint,
    /// so this keeps its authored origin and its place in `total_monsters`.
    /// `PlaceItem` lifts six units and drops, and does remove a bonus item
    /// that found no floor ("bonus item fell out of level at ..."). Removal
    /// here is visibility plus the pickup, not a splice: `movers`, `trains`
    /// and `collision_indices` index this vector by position.
    ///
    /// `flymonster_start_go` and `swimmonster_start_go` carry no
    /// `droptofloor`, a crucified zombie never reaches `walkmonster_start`,
    /// and Chthon is placed by his own rise sequence, so all three are left
    /// where the map author put them.
    #[optimize(size)]
    fn drop_spawns_to_floor(&mut self, map: &ResidentMap) {
        for index in 0..self.entities.len() {
            let origin = self.entities[index].origin;
            // `item_sigil` is a `StartItem` like any other bonus, but the port
            // gives it no `Pickup` (a rune is not inventory), so it is spotted
            // by its own alias model instead of decoding the cooked class back.
            let sigil = self.entities[index].model_id == SIGIL_MODEL_ID;
            let pickup = self.entities[index].pickup.is_some() || sigil;
            let explobox = self.entities[index].solid && self.entities[index].model_id >= 0;
            // `SV_Move` sweeps `hull->clip_mins - mins` away from the origin.
            // Every walkmonster and every keyed item already sits at the hull
            // corner; the remaining item sizes start at `mins_z == 0`.
            let (hull, lift_units, hull_z_units) = match self.entities[index].monster {
                Some(runtime) => {
                    if runtime.crucified() || runtime.kind().is_boss() || runtime.kind().flies() {
                        continue;
                    }
                    (runtime.kind().collision_hull(), 1, 0)
                }
                None if pickup => {
                    let corner = sigil
                        || matches!(
                            self.entities[index].pickup,
                            Some(Pickup::Key { .. } | Pickup::Powerup { .. })
                        );
                    (1, 6, if corner { 0 } else { 24 })
                }
                // `misc_explobox` is 0..32/0..32/0..64. Hull 1 has the same
                // horizontal span; centring it and aligning its low face is
                // the `SV_Move` offset `droptofloor` applies to this box.
                None if explobox => (1, 2, 24),
                None => continue,
            };
            let centre = if explobox { 16 << 12 } else { 0 };
            let start = Vec3I32 {
                x: origin.x.saturating_add(centre),
                y: origin.y.saturating_add(centre),
                z: origin.z.saturating_add((lift_units + hull_z_units) << 12),
            };
            let end = Vec3I32 {
                z: start.z.saturating_sub(256 << 12),
                ..start
            };
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_hull(map, hull, &start, &end, &mut scratch, &mut trace) {
                continue;
            }
            if trace.fraction >= Q12_ONE || trace.all_solid {
                if pickup || explobox {
                    self.entities[index].visible = false;
                    self.entities[index].pickup = None;
                    self.entities[index].damageable = false;
                }
                continue;
            }
            let shift = trace
                .end
                .z
                .saturating_sub(hull_z_units << 12)
                .saturating_sub(origin.z);
            if shift == 0 {
                continue;
            }
            let dropped = Vec3I32 {
                z: origin.z.saturating_add(shift),
                ..origin
            };
            let Some((leaf_index, light)) =
                alias_leaf_light(map, dropped, pickup, &self.light_styles)
            else {
                continue;
            };
            let render = &mut self.entities[index];
            render.origin = dropped;
            render.clip_mins[2] = render.clip_mins[2].saturating_add((shift >> 12) as i16);
            render.clip_maxs[2] = render.clip_maxs[2].saturating_add((shift >> 12) as i16);
            if !pickup {
                render.hit_mins.z = render.hit_mins.z.saturating_add(shift);
                render.hit_maxs.z = render.hit_maxs.z.saturating_add(shift);
            }
            render.leaf_index = leaf_index;
            render.light = light;
        }
    }

    /// `LinkDoors`: merge every pair of `func_door` bodies whose closed bounds
    /// touch, unless either carries `DOOR_DONT_LINK`. Episode 1 authors each
    /// key lock as two half-doors, so a key opens both leaves at once.
    #[optimize(size)]
    fn link_doors(&mut self, map: &ResidentMap) {
        let sources = map.entities();
        let door = |scene: &Self, index: usize| -> Option<(u16, [i16; 3], [i16; 3])> {
            let entity = scene
                .entities
                .get(scene.movers[index].render_index as usize)?;
            let source = sources.get(entity.source_index as usize)?;
            (source.class_name == 0x0c && source.spawn_flags & door::DOOR_DONT_LINK == 0)
                .then_some((entity.source_index, entity.clip_mins, entity.clip_maxs))
        };
        for left in 0..self.movers.len() {
            let Some((_, left_mins, left_maxs)) = door(self, left) else {
                continue;
            };
            for right in (left + 1)..self.movers.len() {
                let Some((_, right_mins, right_maxs)) = door(self, right) else {
                    continue;
                };
                // `Mod_LoadBrushModel` grows every submodel's runtime bounds by
                // one unit on each axis, and that is the size `setmodel` hands
                // `EntitiesTouching`. Without it the authored door leaves miss
                // each other by exactly two units and never link.
                if self.movers[left].link_group == self.movers[right].link_group
                    || !door::entities_touching(
                        grown_whole_units(left_mins, -1),
                        grown_whole_units(left_maxs, 1),
                        grown_whole_units(right_mins, -1),
                        grown_whole_units(right_maxs, 1),
                    )
                {
                    continue;
                }
                let merged = self.movers[left].link_group;
                let replaced = self.movers[right].link_group;
                for mover in &mut self.movers {
                    if mover.link_group == replaced {
                        mover.link_group = merged;
                    }
                }
            }
        }
    }

    #[optimize(size)]
    fn install_projectile_render_slots(
        &mut self,
        map: &ResidentMap,
        model_id: i16,
        slot_count: usize,
    ) -> Result<u16, EntityLoadError> {
        let models = map.alias_models();
        let Some(model_index) = (0..models.len())
            .find(|&index| models.model_at(index).unwrap().header().id == model_id)
        else {
            return Err(EntityLoadError::MissingModel {
                entity: u16::MAX,
                model: model_id,
            });
        };
        if model_index > u8::MAX as usize
            || self.entities.len().saturating_add(slot_count) > self.entities.capacity()
        {
            return Err(EntityLoadError::TooMany);
        }
        let header = models
            .model_at(model_index)
            .expect("rocket model index came from table")
            .header();
        let clip_radius = model_clip_radius(header).max(PROJECTILE_CLIP_RADIUS_UNITS);
        let render_start = self.entities.len() as u16;
        for _ in 0..slot_count {
            let (clip_mins, clip_maxs) = alias_clip_bounds(Vec3I32::default(), clip_radius);
            self.entities.push(RenderEntity {
                source_index: u16::MAX,
                origin: Vec3I32::default(),
                angles: Vec3I16::default(),
                model_id,
                model_index: model_index as u8,
                frame: 0,
                skin: 0,
                clip_mins,
                clip_maxs,
                leaf_index: 0,
                light: 0,
                solid: false,
                visible: false,
                animation_start: 0,
                animation_end: 0,
                hit_mins: Vec3I32::default(),
                hit_maxs: Vec3I32::default(),
                health: 0,
                max_health: 0,
                damageable: false,
                pickup: None,
                projectile: true,
                pending_gib: false,
                pending_explosion: false,
                monster: None,
                patrol: 0,
            });
        }
        Ok(render_start)
    }

    #[inline(never)]
    pub fn update(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        // `killed_monsters` by rescan rather than by increment: a telefrag, a
        // splash kill and a lightning shock all reach the same runtime state,
        // and a scan cannot double-count or miss one of them.
        let mut killed = 0u16;
        for entity in &self.entities {
            if let Some(monster) = entity.monster {
                if !monster.crucified() && monster.dead() {
                    killed = killed.saturating_add(1);
                }
            }
        }
        self.monsters.set_killed(killed);
        self.animation_subtick += 1;
        if self.animation_subtick < ANIMATION_FRAME_DIVISOR {
            return;
        }
        self.animation_subtick = 0;
        for entity in &mut self.entities {
            if entity.visible && entity.monster.is_none() {
                entity.advance_animation();
            }
        }
    }

    /// Consume each overlapping implemented pickup at most once for the
    /// lifetime of this resident map. Shareware single player has no respawn.
    pub fn collect_pickups(
        &mut self,
        map: &ResidentMap,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        weapon: &mut WeaponState,
    ) -> PickupResult {
        let mut result = PickupResult::default();
        for index in 0..self.entities.len() {
            // Reject through a borrow. `RenderEntity` is 112 bytes and this
            // loop runs over the whole scene on every tick, so copying the
            // record before the tests streams the entire array through a CPU
            // with no data cache. Almost every entity fails on `pickup`.
            let entity = &self.entities[index];
            let Some(pickup) = entity.pickup else {
                continue;
            };
            if !entity.visible {
                continue;
            }
            // Every class's touch box lies inside the hull below, so an entity
            // whose hull misses the player cannot be touched: skip it before
            // decoding its cooked record (the decode dominated this loop).
            let (hull_mins, hull_maxs) = pickup_touch_hull(entity.origin);
            if !aabb_overlaps(player_mins, player_maxs, hull_mins, hull_maxs) {
                continue;
            }
            let entity = self.entities[index];
            let source = map
                .entities()
                .get(entity.source_index as usize)
                .unwrap_or_default();
            let (mins, maxs) = pickup_touch_bounds(source.class_name, entity.origin);
            if !aabb_overlaps(player_mins, player_maxs, mins, maxs) {
                continue;
            }
            let outcome = weapon.apply_pickup(pickup);
            if !outcome.consumed {
                continue;
            }
            self.entities[index].visible = false;
            self.entities[index].pickup = None;
            result.consumed = result.consumed.saturating_add(1);
            result.switched_weapon |= outcome.switched_weapon;
            result.last_source_index = Some(entity.source_index);
            result.sound_id = outcome.sound_id;
            result.message = outcome.message;
            // Every original item touch function ends in `activator = other;
            // SUB_UseTargets`. Across the whole shareware episode only the
            // keys author one -- E1M2's silver key, both of E1M6's, and
            // E1M8's -- and skipping it left those chains dead.
            if source.target != 0 || source.kill_target != 0 {
                self.fire_pickup_targets(map, entity.source_index, &mut result);
            }
        }
        // `item_sigil` is not an inventory item: `sigil_touch` folds its
        // spawnflags into `serverflags`, which survives every later map load.
        for index in 0..self.entities.len() {
            // Same borrow-before-copy rule as the pickup scan above.
            let entity = &self.entities[index];
            if !entity.visible {
                continue;
            }
            // Same hull pre-test: a sigil the player is not touching is skipped
            // either way, so only entities inside the hull need decoding.
            let (hull_mins, hull_maxs) = pickup_touch_hull(entity.origin);
            if !aabb_overlaps(player_mins, player_maxs, hull_mins, hull_maxs) {
                continue;
            }
            let entity = self.entities[index];
            let source = map
                .entities()
                .get(entity.source_index as usize)
                .unwrap_or_default();
            if source.class_name != CLASS_ITEM_SIGIL {
                continue;
            }
            let (mins, maxs) = pickup_touch_bounds(source.class_name, entity.origin);
            if !aabb_overlaps(player_mins, player_maxs, mins, maxs) {
                continue;
            }
            self.entities[index].visible = false;
            self.runes |= source.spawn_flags as u8 & RUNE_MASK;
            result.consumed = result.consumed.saturating_add(1);
            result.last_source_index = Some(entity.source_index);
            result.sound_id = Some(RUNE_SOUND_ID);
            // `sigil_touch` finishes with `activator = other; SUB_UseTargets`.
            // E1M7's sigil targets `monster_boss`, so skipping this left the
            // whole Chthon encounter unreachable through the authored chain.
            self.fire_pickup_targets(map, entity.source_index, &mut result);
        }
        result
    }

    /// `SUB_UseTargets` for a just-taken item, with the player as activator.
    fn fire_pickup_targets(
        &mut self,
        map: &ResidentMap,
        source_index: u16,
        result: &mut PickupResult,
    ) {
        let mut actions = TargetActions::new();
        let mut fired = GameplayResult::default();
        if let Err(error) = self.targets.fire_source_by(
            &map.entities(),
            source_index,
            TargetActivator::Player,
            &mut actions,
        ) {
            result.target_error = Some(error);
        }
        self.apply_target_actions(map, &mut actions, &mut fired);
        result.boss_awakened |= fired.boss_awakened;
        result.fired_target_edges = result
            .fired_target_edges
            .saturating_add(fired.fired_target_edges);
        if result.target_error.is_none() {
            result.target_error = fired.target_error;
        }
    }

    /// The nearest live shootable `trigger_multiple` a segment reaches before
    /// `limit`. Such a trigger is `SOLID_BBOX`, so it stops the shot as well as
    /// taking it.
    fn shootable_trigger_hit(
        &self,
        start: Vec3I32,
        end: Vec3I32,
        limit: i32,
    ) -> Option<(usize, i32)> {
        trigger::nearest_shot(
            start,
            end,
            limit,
            self.triggers.iter().map(|trigger| trigger::ShotCandidate {
                trigger: trigger.multi,
                enabled: trigger.armed && self.targets.is_enabled(trigger.source_index),
                mins: trigger.mins,
                maxs: trigger.maxs,
            }),
        )
    }

    /// `T_Damage` against a shootable trigger, ending in `multi_killed`.
    fn damage_trigger(&mut self, index: usize, damage: i16, result: &mut DamageResult) {
        let Some(trigger) = self.triggers.get_mut(index) else {
            return;
        };
        let applied = trigger.multi.take_damage(damage);
        if applied.applied == 0 {
            return;
        }
        result.damaged_targets = result.damaged_targets.saturating_add(1);
        result.total_damage = result.total_damage.saturating_add(applied.applied as u16);
        result.last_source_index = Some(trigger.source_index);
        if applied.killed {
            result.killed_targets = result.killed_targets.saturating_add(1);
        }
    }

    /// The nearest live shootable `func_button` a segment reaches before
    /// `limit`. Like a shootable trigger it is `SOLID_BBOX`, so it stops the
    /// shot as well as taking it.
    fn shootable_button_hit(
        &self,
        start: Vec3I32,
        end: Vec3I32,
        limit: i32,
    ) -> Option<(usize, i32)> {
        let mut best = None;
        let mut best_fraction = limit;
        for (index, mover) in self.movers.iter().enumerate() {
            if mover.health <= 0 || mover.shot_open {
                continue;
            }
            let Some(entity) = self.entities.get(mover.render_index as usize) else {
                continue;
            };
            if !self.targets.is_enabled(entity.source_index) {
                continue;
            }
            // Only a button that is down can be shot open; one already at the
            // top has nothing left to fire.
            if !matches!(
                mover.policy.state(),
                QuakeMoverState::Bottom | QuakeMoverState::Down
            ) {
                continue;
            }
            // The clip bounds are whole world units; the shot is Q20.12.
            let to_q12 = |bounds: [i16; 3]| Vec3I32 {
                x: i32::from(bounds[0]) << 12,
                y: i32::from(bounds[1]) << 12,
                z: i32::from(bounds[2]) << 12,
            };
            let Some(fraction) = segment_aabb_fraction(
                start,
                end,
                to_q12(entity.clip_mins),
                to_q12(entity.clip_maxs),
            ) else {
                continue;
            };
            // The BSP trace stops just before the impact plane, while the
            // conservative whole-unit button AABB begins at the cooked model
            // bound. E1M2 #243 proves the two representations can differ by
            // four Q12 fraction steps over Quake's 2048-unit hitscan. Admit
            // that measured bound for the first button only; anything farther
            // away remains behind the blocking brush.
            if fraction < best_fraction
                || best.is_none() && fraction <= best_fraction.saturating_add(4)
            {
                best_fraction = fraction;
                best = Some(index);
            }
        }
        best.map(|index| (index, best_fraction))
    }

    /// `T_Damage` against a shootable button, ending in `button_killed`.
    fn damage_button(&mut self, index: usize, damage: i16, result: &mut DamageResult) {
        let Some(mover) = self.movers.get_mut(index) else {
            return;
        };
        if mover.health <= 0 || damage <= 0 {
            return;
        }
        let source_index = mover.render_index;
        mover.health = mover.health.saturating_sub(damage);
        result.damaged_targets = result.damaged_targets.saturating_add(1);
        result.total_damage = result.total_damage.saturating_add(damage as u16);
        if mover.health <= 0 {
            // `button_killed` restores the authored health and fires.
            mover.health = mover.max_health;
            mover.shot_open = true;
            result.killed_targets = result.killed_targets.saturating_add(1);
        }
        result.last_source_index = self
            .entities
            .get(source_index as usize)
            .map(|entity| entity.source_index);
    }

    /// The nearest brush entity a shot can hurt: a shootable trigger or a
    /// shootable button, whichever the segment reaches first.
    #[optimize(size)]
    fn shootable_brush_hit(
        &self,
        start: Vec3I32,
        end: Vec3I32,
        limit: i32,
    ) -> Option<(ShotBrush, i32)> {
        let trigger = self.shootable_trigger_hit(start, end, limit);
        let button = self.shootable_button_hit(start, end, limit);
        match (trigger, button) {
            (Some((index, near)), Some((_, far))) if near <= far => {
                Some((ShotBrush::Trigger(index), near))
            }
            (_, Some((index, fraction))) => Some((ShotBrush::Button(index), fraction)),
            (Some((index, fraction)), None) => Some((ShotBrush::Trigger(index), fraction)),
            (None, None) => None,
        }
    }

    fn damage_shootable_brush(&mut self, hit: ShotBrush, damage: i16, result: &mut DamageResult) {
        match hit {
            ShotBrush::Trigger(index) => self.damage_trigger(index, damage, result),
            ShotBrush::Button(index) => self.damage_button(index, damage, result),
        }
    }

    pub fn fire_hitscan(
        &mut self,
        map: &ResidentMap,
        attack: HitscanAttack,
    ) -> Option<DamageResult> {
        let mut scratch = TraceScratch::default();
        let mut world = Trace::default();
        if !self.trace_point(map, &attack.start, &attack.end, &mut scratch, &mut world) {
            return None;
        }
        let mut best_fraction = world.fraction;
        let mut best_entity = None;
        for (index, entity) in self.entities.iter().enumerate() {
            if !entity.visible || !entity.damageable || entity.health <= 0 {
                continue;
            }
            let Some(fraction) =
                segment_aabb_fraction(attack.start, attack.end, entity.hit_mins, entity.hit_maxs)
            else {
                continue;
            };
            if fraction < best_fraction {
                best_fraction = fraction;
                best_entity = Some(index);
            }
        }
        let mut result = DamageResult::default();
        if let Some((hit, _)) = self.shootable_brush_hit(attack.start, attack.end, best_fraction) {
            self.damage_shootable_brush(hit, attack.damage, &mut result);
        } else if let Some(index) = best_entity {
            apply_entity_damage(
                map,
                self.nightmare(),
                &mut self.entities[index],
                attack.damage,
                DamageAttacker::Player,
                &mut result,
                &mut self.pending_scene_work,
            );
            if result.damaged_targets != 0 {
                result.last_impact =
                    Some(interpolate_impact(attack.start, attack.end, best_fraction));
            }
        } else if world.fraction < 1 << 12 {
            // `W_FireAxe` on something that does not take damage:
            // `sound (self, CHAN_WEAPON, "player/axhit2.wav", 1, ATTN_NORM)`.
            result.response_sound = Some(
                SoundEvent::listener(AXE_WALL_SOUND)
                    .on(crate::audio::OWNER_PLAYER, crate::audio::CHAN_WEAPON),
            );
        }
        Some(result)
    }

    /// `PF_aim` with `sv_aim` at its 0.93 default. `forward` is `v_forward`
    /// in Q12; the result is a unit Q12 direction with the yaw of `forward`
    /// and the pitch bent onto the best `DAMAGE_AIM` target (any live
    /// damageable entity: monsters and explobox barrels), or `forward` when
    /// a straight shot already lands on one or nothing qualifies.
    pub fn auto_aim(&self, map: &ResidentMap, player_origin: Vec3I32, forward: Vec3I32) -> Vec3I32 {
        // `sv_aim` 0.93 in Q12.
        const AIM_COSINE_Q12: i32 = 3809;
        let start = Vec3I32 {
            x: player_origin.x,
            y: player_origin.y,
            z: player_origin.z.saturating_add(20 << 12),
        };
        // `VectorMA (start, 2048, dir, end)`: a straight shot that already
        // hits something aimable keeps the raw view direction.
        let straight = Vec3I32 {
            x: start.x.saturating_add(forward.x.saturating_mul(2048)),
            y: start.y.saturating_add(forward.y.saturating_mul(2048)),
            z: start.z.saturating_add(forward.z.saturating_mul(2048)),
        };
        if self.aim_trace_target(map, start, straight).is_some() {
            return forward;
        }
        let mut best_dot = AIM_COSINE_Q12;
        let mut best_index = None;
        for (index, entity) in self.entities.iter().enumerate() {
            if !entity.visible || !entity.damageable || entity.health <= 0 {
                continue;
            }
            // `check.origin + 0.5 * (check.mins + check.maxs)`.
            let end = midpoint_vec_all(entity.hit_mins, entity.hit_maxs);
            let dot = dot_q12(normalize_q12(subtract_vec(end, start)), forward);
            if dot < best_dot {
                // Too far to turn.
                continue;
            }
            if self.aim_trace_target(map, start, end) == Some(index) {
                best_dot = dot;
                best_index = Some(index);
            }
        }
        let Some(index) = best_index else {
            return forward;
        };
        // Keep the horizontal heading, only bend the pitch onto the target:
        // `end = v_forward * (dir . v_forward); end_z = dir_z; normalize`.
        let dir = subtract_vec(self.entities[index].origin, player_origin);
        let along = dot_q12(dir, forward);
        normalize_q12(Vec3I32 {
            x: mul_q12_i32(along, forward.x),
            y: mul_q12_i32(along, forward.y),
            z: dir.z,
        })
    }

    /// The live damageable entity a point trace from `start` to `end` hits
    /// first, or `None` when the world or a mover stops it earlier (or it
    /// touches nothing at all).
    fn aim_trace_target(&self, map: &ResidentMap, start: Vec3I32, end: Vec3I32) -> Option<usize> {
        let mut scratch = TraceScratch::default();
        let mut world = Trace::default();
        if !self.trace_point(map, &start, &end, &mut scratch, &mut world) {
            return None;
        }
        let mut best_fraction = world.fraction;
        let mut best_entity = None;
        for (index, entity) in self.entities.iter().enumerate() {
            if !entity.visible || !entity.damageable || entity.health <= 0 {
                continue;
            }
            let Some(fraction) =
                segment_aabb_fraction(start, end, entity.hit_mins, entity.hit_maxs)
            else {
                continue;
            };
            if fraction < best_fraction {
                best_fraction = fraction;
                best_entity = Some(index);
            }
        }
        best_entity
    }

    #[optimize(size)]
    pub fn spawn_rocket(&mut self, map: &ResidentMap, spawn: RocketSpawn) -> bool {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let Some(slot_index) = self.rockets.iter().position(|slot| slot.is_none()) else {
            return false;
        };
        let render_index = self.rocket_render_start as usize + slot_index;
        let Some(render) = self.entities.get_mut(render_index) else {
            return false;
        };
        if !update_projectile_render(map, render, spawn.origin, spawn.step, &styles) {
            return false;
        }
        self.rockets[slot_index] = Some(RocketProjectile {
            origin: spawn.origin,
            step: spawn.step,
            remaining_ticks: spawn.lifetime_ticks,
            direct_damage: spawn.direct_damage,
        });
        self.trail_anchors[slot_index] = spawn.origin;
        true
    }

    pub fn begin_weapon_frame(&mut self) {
        self.lightning_beam_frames = self.lightning_beam_frames.saturating_sub(1);
        if self.lightning_beam_frames == 0 {
            self.lightning_beam = None;
        }
    }

    pub const fn lightning_beam(&self) -> Option<LightningBeam> {
        self.lightning_beam
    }

    pub fn attack_admission(&self) -> AttackAdmission {
        AttackAdmission {
            nail: self.nails.iter().any(Option::is_none),
            grenade: self.grenades.iter().any(Option::is_none),
            rocket: self.rockets.iter().any(Option::is_none),
        }
    }

    #[optimize(size)]
    pub fn spawn_nail(&mut self, map: &ResidentMap, spawn: NailSpawn) -> bool {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let Some(slot_index) = self.nails.iter().position(|slot| slot.is_none()) else {
            return false;
        };
        let render_index = self.nail_render_start as usize + slot_index;
        let Some(render) = self.entities.get_mut(render_index) else {
            return false;
        };
        if !update_projectile_render(map, render, spawn.origin, spawn.step, &styles) {
            return false;
        }
        self.nails[slot_index] = Some(NailProjectile {
            origin: spawn.origin,
            step: spawn.step,
            remaining_ticks: spawn.lifetime_ticks,
            damage: spawn.damage,
            hostile: false,
        });
        true
    }

    /// `spikeshooter_use`: launch one spike along the shooter's `movedir`.
    ///
    /// The shared nail pool is reused, so a trap spike competes with the
    /// player's own nails for slots exactly like every other projectile.
    #[optimize(size)]
    fn fire_shooter(&mut self, map: &ResidentMap, source_index: u16) -> Option<SoundEvent> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let source = map.entities().get(source_index as usize)?;
        if source.spawn_flags & traps::SPAWNFLAG_LASER != 0 {
            // No authored shareware Episode 1 shooter sets the laser bit, so
            // LaunchLaser has no cooked model or sound to reach for.
            return None;
        }
        let slot_index = self.nails.iter().position(|slot| slot.is_none())?;
        let step = traps::spike_step(move_direction(source.angles));
        let render_index = self.nail_render_start as usize + slot_index;
        let render = self.entities.get_mut(render_index)?;
        if !update_projectile_render(map, render, source.origin, step, &styles) {
            return None;
        }
        self.nails[slot_index] = Some(NailProjectile {
            origin: source.origin,
            step,
            remaining_ticks: traps::SPIKE_LIFETIME_TICKS,
            damage: traps::spikeshooter_damage(source.spawn_flags),
            hostile: true,
        });
        Some(SoundEvent::at(traps::SPIKE_SOUND_ID, source.origin))
    }

    #[optimize(size)]
    pub fn spawn_grenade(&mut self, map: &ResidentMap, spawn: GrenadeSpawn) -> bool {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let Some(slot_index) = self.grenades.iter().position(|slot| slot.is_none()) else {
            return false;
        };
        let render_index = self.grenade_render_start as usize + slot_index;
        let Some(render) = self.entities.get_mut(render_index) else {
            return false;
        };
        if !update_projectile_render(map, render, spawn.origin, spawn.velocity, &styles) {
            return false;
        }
        self.grenades[slot_index] = Some(GrenadeProjectile {
            origin: spawn.origin,
            velocity: spawn.velocity,
            angles: spawn.angles,
            angular_velocity: Vec3I16 { x: 57, y: 0, z: 0 },
            resting: false,
            remaining_ticks: spawn.lifetime_ticks,
            damage: spawn.damage,
        });
        self.trail_anchors[MAX_ROCKETS + slot_index] = spawn.origin;
        true
    }

    /// Advance a fixed eight-slot rocket pool and collide each Q20.12 segment
    /// with the world, translated movers, and damageable entity bounds.
    pub fn update_rockets(
        &mut self,
        map: &ResidentMap,
        player_origin: Vec3I32,
        weapon: &mut WeaponState,
        elapsed_ticks: u16,
    ) -> Option<RocketResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let mut result = RocketResult::default();
        let ticks = rocket_elapsed_ticks(elapsed_ticks);
        if ticks == 0 {
            return Some(result);
        }
        let mut rocket_index = 0usize;
        while rocket_index < self.rockets.len() {
            let Some(mut rocket) = self.rockets[rocket_index] else {
                rocket_index += 1;
                continue;
            };
            let mut tick = 0u16;
            let mut removed = false;
            while tick < ticks {
                if projectile_expires_this_tick(&mut rocket.remaining_ticks) {
                    removed = true;
                    break;
                }
                let end = add_vec(rocket.origin, rocket.step);
                let mut scratch = TraceScratch::default();
                let mut world = Trace::default();
                if !self.trace_point(map, &rocket.origin, &end, &mut scratch, &mut world) {
                    return None;
                }
                let mut best_fraction = world.fraction;
                let mut direct_entity = None;
                for (index, entity) in self.entities.iter().enumerate() {
                    if !entity.visible || !entity.damageable || entity.health <= 0 {
                        continue;
                    }
                    let Some(fraction) =
                        segment_aabb_fraction(rocket.origin, end, entity.hit_mins, entity.hit_maxs)
                    else {
                        continue;
                    };
                    if fraction < best_fraction {
                        best_fraction = fraction;
                        direct_entity = Some(index);
                    }
                }
                // A shootable trigger is solid to a rocket as well: the rocket
                // stops on it and `multi_killed` takes the direct damage. A
                // shootable button is solid on the same terms.
                let direct_trigger = self
                    .shootable_brush_hit(rocket.origin, end, best_fraction)
                    .map(|(hit, fraction)| {
                        best_fraction = fraction;
                        direct_entity = None;
                        hit
                    });
                if best_fraction < 1 << 12 {
                    let impact = interpolate_segment(rocket.origin, end, best_fraction);
                    if let Some(trigger) = direct_trigger {
                        let mut applied = DamageResult::default();
                        self.damage_shootable_brush(trigger, rocket.direct_damage, &mut applied);
                        merge_rocket_damage(&mut result, applied);
                    }
                    if direct_entity.is_none()
                        && direct_trigger.is_none()
                        && world_point_contents(map, end) == Some(CONTENTS_SKY)
                    {
                        result.sky_removals = result.sky_removals.saturating_add(1);
                        removed = true;
                        break;
                    }
                    self.apply_rocket_impact(
                        map,
                        impact,
                        rocket.direct_damage,
                        quake_core::combat::ROCKET_SPLASH_DAMAGE,
                        direct_entity,
                        ExplosionKind::Rocket.radius_ignores_direct_target(),
                        Some(player_origin),
                        DamageAttacker::Player,
                        weapon,
                        &mut result,
                    )?;
                    removed = true;
                    break;
                }
                rocket.origin = end;
                tick += 1;
            }
            self.rockets[rocket_index] = (!removed).then_some(rocket);
            let render_index = self.rocket_render_start as usize + rocket_index;
            let Some(render) = self.entities.get_mut(render_index) else {
                return None;
            };
            if removed {
                render.visible = false;
            } else if !update_projectile_render(map, render, rocket.origin, rocket.step, &styles) {
                return None;
            }
            rocket_index += 1;
        }
        Some(result)
    }

    pub fn update_nails(
        &mut self,
        map: &ResidentMap,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        elapsed_ticks: u16,
    ) -> Option<NailResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let mut result = NailResult::default();
        let ticks = rocket_elapsed_ticks(elapsed_ticks);
        let mut nail_index = 0usize;
        while nail_index < self.nails.len() {
            let Some(mut nail) = self.nails[nail_index] else {
                nail_index += 1;
                continue;
            };
            let mut tick = 0u16;
            let mut removed = false;
            while tick < ticks {
                if projectile_expires_this_tick(&mut nail.remaining_ticks) {
                    removed = true;
                    break;
                }
                let end = add_vec(nail.origin, nail.step);
                let mut scratch = TraceScratch::default();
                let mut world = Trace::default();
                if !self.trace_point(map, &nail.origin, &end, &mut scratch, &mut world) {
                    return None;
                }
                let mut best_fraction = world.fraction;
                let mut direct_entity = None;
                for (index, entity) in self.entities.iter().enumerate() {
                    if !entity.visible || !entity.damageable || entity.health <= 0 {
                        continue;
                    }
                    let Some(fraction) =
                        segment_aabb_fraction(nail.origin, end, entity.hit_mins, entity.hit_maxs)
                    else {
                        continue;
                    };
                    if fraction < best_fraction {
                        best_fraction = fraction;
                        direct_entity = Some(index);
                    }
                }
                // `SOLID_BBOX` stops a spike the same way it stops a bullet.
                let mut direct_trigger = self
                    .shootable_brush_hit(nail.origin, end, best_fraction)
                    .map(|(hit, fraction)| {
                        best_fraction = fraction;
                        direct_entity = None;
                        hit
                    });
                // A trap spike is owned by the shooter, so `spike_touch` hits
                // the player instead of passing through.
                let mut hit_player = false;
                if nail.hostile {
                    if let Some(fraction) =
                        segment_aabb_fraction(nail.origin, end, player_mins, player_maxs)
                    {
                        if fraction < best_fraction {
                            best_fraction = fraction;
                            direct_entity = None;
                            direct_trigger = None;
                            result.player_damage = result
                                .player_damage
                                .saturating_add(nail.damage.max(0) as u16);
                            // `T_Damage (other, self, self.owner, 9)`: the
                            // spike itself is the inflictor.
                            result.player_impulse = add_vec(
                                result.player_impulse,
                                knockback_impulse(
                                    player_origin_from_mins(player_mins),
                                    interpolate_segment(nail.origin, end, fraction),
                                    nail.damage,
                                ),
                            );
                            hit_player = true;
                        }
                    }
                }
                if best_fraction < 1 << 12 {
                    result.impacts = result.impacts.saturating_add(1);
                    if let Some(trigger) = direct_trigger {
                        self.damage_shootable_brush(trigger, nail.damage, &mut result.damage);
                    } else if let Some(index) = direct_entity {
                        apply_entity_damage(
                            map,
                            self.nightmare(),
                            &mut self.entities[index],
                            nail.damage,
                            DamageAttacker::Player,
                            &mut result.damage,
                            &mut self.pending_scene_work,
                        );
                        if result.damage.damaged_targets != 0 {
                            result.damage.last_impact =
                                Some(interpolate_segment(nail.origin, end, best_fraction));
                        }
                    } else if !hit_player {
                        // `spike_touch` on the world: gone silently into the
                        // sky, otherwise `TE_SPIKE` (the client's tink).
                        if world_point_contents(map, end) != Some(CONTENTS_SKY) {
                            result.world_impacts = result.world_impacts.saturating_add(1);
                            result.last_world_impact = Some(end);
                        }
                    }
                    removed = true;
                    break;
                }
                nail.origin = end;
                tick += 1;
            }
            self.nails[nail_index] = (!removed).then_some(nail);
            let render_index = self.nail_render_start as usize + nail_index;
            let render = self.entities.get_mut(render_index)?;
            if removed {
                render.visible = false;
            } else if !update_projectile_render(map, render, nail.origin, nail.step, &styles) {
                return None;
            }
            nail_index += 1;
        }
        Some(result)
    }

    /// `fire_fly` and `fire_touch`: every authored lava spout lobs a ball on
    /// its own stagger and each ball damages whatever it lands on.
    pub fn update_fireballs(
        &mut self,
        map: &ResidentMap,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        elapsed_ticks: u16,
    ) -> Option<FireballResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        const GRAVITY_STEP_Q12: i32 = 910; // 800 units/second squared at 60 Hz.
        let Some(render_start) = self.fireball_render_start else {
            return None;
        };
        let mut result = FireballResult::default();
        let ticks = rocket_elapsed_ticks(elapsed_ticks);
        for emitter_index in 0..self.fireball_emitters.len() {
            let emitter = self.fireball_emitters[emitter_index];
            if emitter.remaining_ticks > ticks {
                self.fireball_emitters[emitter_index].remaining_ticks -= ticks;
                continue;
            }
            let mut random = emitter.random;
            let Some(source) = map.entities().get(emitter.source_index as usize) else {
                continue;
            };
            let velocity = traps::fireball_velocity(&mut random, emitter.speed);
            self.fireball_emitters[emitter_index].remaining_ticks =
                traps::fireball_next_delay_ticks(&mut random);
            self.fireball_emitters[emitter_index].random = random;
            let Some(slot_index) = self.fireballs[..self.fireball_slots]
                .iter()
                .position(|slot| slot.is_none())
            else {
                continue;
            };
            let render_index = render_start as usize + slot_index;
            let Some(render) = self.entities.get_mut(render_index) else {
                continue;
            };
            if !update_projectile_render(map, render, source.origin, velocity, &styles) {
                continue;
            }
            self.fireballs[slot_index] = Some(FireballProjectile {
                origin: source.origin,
                velocity,
                remaining_ticks: traps::FIREBALL_LIFETIME_TICKS,
            });
            result.launched = result.launched.saturating_add(1);
        }

        for slot_index in 0..self.fireball_slots {
            let Some(mut ball) = self.fireballs[slot_index] else {
                continue;
            };
            let mut tick = 0u16;
            let mut removed = false;
            while tick < ticks {
                if projectile_expires_this_tick(&mut ball.remaining_ticks) {
                    removed = true;
                    break;
                }
                ball.velocity.z = ball.velocity.z.saturating_sub(GRAVITY_STEP_Q12);
                let end = add_vec(ball.origin, ball.velocity);
                let mut scratch = TraceScratch::default();
                let mut world = Trace::default();
                if !self.trace_point(map, &ball.origin, &end, &mut scratch, &mut world) {
                    return None;
                }
                let hit_player = segment_aabb_fraction(ball.origin, end, player_mins, player_maxs)
                    .is_some_and(|fraction| fraction <= world.fraction);
                if hit_player {
                    result.player_damage = result
                        .player_damage
                        .saturating_add(traps::FIREBALL_DAMAGE as u16);
                    // `fire_touch`: `T_Damage (other, self, self, 20)`, so the
                    // ball at its contact point pushes the player.
                    result.player_impulse = add_vec(
                        result.player_impulse,
                        knockback_impulse(
                            player_origin_from_mins(player_mins),
                            ball.origin,
                            traps::FIREBALL_DAMAGE,
                        ),
                    );
                }
                if hit_player || world.fraction < 1 << 12 {
                    result.impacts = result.impacts.saturating_add(1);
                    removed = true;
                    break;
                }
                ball.origin = end;
                tick += 1;
            }
            self.fireballs[slot_index] = (!removed).then_some(ball);
            let render_index = render_start as usize + slot_index;
            let render = self.entities.get_mut(render_index)?;
            if removed {
                render.visible = false;
            } else if !update_projectile_render(map, render, ball.origin, ball.velocity, &styles) {
                return None;
            }
        }
        Some(result)
    }

    pub fn update_grenades(
        &mut self,
        map: &ResidentMap,
        player_origin: Vec3I32,
        weapon: &mut WeaponState,
        elapsed_ticks: u16,
    ) -> Option<GrenadeResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        const GRAVITY_STEP_Q12: i32 = 910; // 800 units/second squared at 60 Hz.
        const OVERBOUNCE_Q12: i32 = 6_144; // Quake MOVETYPE_BOUNCE uses 1.5.
        let mut result = GrenadeResult::default();
        let ticks = rocket_elapsed_ticks(elapsed_ticks);
        let mut grenade_index = 0usize;
        while grenade_index < self.grenades.len() {
            let Some(mut grenade) = self.grenades[grenade_index] else {
                grenade_index += 1;
                continue;
            };
            let mut tick = 0u16;
            let mut removed = false;
            while tick < ticks {
                match grenade_tick(&mut grenade.remaining_ticks, grenade.resting) {
                    GrenadeTick::Explode => {
                        self.apply_rocket_impact(
                            map,
                            grenade.origin,
                            0,
                            grenade.damage,
                            None,
                            ExplosionKind::Grenade.radius_ignores_direct_target(),
                            Some(player_origin),
                            DamageAttacker::Player,
                            weapon,
                            &mut result.damage,
                        )?;
                        result.explosions = result.explosions.saturating_add(1);
                        removed = true;
                        break;
                    }
                    GrenadeTick::Rest => {
                        tick += 1;
                        continue;
                    }
                    GrenadeTick::Move => {}
                }
                grenade.angles.x = grenade.angles.x.wrapping_add(grenade.angular_velocity.x);
                grenade.angles.y = grenade.angles.y.wrapping_add(grenade.angular_velocity.y);
                grenade.angles.z = grenade.angles.z.wrapping_add(grenade.angular_velocity.z);
                grenade.velocity.z = grenade.velocity.z.saturating_sub(GRAVITY_STEP_Q12);
                let end = add_vec(grenade.origin, grenade.velocity);
                let mut scratch = TraceScratch::default();
                let mut world = Trace::default();
                if !self.trace_point(map, &grenade.origin, &end, &mut scratch, &mut world) {
                    return None;
                }
                let mut best_fraction = world.fraction;
                let mut direct_entity = None;
                for (index, entity) in self.entities.iter().enumerate() {
                    if !entity.visible || !entity.damageable || entity.health <= 0 {
                        continue;
                    }
                    let Some(fraction) = segment_aabb_fraction(
                        grenade.origin,
                        end,
                        entity.hit_mins,
                        entity.hit_maxs,
                    ) else {
                        continue;
                    };
                    if fraction < best_fraction {
                        best_fraction = fraction;
                        direct_entity = Some(index);
                    }
                }
                // A shootable trigger is `SOLID_BBOX`. A grenade bounces from
                // it (DAMAGE_YES is not DAMAGE_AIM), then its eventual
                // `T_RadiusDamage` can fire the target. Buttons already join
                // the ordinary solid-brush trace above.
                let brush_hit = self.shootable_trigger_hit(grenade.origin, end, best_fraction);
                if let Some((_, fraction)) = brush_hit {
                    best_fraction = fraction;
                    direct_entity = None;
                }
                if let Some(index) = direct_entity {
                    let impact = interpolate_impact(grenade.origin, end, best_fraction);
                    self.apply_rocket_impact(
                        map,
                        impact,
                        0,
                        grenade.damage,
                        Some(index),
                        ExplosionKind::Grenade.radius_ignores_direct_target(),
                        Some(player_origin),
                        DamageAttacker::Player,
                        weapon,
                        &mut result.damage,
                    )?;
                    result.explosions = result.explosions.saturating_add(1);
                    removed = true;
                    break;
                }
                if best_fraction < 1 << 12 {
                    grenade.origin = interpolate_segment(grenade.origin, end, best_fraction);
                    let normal = brush_hit.map_or(world.normal, |(index, _)| {
                        let trigger = self.triggers[index];
                        aabb_impact_normal(grenade.origin, trigger.mins, trigger.maxs)
                    });
                    let dot = mul_q12_i32(grenade.velocity.x, i32::from(normal.x))
                        .saturating_add(mul_q12_i32(grenade.velocity.y, i32::from(normal.y)))
                        .saturating_add(mul_q12_i32(grenade.velocity.z, i32::from(normal.z)));
                    let impulse = mul_q12_i32(dot, OVERBOUNCE_Q12);
                    grenade.velocity.x = grenade
                        .velocity
                        .x
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.x)));
                    grenade.velocity.y = grenade
                        .velocity
                        .y
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.y)));
                    grenade.velocity.z = grenade
                        .velocity
                        .z
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.z)));
                    if settle_grenade_motion(
                        &mut grenade.velocity,
                        &mut grenade.angular_velocity,
                        normal.z,
                    ) {
                        grenade.resting = true;
                        result.rests = result.rests.saturating_add(1);
                    }
                    result.bounces = result.bounces.saturating_add(1);
                    result.last_bounce = Some(grenade.origin);
                } else {
                    grenade.origin = end;
                }
                tick += 1;
            }
            self.grenades[grenade_index] = (!removed).then_some(grenade);
            let render_index = self.grenade_render_start as usize + grenade_index;
            let render = self.entities.get_mut(render_index)?;
            if removed {
                render.visible = false;
            } else if !update_projectile_render(
                map,
                render,
                grenade.origin,
                grenade.velocity,
                &styles,
            ) {
                return None;
            } else {
                render.angles = grenade.angles;
            }
            grenade_index += 1;
        }
        Some(result)
    }

    pub fn fire_lightning(
        &mut self,
        map: &ResidentMap,
        attack: LightningAttack,
    ) -> Option<LightningResult> {
        let mut scratch = TraceScratch::default();
        let mut world = Trace::default();
        if !self.trace_point(
            map,
            &attack.beam_start,
            &attack.end,
            &mut scratch,
            &mut world,
        ) {
            return None;
        }
        let geometry = lightning_trace_geometry(attack, world.fraction);
        let mut hit_entities = [None; 3];
        let mut hit_count = 0usize;
        let mut result = LightningResult {
            world_clipped: world.fraction < 1 << 12,
            trace_end: geometry.beam_end,
            side_end: geometry.ends[1],
            ..LightningResult::default()
        };
        let mut line = 0usize;
        while line < 3 {
            let mut scratch = TraceScratch::default();
            let mut world = Trace::default();
            if !self.trace_point(
                map,
                &geometry.starts[line],
                &geometry.ends[line],
                &mut scratch,
                &mut world,
            ) {
                return None;
            }
            let mut best_fraction = world.fraction;
            let mut best_entity = None;
            for (index, entity) in self.entities.iter().enumerate() {
                if !entity.visible || !entity.damageable || entity.health <= 0 {
                    continue;
                }
                let Some(fraction) = segment_aabb_fraction(
                    geometry.starts[line],
                    geometry.ends[line],
                    entity.hit_mins,
                    entity.hit_maxs,
                ) else {
                    continue;
                };
                if fraction < best_fraction {
                    best_fraction = fraction;
                    best_entity = Some(index);
                }
            }
            if let Some((hit, _)) =
                self.shootable_brush_hit(geometry.starts[line], geometry.ends[line], best_fraction)
            {
                self.damage_shootable_brush(hit, attack.damage, &mut result.damage);
            } else if let Some(index) = best_entity {
                if !hit_entities[..hit_count].contains(&Some(index)) {
                    let damaged_before = result.damage.damaged_targets;
                    apply_entity_damage(
                        map,
                        self.nightmare(),
                        &mut self.entities[index],
                        attack.damage,
                        DamageAttacker::Player,
                        &mut result.damage,
                        &mut self.pending_scene_work,
                    );
                    if result.damage.damaged_targets != damaged_before {
                        result.damage.last_impact = Some(interpolate_impact(
                            geometry.starts[line],
                            geometry.ends[line],
                            best_fraction,
                        ));
                    }
                    hit_entities[hit_count] = Some(index);
                    hit_count += 1;
                }
            }
            line += 1;
        }
        self.lightning_beam = Some(LightningBeam {
            start: geometry.beam_start,
            end: geometry.beam_end,
        });
        self.lightning_beam_frames = 2;
        Some(result)
    }

    pub fn fire_lightning_discharge(
        &mut self,
        map: &ResidentMap,
        discharge: LightningDischarge,
        player_origin: Vec3I32,
        weapon: &mut WeaponState,
    ) -> Option<LightningResult> {
        let mut result = LightningResult {
            discharge: true,
            ..LightningResult::default()
        };
        self.lightning_beam = None;
        self.lightning_beam_frames = 0;
        let mut index = 0usize;
        while index < self.entities.len() {
            if !self.entities[index].visible
                || !self.entities[index].damageable
                || self.entities[index].health <= 0
            {
                index += 1;
                continue;
            }
            let target =
                midpoint_vec_all(self.entities[index].hit_mins, self.entities[index].hit_maxs);
            let distance = distance_units(discharge.origin, target);
            let class_name = map
                .entities()
                .get(self.entities[index].source_index as usize)
                .unwrap_or_default()
                .class_name;
            let points = explosion_splash_points(
                discharge.damage,
                distance,
                false,
                true,
                class_name == 0x41,
            );
            if points <= 0 {
                index += 1;
                continue;
            }
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_point(map, &discharge.origin, &target, &mut scratch, &mut trace) {
                return None;
            }
            if trace.fraction == 1 << 12 {
                apply_entity_damage(
                    map,
                    self.nightmare(),
                    &mut self.entities[index],
                    points,
                    DamageAttacker::Player,
                    &mut result.damage,
                    &mut self.pending_scene_work,
                );
            }
            index += 1;
        }
        let player_distance = distance_units(discharge.origin, player_origin);
        let player_points =
            explosion_splash_points(discharge.damage, player_distance, true, true, false);
        let _ = weapon.take_damage(player_points);
        Some(result)
    }

    #[optimize(size)]
    fn apply_rocket_impact(
        &mut self,
        map: &ResidentMap,
        impact: Vec3I32,
        direct_damage: i16,
        splash_damage: i16,
        direct_entity: Option<usize>,
        radius_ignores_direct: bool,
        player_origin: Option<Vec3I32>,
        attacker: DamageAttacker,
        weapon: &mut WeaponState,
        result: &mut RocketResult,
    ) -> Option<()> {
        result.impacts = result.impacts.saturating_add(1);
        result.last_impact = Some(impact);
        // `T_Damage`'s `self != attacker`: a monster caught in its own splash
        // does not turn on itself.
        let attacker_for = |index: usize| match attacker {
            DamageAttacker::Monster { index: owner, .. } if usize::from(owner) == index => {
                DamageAttacker::World
            }
            other => other,
        };
        if let Some(index) = direct_entity {
            let class_name = map
                .entities()
                .get(self.entities[index].source_index as usize)
                .unwrap_or_default()
                .class_name;
            let direct = rocket_direct_points(direct_damage, class_name == 0x41);
            let mut damage = DamageResult::default();
            apply_entity_damage(
                map,
                self.nightmare(),
                &mut self.entities[index],
                direct,
                attacker_for(index),
                &mut damage,
                &mut self.pending_scene_work,
            );
            result.direct_hits = result.direct_hits.saturating_add(damage.damaged_targets);
            merge_rocket_damage(result, damage);
        }

        let mut index = 0usize;
        while index < self.entities.len() {
            if (radius_ignores_direct && Some(index) == direct_entity)
                || !self.entities[index].visible
                || !self.entities[index].damageable
                || self.entities[index].health <= 0
            {
                index += 1;
                continue;
            }
            let target =
                midpoint_vec_all(self.entities[index].hit_mins, self.entities[index].hit_maxs);
            let distance = distance_units(impact, target);
            let class_name = map
                .entities()
                .get(self.entities[index].source_index as usize)
                .unwrap_or_default()
                .class_name;
            let points =
                explosion_splash_points(splash_damage, distance, false, true, class_name == 0x41);
            if points <= 0 {
                index += 1;
                continue;
            }
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_point(map, &impact, &target, &mut scratch, &mut trace) {
                return None;
            }
            if trace.fraction != 1 << 12 {
                index += 1;
                continue;
            }
            let mut damage = DamageResult::default();
            apply_entity_damage(
                map,
                self.nightmare(),
                &mut self.entities[index],
                points,
                attacker_for(index),
                &mut damage,
                &mut self.pending_scene_work,
            );
            result.splash_hits = result.splash_hits.saturating_add(damage.damaged_targets);
            merge_rocket_damage(result, damage);
            index += 1;
        }

        // `findradius` includes damageable trigger volumes. A direct rocket
        // has already dropped its trigger's health to zero, while a grenade's
        // timed blast reaches the still-live volume it bounced away from.
        let mut trigger_index = 0usize;
        while trigger_index < self.triggers.len() {
            let trigger = self.triggers[trigger_index];
            if trigger.multi.takes_damage() {
                let target = midpoint_vec_all(trigger.mins, trigger.maxs);
                let points = explosion_splash_points(
                    splash_damage,
                    distance_units(impact, target),
                    false,
                    true,
                    false,
                );
                let mut scratch = TraceScratch::default();
                let mut trace = Trace::default();
                if !self.trace_point(map, &impact, &target, &mut scratch, &mut trace) {
                    return None;
                }
                if points > 0 && trace.fraction == Q12_ONE {
                    let mut damage = DamageResult::default();
                    self.damage_trigger(trigger_index, points, &mut damage);
                    result.splash_hits = result.splash_hits.saturating_add(damage.damaged_targets);
                    merge_rocket_damage(result, damage);
                }
            }
            trigger_index += 1;
        }

        // `T_RadiusDamage`'s `ignore` argument: a lava ball that hit the
        // player directly leaves them out of its splash.
        let Some(player_origin) = player_origin else {
            return Some(());
        };
        let player_distance = distance_units(impact, player_origin);
        let mut player_scratch = TraceScratch::default();
        let mut player_trace = Trace::default();
        if !self.trace_point(
            map,
            &impact,
            &player_origin,
            &mut player_scratch,
            &mut player_trace,
        ) {
            return None;
        }
        let player_points = explosion_splash_points(
            splash_damage,
            player_distance,
            true,
            player_trace.fraction == 1 << 12,
            false,
        );
        if player_points > 0 {
            let taken = weapon.take_damage(player_points);
            result.self_damage = result.self_damage.saturating_add(taken as u16);
            // `T_Damage`: the push uses the points before armour, and the
            // player's own rocket pushes just the same (rocket jumping).
            result.player_impulse = add_vec(
                result.player_impulse,
                knockback_impulse(player_origin, impact, player_points),
            );
        }
        Some(())
    }

    /// Trace all six Quake shotgun pellets through the point hull and current
    /// translated brush movers, then apply accumulated damage to authored
    /// monster hitboxes. A malformed collision hull fails closed and leaves
    /// every entity unchanged.
    pub fn fire_shotgun(
        &mut self,
        map: &ResidentMap,
        attack: &ShotgunAttack,
    ) -> Option<ShotgunResult> {
        #[derive(Copy, Clone, Default)]
        struct DamageSlot {
            entity_index: u16,
            pellets: u8,
            occupied: bool,
            /// Set when `entity_index` indexes `self.movers` (a shootable
            /// button) instead of `self.triggers`.
            button: bool,
            impact: Vec3I32,
        }

        let mut slots = [DamageSlot::default(); MAX_SHOTGUN_PELLETS];
        let mut slot_count = 0usize;
        let mut trigger_slots = [DamageSlot::default(); MAX_SHOTGUN_PELLETS];
        let mut trigger_slot_count = 0usize;
        let mut scratch = TraceScratch::default();
        for &end in &attack.ends[..attack.pellet_count as usize] {
            let mut world = Trace::default();
            if !self.trace_point(map, &attack.start, &end, &mut scratch, &mut world) {
                return None;
            }
            let mut best_fraction = world.fraction;
            let mut best_entity = None;
            for (index, entity) in self.entities.iter().enumerate() {
                if !entity.visible || !entity.damageable || entity.health <= 0 {
                    continue;
                }
                let Some(fraction) =
                    segment_aabb_fraction(attack.start, end, entity.hit_mins, entity.hit_maxs)
                else {
                    continue;
                };
                if fraction < best_fraction {
                    best_fraction = fraction;
                    best_entity = Some(index);
                }
            }
            // A shootable trigger stands in front of everything the pellet
            // would otherwise reach, so it collects the pellet instead.
            if let Some((hit, _)) = self.shootable_brush_hit(attack.start, end, best_fraction) {
                let (index, button) = match hit {
                    ShotBrush::Trigger(index) => (index, false),
                    ShotBrush::Button(index) => (index, true),
                };
                if let Some(slot) = trigger_slots[..trigger_slot_count]
                    .iter_mut()
                    .find(|slot| slot.entity_index as usize == index && slot.button == button)
                {
                    slot.pellets = slot.pellets.saturating_add(1);
                } else {
                    trigger_slots[trigger_slot_count] = DamageSlot {
                        entity_index: index as u16,
                        pellets: 1,
                        occupied: true,
                        button,
                        ..DamageSlot::default()
                    };
                    trigger_slot_count += 1;
                }
                continue;
            }
            let Some(entity_index) = best_entity else {
                continue;
            };
            let impact = interpolate_segment(attack.start, end, best_fraction);
            if let Some(slot) = slots[..slot_count]
                .iter_mut()
                .find(|slot| slot.entity_index as usize == entity_index)
            {
                slot.pellets = slot.pellets.saturating_add(1);
                slot.impact = impact;
            } else {
                slots[slot_count] = DamageSlot {
                    entity_index: entity_index as u16,
                    pellets: 1,
                    occupied: true,
                    button: false,
                    impact,
                };
                slot_count += 1;
            }
        }

        let mut result = ShotgunResult::default();
        for slot in trigger_slots[..trigger_slot_count]
            .iter()
            .copied()
            .filter(|slot| slot.occupied)
        {
            let damage = i16::from(slot.pellets).saturating_mul(attack.damage_per_pellet);
            let mut applied = DamageResult::default();
            let hit = if slot.button {
                ShotBrush::Button(slot.entity_index as usize)
            } else {
                ShotBrush::Trigger(slot.entity_index as usize)
            };
            self.damage_shootable_brush(hit, damage, &mut applied);
            result.pellet_hits = result.pellet_hits.saturating_add(slot.pellets);
            result.damaged_targets = result
                .damaged_targets
                .saturating_add(applied.damaged_targets);
            result.total_damage = result.total_damage.saturating_add(applied.total_damage);
            result.killed_targets = result.killed_targets.saturating_add(applied.killed_targets);
            if applied.last_source_index.is_some() {
                result.last_source_index = applied.last_source_index;
            }
        }
        for slot in slots[..slot_count]
            .iter()
            .copied()
            .filter(|slot| slot.occupied)
        {
            let nightmare = self.nightmare();
            let entity = &mut self.entities[slot.entity_index as usize];
            let damage = i16::from(slot.pellets).saturating_mul(attack.damage_per_pellet);
            let mut applied = DamageResult::default();
            apply_entity_damage(
                map,
                nightmare,
                entity,
                damage,
                DamageAttacker::Player,
                &mut applied,
                &mut self.pending_scene_work,
            );
            result.pellet_hits = result.pellet_hits.saturating_add(slot.pellets);
            result.damaged_targets = result
                .damaged_targets
                .saturating_add(applied.damaged_targets);
            result.total_damage = result.total_damage.saturating_add(applied.total_damage);
            result.killed_targets = result.killed_targets.saturating_add(applied.killed_targets);
            if applied.last_source_index.is_some() {
                result.last_source_index = applied.last_source_index;
                result.last_impact = Some(slot.impact);
            }
            if applied.response_sound.is_some() {
                result.response_sound = applied.response_sound;
            }
        }
        Some(result)
    }

    /// Advance Quake trigger policy and canonical fixed-tick brush movers.
    /// Render bounds and transformed collision consume the same mover origin.
    ///
    /// `rider` is the player body every pusher may carry. It is lent mutably
    /// because `SV_PushMove` is a move, not a collision: a lift that rises
    /// under a standing player takes the player with it.
    pub fn update_gameplay(
        &mut self,
        map: &ResidentMap,
        rider: &mut Rider,
        use_pressed: bool,
        held_keys: u8,
        elapsed_ticks: u16,
    ) -> GameplayResult {
        let (mut player_mins, mut player_maxs) = (rider.mins, rider.maxs);
        // Quake's `groundentity`, resolved once before any pusher moves: the
        // flag `SV_PushMove` reads was set while the player's own move ran, so
        // it names the surface as it was at the start of this frame.
        let ground_entity = self.ground_brush_entity(map, rider);
        let ticks = elapsed_ticks.clamp(1, 4);
        let sources = map.entities();
        let mut result = GameplayResult::default();
        let mut actions = TargetActions::new();
        if let Err(error) = self.targets.tick(ticks, &sources, &mut actions) {
            result.target_error = Some(error);
        }
        self.apply_target_actions(map, &mut actions, &mut result);
        self.fire_monster_death_targets(map, &mut result);

        let mut fired_sources = [0u16; MAX_TRIGGERS];
        let mut fired_source_count = 0usize;
        for trigger in &mut self.triggers {
            trigger.cooldown = trigger.cooldown.saturating_sub(ticks);
            if !trigger.armed || !self.targets.is_enabled(trigger.source_index) {
                continue;
            }
            let source = sources
                .get(trigger.source_index as usize)
                .unwrap_or_default();
            // A shootable trigger has no touch function at all: it fires from
            // `multi_killed` and `multi_wait` hands its health back when the
            // authored wait runs out.
            if trigger.multi.shootable() {
                trigger.multi.heal_after_wait(trigger.cooldown == 0);
                if !trigger.multi.take_kill() {
                    continue;
                }
            } else if trigger.cooldown != 0
                || !aabb_overlaps(player_mins, player_maxs, trigger.mins, trigger.maxs)
            {
                continue;
            }
            if source.class_name == CLASS_TRIGGER_SETSKILL {
                if let Some(skill) = map.string_at(source.string).and_then(parse_setskill) {
                    // `cvar_set ("skill", ...)`: the entity loader reads the
                    // cvar, so the new skill takes effect on the next map.
                    self.skill = skill;
                    result.selected_skill = Some(skill);
                }
            } else if source.class_name == CLASS_TRIGGER_ONLY_REGISTERED {
                // Shareware is `registered 0`, so this volume never reaches
                // SUB_UseTargets and never removes itself. It only reports its
                // authored message, throttled to once every two seconds.
            } else if fired_source_count < fired_sources.len() {
                fired_sources[fired_source_count] = trigger.source_index;
                fired_source_count += 1;
            }
            if source.class_name == 0x50 {
                self.secrets.record_found();
                result.found_secret = true;
            }
            // `multi_trigger` centerprints the authored message and plays the
            // `sounds`-selected noise whether or not the trigger has a target.
            // trigger_secret carries a built-in message when the map authors
            // none, so it always reports a source.
            if source.string != 0 || source.class_name == 0x50 {
                result.message_source = Some(trigger.source_index);
            }
            if let Some(sound) = trigger_noise(source.class_name, source.noise) {
                result.message_sound = Some(SoundEvent::at(
                    sound,
                    midpoint_vec_all(trigger.mins, trigger.maxs),
                ));
            }
            if trigger.once {
                trigger.armed = false;
            } else {
                trigger.cooldown = trigger.wait_ticks.max(12);
            }
        }
        for source_index in fired_sources[..fired_source_count].iter().copied() {
            if let Err(error) = self.targets.fire_source_by(
                &sources,
                source_index,
                TargetActivator::Player,
                &mut actions,
            ) {
                result.target_error.get_or_insert(error);
            }
        }
        self.apply_target_actions(map, &mut actions, &mut result);

        let mut touched_teleport = None;
        for teleport in &mut self.teleports {
            teleport.cooldown = teleport.cooldown.saturating_sub(ticks);
            teleport.gate.tick(ticks);
            if teleport.gate.admits(teleport.spawn_flags, true)
                && teleport.cooldown == 0
                && self.targets.is_enabled(teleport.source_index)
                && aabb_overlaps(player_mins, player_maxs, teleport.mins, teleport.maxs)
            {
                teleport.cooldown = 2;
                touched_teleport = Some(teleport.source_index);
                break;
            }
        }
        if let Some(source_index) = touched_teleport {
            if let Err(error) = self.targets.fire_source_by(
                &sources,
                source_index,
                TargetActivator::Player,
                &mut actions,
            ) {
                result.target_error.get_or_insert(error);
            }
            self.apply_target_actions(map, &mut actions, &mut result);
            result.teleport =
                self.teleport_destination(map, source_index, player_mins, player_maxs);
        }

        let mut mover_outputs = [None; MAX_MOVERS];
        let mut mover_output_count = 0usize;
        for mover_index in 0..self.movers.len() {
            let render_index = self.movers[mover_index].render_index as usize;
            let Some(entity_snapshot) = self.entities.get(render_index).copied() else {
                continue;
            };
            if !self.targets.is_enabled(entity_snapshot.source_index) {
                continue;
            }
            // The authored fields this loop reads were copied at load, which
            // spares a 50-byte record decode per mover per tick.
            let source = self.movers[mover_index].source;
            // A lift uses `plat_spawn_inside_trigger`'s own volume, built from
            // the authored raised bounds so it never moves with the deck. Every
            // other automatic mover keeps the door's proximity field.
            //
            // Only a lift has a volume at all, and `plat_trigger_volume`
            // rejects everything else on its first line. Asking that question
            // before the model is fetched keeps the record decode off the
            // other 27 of E1M1's 29 movers: the raised bounds are a load
            // constant, and decoding them every frame to throw them away was
            // 96% of the frame's whole `BrushModel::decode` bill.
            let mut plat_mins = [0i32; 3];
            let mut plat_maxs = [0i32; 3];
            let plat_trigger = self.movers[mover_index].policy.is_plat()
                && map
                    .brush_models()
                    .get(entity_snapshot.model_index as usize)
                    .map(|model| {
                        self.movers[mover_index].policy.plat_trigger_volume(
                            [model.mins.x, model.mins.y, model.mins.z],
                            [model.maxs.x, model.maxs.y, model.maxs.z],
                            &mut plat_mins,
                            &mut plat_maxs,
                        )
                    })
                    .unwrap_or(false);
            let plat_touch = plat_trigger
                && aabb_overlaps(
                    player_mins,
                    player_maxs,
                    Vec3I32 {
                        x: plat_mins[0] << 12,
                        y: plat_mins[1] << 12,
                        z: plat_mins[2] << 12,
                    },
                    Vec3I32 {
                        x: plat_maxs[0] << 12,
                        y: plat_maxs[1] << 12,
                        z: plat_maxs[2] << 12,
                    },
                );
            let automatic_touch = self.movers[mover_index].policy.automatic()
                && if plat_trigger {
                    plat_touch
                } else {
                    expanded_overlap(
                        player_mins,
                        player_maxs,
                        entity_snapshot.clip_mins,
                        entity_snapshot.clip_maxs,
                        60,
                        8,
                    )
                };
            let shot_open = self.movers[mover_index].shot_open;
            self.movers[mover_index].shot_open = false;
            // `func_button`'s spawn is an either/or: a button with health has
            // no touch function at all, so it is shot open and walking into
            // it does nothing.
            let touchable = quake_core::mover::button_admits_touch(
                source.class_name,
                self.movers[mover_index].max_health,
            );
            let direct_touch = touchable
                && expanded_overlap(
                    player_mins,
                    player_maxs,
                    entity_snapshot.clip_mins,
                    entity_snapshot.clip_maxs,
                    2,
                    2,
                );
            let message_touch = source.class_name == 0x0c
                && source.target_name != 0
                && source.string != 0
                && expanded_overlap(
                    player_mins,
                    player_maxs,
                    entity_snapshot.clip_mins,
                    entity_snapshot.clip_maxs,
                    2,
                    2,
                );
            let used = use_pressed
                && expanded_overlap(
                    player_mins,
                    player_maxs,
                    entity_snapshot.clip_mins,
                    entity_snapshot.clip_maxs,
                    64,
                    16,
                );
            let directly_usable = quake_core::mover::mover_admits_use(
                source.class_name,
                self.movers[mover_index].max_health,
                source.target_name,
            );
            if message_touch
                || used
                    && source.class_name == 0x0c
                    && source.target_name != 0
                    && source.string != 0
            {
                result.message_source = Some(entity_snapshot.source_index);
                // `door_touch` answers an authored door message with talk.wav.
                result.message_sound = Some(SoundEvent::at(
                    0x7b,
                    bounds_center(entity_snapshot.clip_mins, entity_snapshot.clip_maxs),
                ));
            }

            // `door_touch`'s key half. Key doors never get a trigger field, so
            // walking into the door body is the only way to open one, and the
            // report is throttled by the original's two-second
            // `attack_finished`.
            self.movers[mover_index].key_cooldown =
                self.movers[mover_index].key_cooldown.saturating_sub(ticks);
            let mut key_opened = false;
            if source.class_name == 0x0c
                && !self.movers[mover_index].key_spent
                && self.movers[mover_index].key_cooldown == 0
                && expanded_overlap(
                    player_mins,
                    player_maxs,
                    entity_snapshot.clip_mins,
                    entity_snapshot.clip_maxs,
                    2,
                    2,
                )
            {
                match door::door_touch_key(source.spawn_flags, held_keys) {
                    door::DoorKeyOutcome::NotLocked => {}
                    door::DoorKeyOutcome::Opened { consumed_bit } => {
                        self.movers[mover_index].key_cooldown = door::DOOR_KEY_RETRY_TICKS;
                        result.consumed_key = Some(consumed_bit);
                        key_opened = true;
                    }
                    door::DoorKeyOutcome::Refused { needed_bit } => {
                        self.movers[mover_index].key_cooldown = door::DOOR_KEY_RETRY_TICKS;
                        result.needs_key = Some(needed_bit);
                    }
                }
            }

            let mover_state = self.movers[mover_index].policy.state();
            // `plat_center_touch` starts a lift only from its bottom.  The
            // trigger surrounds a rider through the whole shaft, so treating
            // `Down` like a door reversal makes every automatic lift bounce
            // upward after one descending tick.  Doors and buttons retain
            // their original reversal/re-arm path.
            let state_admits_activation =
                mover_state_admits_activation(source.class_name, mover_state);
            // Every `func_plat`, named or unnamed, owns the same fixed inside
            // trigger. Named platforms need it most: their target lowers them
            // and the player touch is the only event that sends them back up.
            let plat_activated = plat_touch && self.movers[mover_index].policy.plat_center_touch();
            if plat_activated {
                self.movers[mover_index].activator = TargetActivator::Player;
                result.record_player_activation(entity_snapshot.source_index);
            }
            if !plat_trigger
                && (automatic_touch || direct_touch || shot_open || used && directly_usable)
                && door::door_key_bit(source.spawn_flags) == 0
                && state_admits_activation
            {
                self.movers[mover_index].activator = TargetActivator::Player;
                self.movers[mover_index].policy.activate();
                result.record_player_activation(entity_snapshot.source_index);
                if matches!(source.class_name, 0x0c | 0x0d)
                    && mover_output_count < mover_outputs.len()
                {
                    mover_outputs[mover_output_count] =
                        Some((entity_snapshot.source_index, TargetActivator::Player));
                    mover_output_count += 1;
                }
            }
            if key_opened {
                // `door_fire` walks the whole linked chain, and `door_touch`
                // disarms the key check on this door and its partner.
                let group = self.movers[mover_index].link_group;
                self.disarm_door_message_group(mover_index);
                for linked in 0..self.movers.len() {
                    if self.movers[linked].link_group != group {
                        continue;
                    }
                    self.movers[linked].key_spent = true;
                    if !matches!(
                        self.movers[linked].policy.state(),
                        QuakeMoverState::Bottom | QuakeMoverState::Down
                    ) {
                        continue;
                    }
                    self.movers[linked].activator = TargetActivator::Player;
                    self.movers[linked].policy.activate();
                    let linked_source =
                        self.entities[self.movers[linked].render_index as usize].source_index;
                    result.record_player_activation(linked_source);
                    if mover_output_count < mover_outputs.len() {
                        mover_outputs[mover_output_count] =
                            Some((linked_source, TargetActivator::Player));
                        mover_output_count += 1;
                    }
                }
            }

            // `SV_PushMove` is one indivisible step: think, move, carry the
            // riders, and on a block put everything back. The whole policy is
            // snapshot because a blocked pusher never made its move at all.
            let restore_policy = self.movers[mover_index].policy;
            let state_before_tick = restore_policy.state();
            let mut sounds = [0i16; 8];
            let mut sound_count = 0usize;
            for sound in mover_sound_events(
                source.class_name,
                source.noise,
                mover_state,
                state_before_tick,
            )
            .iter()
            {
                sounds[sound_count] = sound;
                sound_count += 1;
            }
            let activation_sound_count = sound_count;
            let previous = entity_snapshot.origin;
            // `place_mover` rebuilds the transform, and the transform alone is
            // three divides through the shared interpolator. A mover that did
            // not move has nothing to rebuild, and on any given frame nearly
            // every mover in the level is parked at one of its stops, so having
            // `tick` report the advance keeps the parked ones free.
            let mut moved = false;
            let mut tick_sound_count = 0usize;
            for _ in 0..ticks {
                let tick = self.movers[mover_index].policy.tick_with_sound();
                moved |= tick.moved;
                if let Some(sound) = tick.sound.filter(|_| tick_sound_count < 4) {
                    sounds[sound_count] = quake_core::mover::secret_tick_sound(source.noise, sound);
                    sound_count += 1;
                    tick_sound_count += 1;
                }
            }
            if moved {
                self.place_mover(map, render_index, mover_index);
            }
            // `place_mover` is the only thing between `previous` and here that
            // writes this entity's origin, so a mover that did not move has a
            // zero delta by construction and `push_riders` would return `None`
            // off its first line. Saying so here spares the call frame instead
            // of paying it for every parked mover in the level.
            let blocker = if moved {
                self.push_riders(
                    map,
                    rider,
                    ground_entity,
                    render_index,
                    subtract_vec(self.entities[render_index].origin, previous),
                )
            } else {
                None
            };
            let blocked = blocker.is_some();
            player_mins = rider.mins;
            player_maxs = rider.maxs;
            if blocked {
                self.movers[mover_index].policy = restore_policy;
                self.place_mover(map, render_index, mover_index);
            }
            self.movers[mover_index].crush.tick(ticks);
            if blocked {
                // `plat_crush` and `door_blocked` both turn a mid-travel pusher
                // around; only the damage differs, so the class picks it.
                self.movers[mover_index].policy.crush_reverse();
                let damage = if source.class_name == CLASS_FUNC_PLAT {
                    PLAT_CRUSH_DAMAGE
                } else if source.damage == 0 {
                    door::DOOR_DEFAULT_DAMAGE.max(0) as u16
                } else {
                    source.damage.max(0) as u16
                };
                let dealt = self.movers[mover_index]
                    .crush
                    .crush(damage, door::DOOR_BLOCK_COOLDOWN_TICKS);
                self.apply_pusher_crush_damage(
                    map,
                    blocker.expect("blocked pusher has a blocker"),
                    dealt,
                    &mut result,
                );
            }
            let state_after_tick = self.movers[mover_index].policy.state();
            if blocked {
                sound_count = activation_sound_count;
            }
            for sound in mover_sound_events(
                source.class_name,
                source.noise,
                state_before_tick,
                state_after_tick,
            )
            .iter()
            {
                sounds[sound_count] = sound;
                sound_count += 1;
            }
            // The origin is only ever read by the loop below, and a silent
            // mover never enters it.
            if sound_count != 0 {
                let sound_origin =
                    bounds_center(entity_snapshot.clip_mins, entity_snapshot.clip_maxs);
                for &sound in &sounds[..sound_count] {
                    result.push_mover_sound(
                        SoundEvent::at(sound, sound_origin)
                            .on(entity_snapshot.source_index, crate::audio::CHAN_VOICE),
                    );
                }
            }
            if source.class_name == 0x0b
                && state_before_tick != QuakeMoverState::Top
                && state_after_tick == QuakeMoverState::Top
                && mover_output_count < mover_outputs.len()
            {
                mover_outputs[mover_output_count] = Some((
                    entity_snapshot.source_index,
                    self.movers[mover_index].activator,
                ));
                mover_output_count += 1;
            }
        }
        self.update_trains(map, rider, ground_entity, ticks, &mut result);
        for (source_index, activator) in mover_outputs[..mover_output_count]
            .iter()
            .flatten()
            .copied()
        {
            if let Err(error) =
                self.targets
                    .fire_source_by(&sources, source_index, activator, &mut actions)
            {
                result.target_error.get_or_insert(error);
            }
        }
        self.apply_target_actions(map, &mut actions, &mut result);
        result
    }

    /// Ride every authored `path_corner` chain, carry whatever stands on the
    /// train, and crush whatever it runs into.
    ///
    /// A `func_train` is `MOVETYPE_PUSH` exactly like a lift, so it goes
    /// through the same `SV_PushMove` rider carry. `train_blocked` damages the
    /// blocker and re-arms after half a second; unlike `plat_crush` it does not
    /// reverse, and this port has no pusher push-back, so a train that cannot
    /// clear its rider keeps going.
    fn update_trains(
        &mut self,
        map: &ResidentMap,
        rider: &mut Rider,
        ground_entity: Option<u16>,
        ticks: u16,
        result: &mut GameplayResult,
    ) {
        let sources = map.entities();
        for train_index in 0..self.trains.len() {
            let render_index = self.trains[train_index].render_index as usize;
            let Some(previous) = self.entities.get(render_index).map(|entity| entity.origin) else {
                continue;
            };
            let source_index = self.entities[render_index].source_index;
            if !self.targets.is_enabled(source_index) {
                continue;
            }
            let source = sources.get(source_index as usize).unwrap_or_default();
            // `SV_PushMove` is one indivisible step for a train exactly as it
            // is for a lift, so the whole policy is snapshot before it runs.
            let restore_policy = self.trains[train_index].policy;
            let arrivals_before = restore_policy.corner_arrivals();
            let state_before = restore_policy.state();
            for _ in 0..ticks {
                self.trains[train_index].policy.tick(&sources);
            }
            self.place_train(map, render_index, train_index);
            // The rider half of `SV_PushMove`.
            let blocker = self.push_riders(
                map,
                rider,
                ground_entity,
                render_index,
                subtract_vec(self.entities[render_index].origin, previous),
            );
            let blocked = blocker.is_some();
            if blocked {
                // The original rolls the pusher back with its riders, so the
                // blocked tick leaves nothing intersecting and nothing that
                // happened during it counts. `train_blocked` does not reverse,
                // so the train simply tries the same leg again next tick.
                self.trains[train_index].policy = restore_policy;
                self.place_train(map, render_index, train_index);
            } else {
                let state_after = self.trains[train_index].policy.state();
                let arrivals_after = self.trains[train_index].policy.corner_arrivals();
                // `sounds 1` selects plats/train1 at each leg start and
                // plats/train2 on arrival; `sounds 0` is the original's silent
                // misc/null pair.
                if source.noise == 1 {
                    let body = self.entities[render_index];
                    let center = bounds_center(body.clip_mins, body.clip_maxs);
                    if state_before != TrainState::Moving && state_after == TrainState::Moving {
                        result.train_sound = Some(SoundEvent::at(0x8a, center));
                    } else if arrivals_after != arrivals_before {
                        result.train_sound = Some(SoundEvent::at(0x8b, center));
                    }
                }
                result.train_arrivals = result
                    .train_arrivals
                    .saturating_add(arrivals_after.wrapping_sub(arrivals_before));
            }
            self.trains[train_index].crush.tick(ticks);
            if blocked {
                let damage = self.trains[train_index].policy.damage().max(0) as u16;
                let dealt = self.trains[train_index]
                    .crush
                    .crush(damage, TRAIN_BLOCK_COOLDOWN_TICKS);
                self.apply_pusher_crush_damage(
                    map,
                    blocker.expect("blocked train has a blocker"),
                    dealt,
                    result,
                );
            }
        }
    }

    /// Write one train's canonical origin into its render entity and rebuild
    /// the bounds and leaf every consumer reads.
    fn place_train(&mut self, map: &ResidentMap, render_index: usize, train_index: usize) {
        let origin = self.trains[train_index].policy.origin();
        let entity = &mut self.entities[render_index];
        if entity.origin == origin {
            return;
        }
        entity.origin = origin;
        let model = map
            .brush_models()
            .get(entity.model_index as usize)
            .expect("train model index validated at load");
        entity.clip_mins = translated_model_bounds(origin, model.mins);
        entity.clip_maxs = translated_model_bounds(origin, model.maxs);
        let center = bounds_center(entity.clip_mins, entity.clip_maxs);
        if let Some(leaf) = map.point_leaf_index(center) {
            entity.leaf_index = leaf.min(u16::MAX as usize) as u16;
        }
    }

    /// `monster_death_use`: a dead monster fires its own `target` once.
    ///
    /// This is the only way E1M7's exit doors ever open, since Chthon targets
    /// the `trigger_relay` that opens them. The scan is one pass over the
    /// live entity list and the flag makes it idempotent, so a death that
    /// arrives from a shotgun, a splash, a telefrag or the `event_lightning`
    /// shock all reach the same dispatch.
    fn fire_monster_death_targets(&mut self, map: &ResidentMap, result: &mut GameplayResult) {
        let sources = map.entities();
        let mut pending = [0u16; MAX_PLAYER_ACTIVATIONS];
        let mut count = 0usize;
        for entity in &self.entities {
            let Some(monster) = entity.monster else {
                continue;
            };
            if !monster.dead() || !self.targets.is_enabled(entity.source_index) {
                continue;
            }
            let source = sources
                .get(entity.source_index as usize)
                .unwrap_or_default();
            if source.target == 0 {
                continue;
            }
            if count < pending.len() {
                pending[count] = entity.source_index;
                count += 1;
            }
        }
        if count == 0 {
            return;
        }
        let mut actions = TargetActions::new();
        for source_index in pending[..count].iter().copied() {
            // The graph's own once-only bit is the bookkeeping: a monster
            // source is fired by nothing but its own death, so disabling it
            // makes `monster_death_use` idempotent without a per-entity flag.
            if let Err(error) = self.targets.fire_source_by(
                &sources,
                source_index,
                TargetActivator::Entity(source_index),
                &mut actions,
            ) {
                result.target_error = Some(error);
            }
            let _ = self.targets.disable_entity(source_index);
        }
        self.apply_target_actions(map, &mut actions, result);
    }

    #[optimize(size)]
    fn apply_target_actions(
        &mut self,
        map: &ResidentMap,
        actions: &mut TargetActions,
        result: &mut GameplayResult,
    ) {
        let sources = map.entities();
        loop {
            result.fired_target_edges = result
                .fired_target_edges
                .saturating_add(actions.fired_edges());
            result.completed_counters = result
                .completed_counters
                .saturating_add(actions.completed_counters());
            if actions.counter_message().is_some() {
                result.counter_message = actions.counter_message();
            }
            if actions.is_empty() {
                actions.clear();
                break;
            }
            let mut pending = [None; MAX_TARGET_ACTIONS];
            let mut pending_count = 0usize;
            for action in actions.iter() {
                pending[pending_count] = Some(action);
                pending_count += 1;
            }
            actions.clear();

            let mut mover_outputs = [None; MAX_TARGET_ACTIONS];
            let mut mover_output_count = 0usize;
            for action in pending[..pending_count].iter().flatten().copied() {
                match action {
                    TargetAction::Disable(source_index) => {
                        for entity in &mut self.entities {
                            if entity.source_index == source_index {
                                entity.visible = false;
                                entity.solid = false;
                                entity.damageable = false;
                            }
                        }
                    }
                    TargetAction::EnableTeleport(source_index) => {
                        if let Some(teleport) = self
                            .teleports
                            .iter_mut()
                            .find(|teleport| teleport.source_index == source_index)
                        {
                            teleport.gate.open();
                            teleport.cooldown = 0;
                        }
                    }
                    TargetAction::ToggleWall(source_index) => {
                        if let Some(entity) = self
                            .entities
                            .iter_mut()
                            .find(|entity| entity.source_index == source_index)
                        {
                            entity.visible = !entity.visible;
                            entity.solid = entity.visible;
                        }
                    }
                    TargetAction::AwakenMonster(source_index, _) => {
                        if self.awaken_boss(source_index) {
                            result.boss_awakened = true;
                        }
                    }
                    TargetAction::WakeMonster(source_index, activator) => {
                        // `monster_use`: only a visible player activator
                        // counts (`activator.items & IT_INVISIBILITY` returns).
                        if activator == TargetActivator::Player && !self.player_invisible {
                            if let Some(sound) = self.wake_monster(source_index) {
                                result.message_sound.get_or_insert(sound);
                            }
                        }
                    }
                    TargetAction::ShockBoss(_) => {
                        if let Some((sound, death_origin)) = self.shock_boss() {
                            result.boss_shock_sound = sound;
                            result.boss_shocks = result.boss_shocks.saturating_add(1);
                            result.boss_death_origin = result.boss_death_origin.or(death_origin);
                        }
                    }
                    TargetAction::FireShooter(source_index) => {
                        if let Some(sound) = self.fire_shooter(map, source_index) {
                            result.message_sound.get_or_insert(sound);
                        }
                    }
                    TargetAction::ToggleLight(source_index) => {
                        self.toggle_light(map, source_index);
                    }
                    TargetAction::Activate(source_index, activator) => {
                        let train_index = self.trains.iter().position(|train| {
                            self.entities
                                .get(train.render_index as usize)
                                .map(|entity| entity.source_index == source_index)
                                .unwrap_or(false)
                        });
                        if let Some(train_index) = train_index {
                            if self.trains[train_index].policy.activate(&sources)
                                && activator == TargetActivator::Player
                            {
                                result.record_player_activation(source_index);
                            }
                            continue;
                        }
                        let mover_index = self.movers.iter().position(|mover| {
                            self.entities
                                .get(mover.render_index as usize)
                                .map(|entity| entity.source_index == source_index)
                                .unwrap_or(false)
                        });
                        let Some(mover_index) = mover_index else {
                            continue;
                        };
                        let source = self.movers[mover_index].source;
                        // `door_use` and `door_fire` clear the touch-only
                        // message before deciding which way the linked door
                        // moves. E1M1's t15 shortcut is the stock case: once
                        // trigger_once #70 has fired it, touching the open
                        // brush must never repeat "opens elsewhere".
                        self.disarm_door_message_group(mover_index);
                        // `door_fire` never moves just the entity named by the
                        // target. It walks the complete `LinkDoors` chain.
                        // E1M2's silver-key pickup targets only source 204;
                        // source 205 is the touching partner, so activating
                        // the named mover alone leaves exactly one half shut.
                        let door_group = self.movers[mover_index].link_group;
                        for linked in 0..self.movers.len() {
                            if source.class_name == 0x0c {
                                if self.movers[linked].link_group != door_group {
                                    continue;
                                }
                            } else if linked != mover_index {
                                continue;
                            }
                            // `door_fire`: a DOOR_TOGGLE door fired while up
                            // goes back down; everything else only fires from
                            // rest.
                            let policy = &self.movers[linked].policy;
                            let toggling_down = policy.toggle()
                                && matches!(
                                    policy.state(),
                                    QuakeMoverState::Up | QuakeMoverState::Top
                                );
                            if !toggling_down
                                && !matches!(
                                    policy.state(),
                                    QuakeMoverState::Bottom | QuakeMoverState::Down
                                )
                            {
                                continue;
                            }
                            let linked_source = self.movers[linked].source;
                            let linked_source_index = self.entities
                                [self.movers[linked].render_index as usize]
                                .source_index;
                            let state_before = self.movers[linked].policy.state();
                            self.movers[linked].activator = activator;
                            self.movers[linked].policy.activate();
                            let state_after = self.movers[linked].policy.state();
                            let body = self.entities[self.movers[linked].render_index as usize];
                            for sound in mover_sound_events(
                                linked_source.class_name,
                                linked_source.noise,
                                state_before,
                                state_after,
                            )
                            .iter()
                            {
                                result.push_mover_sound(
                                    SoundEvent::at(
                                        sound,
                                        bounds_center(body.clip_mins, body.clip_maxs),
                                    )
                                    .on(linked_source_index, crate::audio::CHAN_VOICE),
                                );
                            }
                            if activator == TargetActivator::Player {
                                result.record_player_activation(linked_source_index);
                            }
                            if matches!(linked_source.class_name, 0x0c | 0x0d)
                                && mover_output_count < mover_outputs.len()
                            {
                                mover_outputs[mover_output_count] =
                                    Some((linked_source_index, activator));
                                mover_output_count += 1;
                            }
                        }
                    }
                }
            }
            for (source_index, activator) in mover_outputs[..mover_output_count]
                .iter()
                .flatten()
                .copied()
            {
                if let Err(error) =
                    self.targets
                        .fire_source_by(&sources, source_index, activator, actions)
                {
                    result.target_error.get_or_insert(error);
                }
            }
        }
    }

    /// Resolve one touched `trigger_teleport` and run `spawn_tdeath` at the
    /// arrival point. The player's own cooked box is reused so the telefrag
    /// volume matches the original `death_owner.mins/maxs` grown by one unit.
    fn teleport_destination(
        &mut self,
        map: &ResidentMap,
        source_index: u16,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
    ) -> Option<TeleportDestination> {
        let sources = map.entities();
        let source = sources.get(source_index as usize)?;
        let target = teleport::resolve_destination(&sources, source, |index| {
            self.targets.is_enabled(index)
        })?;
        let player_origin = midpoint_vec_all(player_mins, player_maxs);
        let relative_mins = subtract_vec(player_mins, player_origin);
        let relative_maxs = subtract_vec(player_maxs, player_origin);
        let (frag_mins, frag_maxs) =
            teleport::telefrag_bounds(target.origin, relative_mins, relative_maxs);
        let mut telefragged = 0u8;
        for index in 0..self.entities.len() {
            let entity = &self.entities[index];
            if !entity.visible
                || !entity.damageable
                || entity.health <= 0
                || entity.monster.is_none()
                || !aabb_overlaps(frag_mins, frag_maxs, entity.hit_mins, entity.hit_maxs)
            {
                continue;
            }
            let mut damage = DamageResult::default();
            apply_entity_damage(
                map,
                self.nightmare(),
                &mut self.entities[index],
                teleport::TELEFRAG_DAMAGE,
                DamageAttacker::World,
                &mut damage,
                &mut self.pending_scene_work,
            );
            telefragged = telefragged.saturating_add(damage.killed_targets);
        }
        Some(TeleportDestination {
            source_index,
            destination_index: target.destination_index,
            origin: target.origin,
            angles: target.angles,
            exit_velocity: target.exit_velocity,
            telefragged,
            silent: source.spawn_flags & teleport::SPAWNFLAG_TELEPORT_SILENT != 0,
        })
    }

    /// Run `teleport_touch` for a monster body. Episode 1 authors its sealed
    /// monster closets with ordinary targetnamed `trigger_teleport` volumes;
    /// they use the same gate and `spawn_tdeath` rules as player teleporters.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn teleport_monster_if_touching(
        &mut self,
        map: &ResidentMap,
        index: usize,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        weapon: &mut WeaponState,
        styles: &[u16; lightstyle::DUMMY_STYLE + 1],
        result: &mut MonsterFrameResult,
    ) -> Option<bool> {
        let snapshot = self.entities[index];
        let runtime = snapshot.monster?;
        if !snapshot.visible || snapshot.health <= 0 || runtime.dead() || runtime.crucified() {
            return Some(false);
        }
        let source_index = self
            .teleports
            .iter()
            .find(|source| {
                source.gate.admits(source.spawn_flags, false)
                    && self.targets.is_enabled(source.source_index)
                    && aabb_overlaps(
                        snapshot.hit_mins,
                        snapshot.hit_maxs,
                        source.mins,
                        source.maxs,
                    )
            })
            .map(|source| source.source_index);
        let Some(source_index) = source_index else {
            return Some(false);
        };
        let sources = map.entities();
        let Some(source) = sources.get(source_index as usize) else {
            return Some(false);
        };
        let Some(target) = teleport::resolve_destination(&sources, source, |target_index| {
            self.targets.is_enabled(target_index)
        }) else {
            return Some(false);
        };
        let relative_mins = subtract_vec(snapshot.hit_mins, snapshot.origin);
        let relative_maxs = subtract_vec(snapshot.hit_maxs, snapshot.origin);
        let (frag_mins, frag_maxs) =
            teleport::telefrag_bounds(target.origin, relative_mins, relative_maxs);
        let nightmare = self.nightmare();
        for victim_index in 0..self.entities.len() {
            let victim = self.entities[victim_index];
            if victim_index == index
                || !victim.visible
                || !victim.damageable
                || victim.health <= 0
                || victim.monster.is_none()
                || !aabb_overlaps(frag_mins, frag_maxs, victim.hit_mins, victim.hit_maxs)
            {
                continue;
            }
            let mut damage = DamageResult::default();
            apply_entity_damage(
                map,
                nightmare,
                &mut self.entities[victim_index],
                teleport::TELEFRAG_DAMAGE,
                DamageAttacker::World,
                &mut damage,
                &mut self.pending_scene_work,
            );
            if let Some(sound) = damage.response_sound {
                result.push_sound(sound);
            }
        }
        if weapon.inventory().health() > 0
            && aabb_overlaps(frag_mins, frag_maxs, player_mins, player_maxs)
        {
            let protected = weapon
                .inventory()
                .powerups()
                .active(quake_core::survival::PowerupKind::Pentagram);
            let before = weapon.inventory().health();
            let taken = weapon.take_damage(teleport::TELEFRAG_DAMAGE);
            if taken > 0 {
                result.player_damage = result.player_damage.saturating_add(taken as u16);
                result.push_sound(SoundEvent::player_voice(0xa0));
            }
            if before > 0 && weapon.inventory().health() <= 0 {
                result.player_killed = true;
                result.push_sound(SoundEvent::player_voice(0x8e));
            }
            // `teledeath_touch`: an invulnerable victim reflects the telefrag
            // into the arriving entity after armor has still been consumed.
            if protected {
                let mut reflected = DamageResult::default();
                apply_entity_damage(
                    map,
                    nightmare,
                    &mut self.entities[index],
                    teleport::TELEFRAG_DAMAGE,
                    DamageAttacker::World,
                    &mut reflected,
                    &mut self.pending_scene_work,
                );
                if let Some(sound) = reflected.response_sound {
                    result.push_sound(sound);
                }
            }
        }
        self.entities[index].angles = target.angles;
        if !update_moving_alias_origin(map, &mut self.entities[index], target.origin, styles) {
            return None;
        }
        let fog = teleport::destination_fog_origin(target);
        result.push_teleport_fog(fog);
        if source.spawn_flags & teleport::SPAWNFLAG_TELEPORT_SILENT == 0 {
            let hash = self
                .frame
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(u32::from(source_index));
            result.push_sound(SoundEvent::at(0x74 + ((hash >> 16) % 5) as i16, fog));
        }
        Some(true)
    }

    /// Launch a grounded `MOVETYPE_STEP` body from an authored
    /// `trigger_monsterjump`. The trigger source rides in the dormant patrol
    /// slot until landing, so the flight adds no entity or projectile pool.
    #[optimize(size)]
    fn monster_jump_if_touching(
        &mut self,
        map: &ResidentMap,
        index: usize,
        runtime: &mut MonsterRuntime,
    ) -> bool {
        if map.map() != EpisodeMap::E1M6
            || runtime.leaping()
            || runtime.dead()
            || runtime.crucified()
        {
            return false;
        }
        let source = map
            .entities()
            .get(E1M6_MONSTERJUMP_SOURCE)
            .unwrap_or_default();
        let Some((mins, maxs)) = entity_brush_bounds(map, source) else {
            return false;
        };
        if source.class_name == CLASS_TRIGGER_MONSTERJUMP
            && self.targets.is_enabled(E1M6_MONSTERJUMP_SOURCE as u16)
            && aabb_overlaps(
                self.entities[index].hit_mins,
                self.entities[index].hit_maxs,
                mins,
                maxs,
            )
        {
            runtime.begin_forced_jump();
            self.entities[index].patrol |= FORCED_JUMP_SOURCE;
            return true;
        }
        false
    }

    /// Advance every authored monster at Quake's ten Hz think rate. World and
    /// translated brush collision use the same current hulls as the player and
    /// weapon traces, and live bodies block on both sides.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    pub fn update_monsters(
        &mut self,
        map: &ResidentMap,
        player_origin: Vec3I32,
        player_velocity: Vec3I32,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        player_hostile: bool,
        weapon: &mut WeaponState,
        elapsed_ticks: u16,
    ) -> Option<MonsterFrameResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let has_teleports = !self.teleports.is_empty();
        let mut result = MonsterFrameResult::default();
        self.sight_alert_ticks = self.sight_alert_ticks.saturating_sub(elapsed_ticks);
        self.player_invisible = weapon
            .inventory()
            .powerups()
            .active(quake_core::survival::PowerupKind::Ring);
        let mut index = 0usize;
        while index < self.entities.len() {
            let Some(mut runtime) = self.entities[index].monster else {
                index += 1;
                continue;
            };
            // Walking monsters can enter a trigger only on their 10 Hz think;
            // closet occupants are already inside and the 12-tick authored
            // gate remains open through the next due think at every phase.
            // Leapers move every frame, so retain a per-frame touch check while
            // they are airborne.
            if has_teleports
                && !runtime.dead()
                && !runtime.crucified()
                && (runtime.leaping() || runtime.think_due(elapsed_ticks))
                && self.teleport_monster_if_touching(
                    map,
                    index,
                    player_mins,
                    player_maxs,
                    weapon,
                    &styles,
                    &mut result,
                )?
            {
                runtime = self.entities[index].monster?;
            }
            // Crucified zombies are authored decoration: they never acquire,
            // never move, and never attack, so none of the per-frame geometry
            // below applies to them.
            if runtime.crucified() {
                let action = runtime.advance_ticks(elapsed_ticks, MonsterThinkInput::default());
                self.entities[index].monster = Some(runtime);
                if let Some(action) = action {
                    self.entities[index].frame = action.frame;
                    if let Some(sound) = action.sound_id {
                        let origin = self.entities[index].origin;
                        let distance = distance_units(origin, player_origin);
                        if distance <= quake_core::monster::MONSTER_IDLE_VOICE_RANGE {
                            result.push_sound(
                                SoundEvent::idle_at(sound, origin).on(
                                    self.entities[index].source_index,
                                    crate::audio::CHAN_VOICE,
                                ),
                            );
                        }
                    }
                }
                index += 1;
                continue;
            }
            // `dog_leap1` and `demon1_jump1` hand the body to the physics: it
            // flies every frame, ahead of a think that still only lands at
            // 10 Hz, so the arc is smooth and the animation is unchanged.
            if runtime.leaping() {
                let forced = self.entities[index].patrol & FORCED_JUMP_SOURCE != 0;
                self.fly_leap(
                    map,
                    index,
                    &mut runtime,
                    player_mins,
                    player_maxs,
                    weapon,
                    elapsed_ticks,
                    &styles,
                    &mut result,
                )?;
                self.entities[index].monster = Some(runtime);
                if forced {
                    index += 1;
                    continue;
                }
            }
            let snapshot = self.entities[index];
            let alive = weapon.inventory().health() > 0;
            // The enemy is the player unless infighting substituted another
            // monster; `ai_run` drops a dead enemy for the old one (the
            // player) at once.
            let mut enemy = EnemyTarget {
                origin: player_origin,
                velocity: player_velocity,
                mins: player_mins,
                maxs: player_maxs,
                alive,
                view_height: 22,
                index: None,
            };
            if let MonsterEnemy::Monster(enemy_index) = runtime.enemy() {
                if let Some(target) = self.infight_target(enemy_index) {
                    enemy = target;
                } else {
                    runtime.set_enemy(MonsterEnemy::Player);
                }
            }
            let monster_eye = Vec3I32 {
                x: snapshot.origin.x,
                y: snapshot.origin.y,
                z: snapshot
                    .origin
                    .z
                    .saturating_add(runtime.kind().view_height() << 12),
            };
            let enemy_eye = Vec3I32 {
                x: enemy.origin.x,
                y: enemy.origin.y,
                z: enemy.origin.z.saturating_add(enemy.view_height << 12),
            };
            let distance = distance_units(snapshot.origin, enemy.origin);
            let player_distance = if enemy.index.is_some() {
                distance_units(snapshot.origin, player_origin)
            } else {
                distance
            };
            let mut visible = false;
            // Crucified zombies are decoration and Chthon is woken by his own
            // trigger, so neither needs a sight trace. Skipping them keeps a
            // map full of authored decoration off the per-frame trace budget.
            let needs_sight =
                !runtime.crucified() && !(runtime.kind().is_boss() && !runtime.active());
            if enemy.alive
                && needs_sight
                && !runtime.dead()
                && distance < quake_core::monster::MONSTER_FAR_RANGE
            {
                let mut scratch = TraceScratch::default();
                let mut trace = Trace::default();
                if !self.trace_point(map, &monster_eye, &enemy_eye, &mut scratch, &mut trace) {
                    return None;
                }
                visible = !trace.start_solid && !trace.all_solid && trace.fraction == 1 << 12;
            }
            // `sight_entity`: a monster that just found the player wakes the
            // resting monsters that can see it, inside RANGE_NEAR outright and
            // inside RANGE_MID when they face it. Only checked on the frame a
            // think is due, so the window costs one trace per neighbour.
            let mut pack_alert = false;
            if self.sight_alert_ticks != 0
                && !runtime.active()
                && !runtime.dead()
                && needs_sight
                && alive
                && runtime.think_due(elapsed_ticks)
                && usize::from(self.sight_index) != index
            {
                pack_alert = self.pack_alerted(map, &snapshot, monster_eye)?;
            }
            // The corner is only needed on a think, and only by a monster at
            // rest, so the cooked record is decoded on those frames alone.
            let patrol =
                if snapshot.patrol != 0 && !runtime.active() && runtime.think_due(elapsed_ticks) {
                    map.entities().get(usize::from(snapshot.patrol))
                } else {
                    None
                };
            let input = MonsterThinkInput {
                distance,
                visible,
                in_front: target_in_front(snapshot.origin, snapshot.angles.y, enemy.origin),
                player_hostile,
                player_alive: enemy.alive,
                player_invisible: self.player_invisible,
                leap_height_ok: dog_leap_height_ok(
                    snapshot.hit_mins,
                    snapshot.hit_maxs,
                    enemy.mins,
                    enemy.maxs,
                ),
                goal: patrol.map(|corner| corner.origin),
                pack_alert,
                nightmare: self.nightmare(),
            };
            let action = runtime.advance_ticks(elapsed_ticks, input);
            self.entities[index].monster = Some(runtime);
            let Some(action) = action else {
                index += 1;
                continue;
            };

            if action.activated {
                result.activated = result.activated.saturating_add(1);
                // `FoundTarget`: let other monsters see this one for a while.
                self.sight_alert_ticks = SIGHT_ALERT_TICKS;
                self.sight_index = index as u16;
            }
            // Quake clips a voice at its attenuation range, so a distant idle
            // monster is not audible. Without this every crucified zombie in
            // Start would chatter at full volume forever.
            if let Some(sound) = action.sound_id {
                let range = if action.sound_idle {
                    quake_core::monster::MONSTER_IDLE_VOICE_RANGE
                } else {
                    quake_core::monster::MONSTER_VOICE_RANGE
                };
                if player_distance <= range {
                    result.push_sound(
                        if action.sound_idle {
                            SoundEvent::idle_at(sound, snapshot.origin)
                        } else {
                            SoundEvent::at(sound, snapshot.origin)
                        }
                        .on(snapshot.source_index, crate::audio::CHAN_VOICE),
                    );
                }
            }

            if let Some(ammo) = action.drop_backpack {
                self.drop_backpack(map, snapshot.origin, ammo);
            }

            let goal = patrol.map(|corner| corner.origin);
            let mut yaw = snapshot.angles.y;
            let facing = if action.face_target {
                Some(enemy.origin)
            } else if action.face_goal {
                goal
            } else {
                None
            };
            if let Some(target) = facing {
                let target_yaw = atan2_q12(
                    (target.y.saturating_sub(snapshot.origin.y)) >> 12,
                    (target.x.saturating_sub(snapshot.origin.x)) >> 12,
                ) as i16;
                yaw = turn_toward_yaw(yaw, target_yaw, runtime.kind().yaw_speed());
            }
            let mut next_origin = snapshot.origin;
            if action.move_units != 0 {
                let bodies = self.monster_step_bodies(
                    index,
                    i32::from(action.move_units.saturating_abs()),
                    player_mins,
                    player_maxs,
                    alive,
                );
                let hull = runtime.kind().collision_hull();
                next_origin = if runtime.kind().flies() {
                    self.monster_fly_step(
                        map,
                        snapshot.origin,
                        if action.face_goal {
                            goal.unwrap_or(enemy.origin)
                        } else {
                            enemy.origin
                        },
                        yaw,
                        action.move_units,
                        hull,
                        &bodies,
                    )?
                } else {
                    self.monster_step(map, snapshot.origin, yaw, action.move_units, hull, &bodies)?
                };
            }
            {
                let entity = &mut self.entities[index];
                entity.angles.y = yaw;
                entity.frame = action.frame;
                if next_origin != entity.origin {
                    if !update_moving_alias_origin(map, entity, next_origin, &styles) {
                        return None;
                    }
                    result.moved = result.moved.saturating_add(1);
                }
            }
            // `t_movetarget`: a walking monster that touches its corner takes
            // the corner's own target as the next goal.
            if action.face_goal {
                if let Some(corner) = patrol {
                    if path_corner_reached(next_origin, corner.origin) {
                        self.arrive_at_corner(map, index, corner)?;
                    }
                }
            }

            if let Some(attack) = action.attack {
                result.attacks = result.attacks.saturating_add(1);
                let damage = match attack {
                    MonsterAttack::SoldierShot { spread } => self.soldier_attack_damage(
                        map,
                        index,
                        enemy.origin,
                        enemy.velocity,
                        enemy.mins,
                        enemy.maxs,
                        spread,
                    )?,
                    MonsterAttack::Contact { damage, reach } => {
                        let origin = self.entities[index].origin;
                        if distance_units(origin, enemy.origin) <= reach && visible {
                            damage
                        } else {
                            0
                        }
                    }
                    MonsterAttack::Leap { .. } => {
                        // `self.origin_z = self.origin_z + 1`: the box leaves
                        // the floor it was resting on before the arc starts.
                        // The blow itself is `fly_leap`'s, on contact.
                        let entity = &mut self.entities[index];
                        let lifted = Vec3I32 {
                            z: entity.origin.z.saturating_add(1 << 12),
                            ..entity.origin
                        };
                        if !update_moving_alias_origin(map, entity, lifted, &styles) {
                            return None;
                        }
                        0
                    }
                    MonsterAttack::Lightning { damage } => self.monster_lightning(
                        map,
                        index,
                        enemy.origin,
                        enemy.mins,
                        enemy.maxs,
                        damage,
                    )?,
                    MonsterAttack::Grenade { .. }
                    | MonsterAttack::ZombieGib { .. }
                    | MonsterAttack::Spit { .. }
                    | MonsterAttack::LavaBall { .. } => {
                        self.launch_monster_missile(map, index, enemy.origin, attack);
                        0
                    }
                };
                if damage > 0 {
                    if let Some(target_index) = enemy.index {
                        self.infight_damage(
                            map,
                            index,
                            target_index,
                            damage,
                            player_distance,
                            &mut result,
                        );
                        index += 1;
                        continue;
                    }
                    let before = weapon.inventory().health();
                    let taken = weapon.take_damage(damage);
                    // Melee, bullets and the shambler's bolt all pass the
                    // monster itself as `T_Damage`'s inflictor, so the push
                    // comes from the middle of its body box.
                    let inflictor = midpoint_vec_all(
                        self.entities[index].hit_mins,
                        self.entities[index].hit_maxs,
                    );
                    result.player_impulse = add_vec(
                        result.player_impulse,
                        knockback_impulse(player_origin, inflictor, damage),
                    );
                    if taken > 0 {
                        result.player_damage = result.player_damage.saturating_add(taken as u16);
                        result.push_sound(SoundEvent::player_voice(match self.frame % 3 {
                            0 => 0xa0,
                            1 => 0xa2,
                            _ => 0xa5,
                        }));
                    }
                    if before > 0 && weapon.inventory().health() <= 0 {
                        result.player_killed = true;
                        result.push_sound(SoundEvent::player_voice(0x8e));
                    }
                }
            }
            if self.monster_jump_if_touching(map, index, &mut runtime) {
                self.entities[index].monster = Some(runtime);
            }
            index += 1;
        }
        Some(result)
    }

    /// The enemy monster an infighting monster is hunting, if it is still a
    /// live body; `None` sends the hunter back to the player.
    #[cold]
    #[inline(never)]
    fn infight_target(&self, enemy_index: u16) -> Option<EnemyTarget> {
        let target = self.entities.get(usize::from(enemy_index))?;
        let monster = target.monster?;
        if !target.visible || !target.damageable || monster.dead() {
            return None;
        }
        Some(EnemyTarget {
            origin: target.origin,
            velocity: Vec3I32::default(),
            mins: target.hit_mins,
            maxs: target.hit_maxs,
            alive: true,
            view_height: monster.kind().view_height(),
            index: Some(usize::from(enemy_index)),
        })
    }

    /// `FindTarget`'s `sight_entity` branch for one resting monster: the
    /// sighting monster is inside RANGE_FAR, inside RANGE_NEAR or in front,
    /// and visible from here. `None` is a failed trace.
    #[cold]
    #[inline(never)]
    fn pack_alerted(
        &self,
        map: &ResidentMap,
        snapshot: &RenderEntity,
        monster_eye: Vec3I32,
    ) -> Option<bool> {
        let Some(sighter) = self.entities.get(usize::from(self.sight_index)) else {
            return Some(false);
        };
        let sighter_distance = distance_units(snapshot.origin, sighter.origin);
        if sighter_distance >= quake_core::monster::MONSTER_FAR_RANGE
            || (sighter_distance >= quake_core::monster::MONSTER_NEAR_RANGE
                && !target_in_front(snapshot.origin, snapshot.angles.y, sighter.origin))
        {
            return Some(false);
        }
        let sighter_eye = Vec3I32 {
            x: sighter.origin.x,
            y: sighter.origin.y,
            z: sighter.origin.z.saturating_add(25 << 12),
        };
        let mut scratch = TraceScratch::default();
        let mut trace = Trace::default();
        if !self.trace_point(map, &monster_eye, &sighter_eye, &mut scratch, &mut trace) {
            return None;
        }
        Some(!trace.start_solid && !trace.all_solid && trace.fraction == 1 << 12)
    }

    /// `t_movetarget`: park for the corner's `wait` and take the corner's own
    /// target as the next goal, or none when the chain ends.
    #[cold]
    #[inline(never)]
    fn arrive_at_corner(
        &mut self,
        map: &ResidentMap,
        index: usize,
        corner: MapEntity,
    ) -> Option<()> {
        let mut runtime = self.entities[index].monster?;
        runtime.arrive_at_goal(path_corner_wait_ticks(corner.wait));
        self.entities[index].monster = Some(runtime);
        let mut next = PathCorner::EMPTY;
        self.entities[index].patrol =
            if find_path_corner_into(&map.entities(), corner.target, &mut next) {
                next.source_index
            } else {
                0
            };
        Some(())
    }

    /// Infighting: the blow lands on the enemy monster, and `T_Damage` lets
    /// it turn on the attacker in return.
    #[cold]
    #[inline(never)]
    fn infight_damage(
        &mut self,
        map: &ResidentMap,
        attacker_index: usize,
        target_index: usize,
        damage: i16,
        player_distance: i32,
        result: &mut MonsterFrameResult,
    ) {
        let attacker = self.missile_attacker(attacker_index as u16);
        let mut applied = DamageResult::default();
        apply_entity_damage(
            map,
            self.nightmare(),
            &mut self.entities[target_index],
            damage,
            attacker,
            &mut applied,
            &mut self.pending_scene_work,
        );
        if let Some(sound) = applied.response_sound {
            if player_distance <= quake_core::monster::MONSTER_VOICE_RANGE {
                result.push_sound(sound);
            }
        }
    }

    /// Advance one monster by `distance` units, trying the authored heading
    /// first and then a bounded fan of alternates.
    ///
    /// Bodies are cleared with a horizontal sweep of the mover's own origin
    /// before the world floor probe runs. The floor probe is a short vertical
    /// segment at the destination column, so testing bodies there would report
    /// a floor-facing normal and let monsters climb onto each other's heads;
    /// the horizontal sweep is the compose site with Quake's semantics, and a
    /// blocked direction is simply rejected in favour of the next alternate.
    fn monster_step(
        &self,
        map: &ResidentMap,
        origin: Vec3I32,
        yaw: i16,
        distance: i16,
        hull_index: usize,
        bodies: &BodyBlockers,
    ) -> Option<Vec3I32> {
        const DIRECTIONS: [i16; 6] = [0, 512, -512, 1_024, -1_024, 2_048];
        for offset in DIRECTIONS {
            let heading = yaw.wrapping_add(offset) as u16 & 0x0fff;
            let move_x = cos_q12(heading).saturating_mul(i32::from(distance));
            let move_y = sin_q12(heading).saturating_mul(i32::from(distance));
            let wished = Vec3I32 {
                x: origin.x.saturating_add(move_x),
                y: origin.y.saturating_add(move_y),
                z: origin.z,
            };
            if bodies.resolve(origin, wished, hull_index).is_some() {
                continue;
            }
            let start = Vec3I32 {
                z: wished.z.saturating_add(18 << 12),
                ..wished
            };
            let end = Vec3I32 {
                z: wished.z.saturating_sub(18 << 12),
                ..wished
            };
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_hull(map, hull_index, &start, &end, &mut scratch, &mut trace) {
                return None;
            }
            if !trace.start_solid
                && !trace.all_solid
                && trace.fraction < 1 << 12
                && trace.normal.z >= 2_896
            {
                return Some(trace.end);
            }
        }
        Some(origin)
    }

    /// Collect the live bodies a moving monster can hit this think: the player
    /// while alive, plus every other live monster within a conservative radius
    /// of the step. Candidates are visited in ascending authored source index
    /// and the set denies on full (see `quake_core::body`).
    fn monster_step_bodies(
        &self,
        mover_index: usize,
        step_units: i32,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        player_alive: bool,
    ) -> BodyBlockers {
        // Largest Quake body plus the largest clip hull, so no candidate that
        // could touch the swept hull is discarded by the broad phase.
        const BODY_BROAD_PHASE_UNITS: i32 = 128;
        let mut bodies = BodyBlockers::new();
        let origin = self.entities[mover_index].origin;
        let reach = step_units.saturating_add(BODY_BROAD_PHASE_UNITS);
        if player_alive {
            bodies.push(Body {
                source_index: PLAYER_BODY_SOURCE,
                mins: player_mins,
                maxs: player_maxs,
                dead: false,
            });
        }
        for (index, entity) in self.entities.iter().enumerate() {
            if index == mover_index {
                continue;
            }
            let Some(body) = dynamic_body(entity) else {
                continue;
            };
            if body.dead || distance_units(origin, entity.origin) > reach {
                continue;
            }
            bodies.push(body);
        }
        bodies
    }

    fn trace_hull(
        &self,
        map: &ResidentMap,
        hull_index: usize,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let Some(world) = map.brush_models().get(0).and_then(|model| {
            model.head_nodes.get(hull_index).and_then(|&head| {
                Some(unsafe {
                    CollisionHull::from_native_clip_nodes(
                        map.collision_planes(),
                        map.collision_clip_nodes(),
                        head,
                    )
                })
            })
        }) else {
            return false;
        };
        let mut best = Trace::default();
        if !world.trace_into(start, end, scratch, &mut best) {
            return false;
        }
        // Broad phase, matching the one `SceneCollision::trace` already runs.
        // Without it every monster step traced all 29 solid submodels of E1M1
        // through their full hulls no matter where the monster stood, and a
        // blocked monster fans over six directions.
        let swept = SweptUnitBox::new(*start, *end);
        for entity in &self.entities {
            if !entity.visible || !entity.solid || entity.model_id >= 0 {
                continue;
            }
            if !swept.overlaps_within(
                entity.clip_mins,
                entity.clip_maxs,
                quake_core::body::HULL_BROAD_PHASE_MARGIN_UNITS,
            ) {
                continue;
            }
            let Some(model) = map.brush_models().get(entity.model_index as usize) else {
                return false;
            };
            let Some(&head_node) = model.head_nodes.get(hull_index) else {
                return false;
            };
            let Some(hull) = Some(unsafe {
                CollisionHull::from_native_clip_nodes(
                    map.collision_planes(),
                    map.collision_clip_nodes(),
                    head_node,
                )
            }) else {
                return false;
            };
            let mut candidate = Trace::default();
            if !trace_translated_hull(hull, entity.origin, start, end, scratch, &mut candidate) {
                return false;
            }
            if candidate.fraction < best.fraction
                || (candidate.start_solid && !best.start_solid)
                || (candidate.all_solid && !best.all_solid)
            {
                best = candidate;
            }
        }
        *output = best;
        true
    }

    /// Flying monsters do not step to a floor. Quake clips a box move and
    /// trims height toward the enemy instead, so this sweeps the hull straight
    /// at the wished position through world, movers, and live bodies.
    #[allow(clippy::too_many_arguments)]
    fn monster_fly_step(
        &self,
        map: &ResidentMap,
        origin: Vec3I32,
        target: Vec3I32,
        yaw: i16,
        distance: i16,
        hull_index: usize,
        bodies: &BodyBlockers,
    ) -> Option<Vec3I32> {
        const DIRECTIONS: [i16; 6] = [0, 512, -512, 1_024, -1_024, 2_048];
        const HEIGHT_STEP_Q12: i32 = 8 << 12;
        let above = origin.z.saturating_sub(target.z);
        let climb = if above > (40 << 12) {
            -HEIGHT_STEP_Q12
        } else if above < (30 << 12) {
            HEIGHT_STEP_Q12
        } else {
            0
        };
        for offset in DIRECTIONS {
            let heading = yaw.wrapping_add(offset) as u16 & 0x0fff;
            let wished = Vec3I32 {
                x: origin
                    .x
                    .saturating_add(cos_q12(heading).saturating_mul(i32::from(distance))),
                y: origin
                    .y
                    .saturating_add(sin_q12(heading).saturating_mul(i32::from(distance))),
                z: origin.z.saturating_add(climb),
            };
            if bodies.resolve(origin, wished, hull_index).is_some() {
                continue;
            }
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_hull(map, hull_index, &origin, &wished, &mut scratch, &mut trace) {
                return None;
            }
            if !trace.start_solid && !trace.all_solid && trace.fraction == 1 << 12 {
                return Some(wished);
            }
        }
        Some(origin)
    }

    /// One frame of `SV_Physics_Toss` for a dog or fiend in mid-leap. Gravity
    /// lives in the runtime, so the body only has to sweep: horizontally
    /// first and then vertically, which keeps a wall from pinning the monster
    /// in the air. The first live body the arc reaches takes `Dog_JumpTouch`'s
    /// blow, and ground under the box ends the leap.
    #[allow(clippy::too_many_arguments)]
    fn fly_leap(
        &mut self,
        map: &ResidentMap,
        index: usize,
        runtime: &mut MonsterRuntime,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        weapon: &mut WeaponState,
        elapsed_ticks: u16,
        styles: &[u16; lightstyle::DUMMY_STYLE + 1],
        result: &mut MonsterFrameResult,
    ) -> Option<()> {
        let forced = runtime.forced_jump();
        let Some(leap) = runtime.advance_leap(elapsed_ticks) else {
            if forced {
                self.entities[index].patrol &= !FORCED_JUMP_SOURCE;
            }
            return Some(());
        };
        let hull = runtime.kind().collision_hull();
        let origin = self.entities[index].origin;
        let yaw = if forced {
            0
        } else {
            self.entities[index].angles.y
        };
        let step = leap_step(leap, yaw, elapsed_ticks);
        let reach = (step
            .x
            .abs()
            .saturating_add(step.y.abs())
            .saturating_add(step.z.abs()))
            >> 12;
        let alive = weapon.inventory().health() > 0;
        let bodies = self.monster_step_bodies(index, reach, player_mins, player_maxs, alive);
        let mut position = origin;
        let mut touched = None;
        let mut grounded = false;
        for delta in [
            Vec3I32 {
                x: step.x,
                y: step.y,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 0,
                z: step.z,
            },
        ] {
            if touched.is_some() || delta == Vec3I32::default() {
                continue;
            }
            let wished = add_vec(position, delta);
            let mut scratch = TraceScratch::default();
            let mut trace = Trace::default();
            if !self.trace_hull(map, hull, &position, &wished, &mut scratch, &mut trace) {
                return None;
            }
            let world = if trace.start_solid || trace.all_solid {
                0
            } else {
                trace.fraction
            };
            match bodies.resolve(position, wished, hull) {
                Some(impact) if impact.fraction <= world => {
                    position = impact.end;
                    touched = Some(impact.source_index);
                }
                _ => {
                    position = interpolate_segment(position, wished, world);
                    // `FL_ONGROUND`: only a floor plane taken on the way down
                    // ends the fall. A descent that starts inside solid counts
                    // too, so a monster wedged by a mover is not left hanging.
                    grounded |= delta.z < 0
                        && world < (1 << 12)
                        && (trace.normal.z >= LEAP_FLOOR_NORMAL_Q12
                            || trace.start_solid
                            || trace.all_solid);
                }
            }
        }
        if position != origin
            && !update_moving_alias_origin(map, &mut self.entities[index], position, styles)
        {
            return None;
        }
        if let Some(source) = touched {
            if let Some(damage) = runtime.leap_touch_damage() {
                result.attacks = result.attacks.saturating_add(1);
                self.leap_blow(map, index, source, damage, player_mins, weapon, result);
            }
        }
        if touched.is_some() || grounded {
            // `if (!checkbottom(self))`: floor under the box sends the
            // monster back to its run cycle, no floor and both feet on the
            // ground fire `dog_leap1` again, and no floor in mid-air simply
            // lets the arc carry on.
            if self.leap_floor_ok(map, position, hull)? {
                runtime.land_leap(true);
            } else if grounded {
                runtime.land_leap(false);
            }
        }
        if forced && !runtime.leaping() {
            self.entities[index].patrol &= !FORCED_JUMP_SOURCE;
        }
        Some(())
    }

    /// `T_Damage (other, self, self, ldmg)` from a leap that reached a body.
    #[allow(clippy::too_many_arguments)]
    fn leap_blow(
        &mut self,
        map: &ResidentMap,
        index: usize,
        source: u16,
        damage: i16,
        player_mins: Vec3I32,
        weapon: &mut WeaponState,
        result: &mut MonsterFrameResult,
    ) {
        let player_origin = player_origin_from_mins(player_mins);
        if source != PLAYER_BODY_SOURCE {
            let target = self
                .entities
                .iter()
                .position(|entity| entity.source_index == source && entity.monster.is_some());
            if let Some(target) = target {
                let distance = distance_units(self.entities[index].origin, player_origin);
                self.infight_damage(map, index, target, damage, distance, result);
            }
            return;
        }
        let before = weapon.inventory().health();
        let taken = weapon.take_damage(damage);
        // The leaping monster is its own inflictor, so the push comes from
        // the middle of its body box.
        let inflictor =
            midpoint_vec_all(self.entities[index].hit_mins, self.entities[index].hit_maxs);
        result.player_impulse = add_vec(
            result.player_impulse,
            knockback_impulse(player_origin, inflictor, damage),
        );
        if taken > 0 {
            result.player_damage = result.player_damage.saturating_add(taken as u16);
            result.push_sound(SoundEvent::player_voice(match self.frame % 3 {
                0 => 0xa0,
                1 => 0xa2,
                _ => 0xa5,
            }));
        }
        if before > 0 && weapon.inventory().health() <= 0 {
            result.player_killed = true;
            result.push_sound(SoundEvent::player_voice(0x8e));
        }
    }

    /// `checkbottom` cut down to the column the stepping code already trusts:
    /// is there floor within the 18 units a monster may be dropped onto.
    fn leap_floor_ok(&self, map: &ResidentMap, origin: Vec3I32, hull: usize) -> Option<bool> {
        let start = Vec3I32 {
            z: origin.z.saturating_add(1 << 12),
            ..origin
        };
        let end = Vec3I32 {
            z: origin.z.saturating_sub(18 << 12),
            ..origin
        };
        let mut scratch = TraceScratch::default();
        let mut trace = Trace::default();
        if !self.trace_hull(map, hull, &start, &end, &mut scratch, &mut trace) {
            return None;
        }
        Some(
            !trace.start_solid
                && !trace.all_solid
                && trace.fraction < (1 << 12)
                && trace.normal.z >= LEAP_FLOOR_NORMAL_Q12,
        )
    }

    /// The shambler bolt: a 600-unit trace from its hands that damages the
    /// player when the segment reaches their box unobstructed.
    #[allow(clippy::too_many_arguments)]
    fn monster_lightning(
        &mut self,
        map: &ResidentMap,
        shooter_index: usize,
        player_origin: Vec3I32,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        damage: i16,
    ) -> Option<i16> {
        const BOLT_RANGE_UNITS: i32 = 600;
        let shooter = self.entities.get(shooter_index)?;
        let start = Vec3I32 {
            x: shooter.origin.x,
            y: shooter.origin.y,
            z: shooter.origin.z.saturating_add(40 << 12),
        };
        let aim = Vec3I32 {
            x: player_origin.x,
            y: player_origin.y,
            z: player_origin.z.saturating_add(16 << 12),
        };
        let (forward, _, _) = aim_basis(start, aim);
        let end = Vec3I32 {
            x: start
                .x
                .saturating_add(forward.x.saturating_mul(BOLT_RANGE_UNITS)),
            y: start
                .y
                .saturating_add(forward.y.saturating_mul(BOLT_RANGE_UNITS)),
            z: start
                .z
                .saturating_add(forward.z.saturating_mul(BOLT_RANGE_UNITS)),
        };
        let mut scratch = TraceScratch::default();
        let mut world = Trace::default();
        if !self.trace_point(map, &start, &end, &mut scratch, &mut world) {
            return None;
        }
        self.lightning_beam = Some(LightningBeam {
            start,
            end: interpolate_impact(start, end, world.fraction),
        });
        self.lightning_beam_frames = 2;
        let Some(player_fraction) = segment_aabb_fraction(start, end, player_mins, player_maxs)
        else {
            return Some(0);
        };
        Some(if player_fraction < world.fraction {
            damage
        } else {
            0
        })
    }

    /// Launch one monster missile into the shared pool. Denial on full: the
    /// animation frame still plays, the projectile simply does not exist. A
    /// map whose cooked models lack the kind's alias model denies too.
    fn launch_monster_missile(
        &mut self,
        map: &ResidentMap,
        shooter_index: usize,
        player_origin: Vec3I32,
        attack: MonsterAttack,
    ) -> bool {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let Some(shooter) = self.entities.get(shooter_index).copied() else {
            return false;
        };
        let (forward, right, up) = aim_basis(shooter.origin, player_origin);
        let scale = |vector: Vec3I32, units: i32| Vec3I32 {
            x: vector.x.saturating_mul(units),
            y: vector.y.saturating_mul(units),
            z: vector.z.saturating_mul(units),
        };
        // Quake launches all four at units per second; the pools step once per
        // 60 Hz tick, so the stored velocity is units per tick in Q20.12.
        let per_tick = |units_per_second: i32| Vec3I32 {
            x: forward.x.saturating_mul(units_per_second) / 60,
            y: forward.y.saturating_mul(units_per_second) / 60,
            z: forward.z.saturating_mul(units_per_second) / 60,
        };
        let (kind, model_id, damage, origin, velocity, lifetime) = match attack {
            MonsterAttack::Grenade { damage } => (
                MonsterMissileKind::Grenade,
                GRENADE_MODEL_ID,
                damage,
                Vec3I32 {
                    z: shooter.origin.z.saturating_add(16 << 12),
                    ..shooter.origin
                },
                Vec3I32 {
                    x: forward.x.saturating_mul(600) / 60,
                    y: forward.y.saturating_mul(600) / 60,
                    z: (200 << 12) / 60,
                },
                150,
            ),
            MonsterAttack::ZombieGib { damage, offset } => (
                MonsterMissileKind::Gib,
                ZOMBIE_GIB_MODEL_ID,
                damage,
                Vec3I32 {
                    x: shooter
                        .origin
                        .x
                        .saturating_add(forward.x.saturating_mul(i32::from(offset[0]))),
                    y: shooter
                        .origin
                        .y
                        .saturating_add(forward.y.saturating_mul(i32::from(offset[0]))),
                    z: shooter.origin.z.saturating_add(i32::from(offset[2]) << 12),
                },
                Vec3I32 {
                    x: forward.x.saturating_mul(600) / 60,
                    y: forward.y.saturating_mul(600) / 60,
                    z: (200 << 12) / 60,
                },
                150,
            ),
            MonsterAttack::Spit { damage, side } => (
                MonsterMissileKind::Spit,
                NAIL_MODEL_ID,
                damage,
                add_vec(
                    add_vec(shooter.origin, scale(forward, 14)),
                    Vec3I32 {
                        x: right.x.saturating_mul(i32::from(side)),
                        y: right.y.saturating_mul(i32::from(side)),
                        z: up.z.saturating_mul(30),
                    },
                ),
                per_tick(600),
                180,
            ),
            MonsterAttack::LavaBall { damage, side } => (
                MonsterMissileKind::LavaBall,
                LAVA_BALL_MODEL_ID,
                damage,
                add_vec(
                    add_vec(shooter.origin, scale(forward, 100)),
                    Vec3I32 {
                        x: right.x.saturating_mul(i32::from(side)),
                        y: right.y.saturating_mul(i32::from(side)),
                        z: 200 << 12,
                    },
                ),
                per_tick(300),
                300,
            ),
            _ => return false,
        };
        let Some(slot_index) = self.missiles.iter().position(|slot| slot.is_none()) else {
            return false;
        };
        let render_index = self.missile_render_start as usize + slot_index;
        let Some(render) = self.entities.get_mut(render_index) else {
            return false;
        };
        if render.model_id != model_id && !set_alias_model(map, render, model_id) {
            render.visible = false;
            return false;
        }
        if !update_projectile_render(map, render, origin, velocity, &styles) {
            return false;
        }
        self.missiles[slot_index] = Some(MonsterMissile {
            origin,
            velocity,
            angles: Vec3I16::default(),
            angular_velocity: match kind {
                MonsterMissileKind::Gib => Vec3I16 {
                    x: 655,
                    y: 245,
                    z: 491,
                },
                MonsterMissileKind::LavaBall => Vec3I16 {
                    x: 136,
                    y: 68,
                    z: 205,
                },
                _ => Vec3I16 { x: 57, y: 0, z: 0 },
            },
            kind,
            damage,
            remaining_ticks: lifetime,
            resting: false,
            owner: shooter_index as u16,
        });
        self.trail_anchors[MISSILE_TRAIL_BASE + slot_index] = origin;
        true
    }

    /// The `T_Damage` attacker a missile carries: its owner while that is
    /// still a monster entity, else the world.
    #[inline(never)]
    fn missile_attacker(&self, owner: u16) -> DamageAttacker {
        match self
            .entities
            .get(usize::from(owner))
            .and_then(|entity| entity.monster)
        {
            Some(runtime) => DamageAttacker::Monster {
                index: owner,
                class_name: runtime.kind().class_name(),
            },
            None => DamageAttacker::World,
        }
    }

    /// The first thing a missile segment touches before the world does: the
    /// player's box or a live monster body other than the owner (`other ==
    /// self.owner` returns from every monster touch function).
    #[inline(never)]
    fn missile_first_hit(
        &self,
        missile: &MonsterMissile,
        end: Vec3I32,
        world_fraction: i32,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
    ) -> Option<(i32, MissileHit)> {
        let mut best: Option<(i32, MissileHit)> = None;
        if let Some(fraction) = segment_aabb_fraction(missile.origin, end, player_mins, player_maxs)
        {
            best = Some((fraction, MissileHit::Player));
        }
        for &index in &self.collision_indices {
            let index = usize::from(index);
            if index == usize::from(missile.owner) {
                continue;
            }
            let entity = &self.entities[index];
            let Some(runtime) = entity.monster else {
                continue;
            };
            if !entity.visible || !entity.damageable || !runtime.body_solid() {
                continue;
            }
            let Some(fraction) =
                segment_aabb_fraction(missile.origin, end, entity.hit_mins, entity.hit_maxs)
            else {
                continue;
            };
            if best.is_none_or(|(nearest, _)| fraction < nearest) {
                best = Some((fraction, MissileHit::Monster(index)));
            }
        }
        best.filter(|(fraction, _)| *fraction <= world_fraction)
    }

    /// `OgreGrenadeExplode` and the lava ball's `T_MissileTouch`: radius
    /// damage around the impact to everything that takes damage, the player
    /// included unless `spare_player` (`T_RadiusDamage`'s `ignore`, the
    /// target a lava ball already hit directly), then the explosion itself.
    /// The player's origin is recovered from the hull box the caller already
    /// holds (mins + the hull's 16/16/24 offsets).
    #[allow(clippy::too_many_arguments)]
    fn explode_monster_missile(
        &mut self,
        map: &ResidentMap,
        impact: Vec3I32,
        damage: i16,
        player_mins: Vec3I32,
        spare: Option<MissileHit>,
        attacker: DamageAttacker,
        weapon: &mut WeaponState,
        result: &mut MonsterFrameResult,
    ) -> Option<()> {
        let player_origin = player_origin_from_mins(player_mins);
        let (player_origin, spare_entity) = match spare {
            Some(MissileHit::Player) => (None, None),
            Some(MissileHit::Monster(index)) => (Some(player_origin), Some(index)),
            None => (Some(player_origin), None),
        };
        self.explode_at(
            map,
            impact,
            damage,
            player_origin,
            spare_entity,
            attacker,
            weapon,
            result,
        )
    }

    /// One `T_RadiusDamage` from a monster-side inflictor plus its
    /// `r_exp3` and `TE_EXPLOSION`, folded into a monster frame result.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    #[optimize(size)]
    fn explode_at(
        &mut self,
        map: &ResidentMap,
        impact: Vec3I32,
        damage: i16,
        player_origin: Option<Vec3I32>,
        spare_entity: Option<usize>,
        attacker: DamageAttacker,
        weapon: &mut WeaponState,
        result: &mut MonsterFrameResult,
    ) -> Option<()> {
        let before = weapon.inventory().health();
        let mut splash = RocketResult::default();
        self.apply_rocket_impact(
            map,
            impact,
            0,
            damage,
            spare_entity,
            spare_entity.is_some(),
            player_origin,
            attacker,
            weapon,
            &mut splash,
        )?;
        result.player_impulse = add_vec(result.player_impulse, splash.player_impulse);
        if splash.self_damage > 0 {
            result.player_damage = result.player_damage.saturating_add(splash.self_damage);
            result.push_sound(SoundEvent::player_voice(0xa0));
        }
        if before > 0 && weapon.inventory().health() <= 0 {
            result.player_killed = true;
            result.push_sound(SoundEvent::player_voice(0x8e));
        }
        result.push_sound(SoundEvent::at(0xc8, impact));
        result.last_explosion = Some(impact);
        Some(())
    }

    /// `barrel_explode` for every `misc_explobox` killed since the last pass:
    /// `T_RadiusDamage (self, self, 160, world)` from the box's origin, the
    /// explosion sound and sprite (`origin_z + 32`), then the box is gone.
    /// A barrel caught in the blast dies inside this loop and detonates on
    /// a later iteration, so a chain goes off in the same frame.
    #[optimize(size)]
    #[cold]
    #[inline(never)]
    pub fn detonate_pending_explosions(
        &mut self,
        map: &ResidentMap,
        player_origin: Vec3I32,
        weapon: &mut WeaponState,
    ) -> Option<MonsterFrameResult> {
        let mut result = MonsterFrameResult::default();
        if !self.pending_scene_work {
            return Some(result);
        }
        // This is the last pass of the frame; kills made below (or after this
        // point) re-arm the flag for the next frame's gib pass.
        self.pending_scene_work = false;
        let mut index = 0usize;
        while index < self.entities.len() {
            if !self.entities[index].pending_explosion {
                index += 1;
                continue;
            }
            self.entities[index].pending_explosion = false;
            self.entities[index].visible = false;
            let origin = self.entities[index].origin;
            self.explode_at(
                map,
                origin,
                EXPLOBOX_SPLASH_DAMAGE,
                Some(player_origin),
                None,
                DamageAttacker::World,
                weapon,
                &mut result,
            )?;
            result.last_explosion = Some(Vec3I32 {
                x: origin.x,
                y: origin.y,
                z: origin.z.saturating_add(32 << 12),
            });
            // A chained barrel earlier in the table died just now; sweep again.
            index = 0;
        }
        Some(result)
    }

    /// `ThrowGib` x3 for every entity gibbed since the last pass: three chunks
    /// (gib1, gib2, gib3) at the corpse with `VelocityForDamage`'s spread,
    /// bouncing and harmless, gone after four seconds. Uses free missile
    /// slots; a full pool throws fewer.
    fn throw_pending_gibs(&mut self, map: &ResidentMap, result: &mut MonsterFrameResult) {
        if let Some((origin, health)) = self.pending_player_gib.take() {
            self.throw_gibs(map, origin, health, self.entities.len(), result);
        }
        if !self.pending_scene_work {
            return;
        }
        for index in 0..self.entities.len() {
            if !self.entities[index].pending_gib {
                continue;
            }
            self.entities[index].pending_gib = false;
            let origin = self.entities[index].origin;
            let health = self.entities[index].health;
            self.throw_gibs(map, origin, health, index, result);
        }
    }

    /// `GibPlayer`'s chunks: the player's own death has no corpse entity, so
    /// the scene throws them from the origin the game hands over.
    #[optimize(size)]
    pub fn gib_player(&mut self, origin: Vec3I32, health: i16) {
        self.pending_player_gib = Some((origin, health));
    }

    #[optimize(size)]
    fn throw_gibs(
        &mut self,
        map: &ResidentMap,
        origin: Vec3I32,
        health: i16,
        index: usize,
        result: &mut MonsterFrameResult,
    ) {
        const GIB_MODEL_IDS: [i16; 3] = [0x27, 0x28, 0x29];
        const DEBRIS_TICKS: u16 = 240;
        let styles = self.light_styles;
        {
            result.last_gib = Some(origin);
            // Deterministic spread: a small hash of frame and entity index
            // stands in for `crandom`.
            let mut seed = (self.frame as u32)
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(index as u32 * 0x85eb_ca6b);
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            for &model_id in &GIB_MODEL_IDS {
                let Some(slot_index) = self.missiles.iter().position(|slot| slot.is_none()) else {
                    return;
                };
                // `VelocityForDamage`: 100*crandom, 100*crandom, 200+100*random,
                // scaled by 0.7 above -50, 2 above -200, 10 below.
                let random = next();
                let crandom = |bits: u32| i32::from(bits as u16) - 32_768; // -32768..32767
                let scale_q12 = if health > -50 {
                    2_867
                } else if health > -200 {
                    8_192
                } else {
                    40_960
                };
                let axis = |value: i32| mul_q12_i32(value, scale_q12) / 60;
                let velocity = Vec3I32 {
                    x: axis(crandom(random) * 100 / 8), // 100 * crandom in Q12
                    y: axis(crandom(random >> 11) * 100 / 8),
                    z: axis((200 << 12) + i32::from((random >> 22) as u16 & 0x3ff) * 100 * 4),
                };
                let render_index = self.missile_render_start as usize + slot_index;
                let Some(render) = self.entities.get_mut(render_index) else {
                    return;
                };
                if render.model_id != model_id && !set_alias_model(map, render, model_id) {
                    render.visible = false;
                    return;
                }
                if !update_projectile_render(map, render, origin, velocity, &styles) {
                    return;
                }
                self.missiles[slot_index] = Some(MonsterMissile {
                    origin,
                    velocity,
                    angles: Vec3I16::default(),
                    angular_velocity: Vec3I16 {
                        x: 655 + (random & 0xff) as i16,
                        y: 245,
                        z: 491 - ((random >> 8) & 0xff) as i16,
                    },
                    kind: MonsterMissileKind::Debris,
                    damage: 0,
                    remaining_ticks: DEBRIS_TICKS,
                    resting: false,
                    owner: index as u16,
                });
                self.trail_anchors[MISSILE_TRAIL_BASE + slot_index] = origin;
            }
        }
    }

    /// `DropBackpack`: `progs/backpack.mdl` 24 units under the corpse,
    /// `velocity_z = 300` with `-100 + random*200` sideways, `MOVETYPE_TOSS`,
    /// `SUB_Remove` after 120 seconds. Uses a free missile slot.
    #[optimize(size)]
    fn drop_backpack(&mut self, map: &ResidentMap, origin: Vec3I32, ammo: BackpackAmmo) {
        const BACKPACK_MODEL_ID: i16 = 0x11;
        const BACKPACK_TICKS: u16 = 7_200;
        let Some(slot_index) = self.missiles.iter().position(|slot| slot.is_none()) else {
            return;
        };
        let render_index = self.missile_render_start as usize + slot_index;
        let styles = self.light_styles;
        let Some(render) = self.entities.get_mut(render_index) else {
            return;
        };
        if render.model_id != BACKPACK_MODEL_ID && !set_alias_model(map, render, BACKPACK_MODEL_ID)
        {
            render.visible = false;
            return;
        }
        let origin = Vec3I32 {
            x: origin.x,
            y: origin.y,
            z: origin.z - (24 << 12),
        };
        let mut seed = (self.frame as u32).wrapping_mul(0x9e37_79b9) ^ (origin.x as u32);
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        // Per-tick velocity: units per second in Q12, over 60 ticks.
        let sideways =
            |bits: u32| ((-100 + i32::from((bits & 0xffff) as u16) * 200 / 65_536) << 12) / 60;
        let velocity = Vec3I32 {
            x: sideways(seed),
            y: sideways(seed >> 16),
            z: (300 << 12) / 60,
        };
        if !update_projectile_render(map, render, origin, velocity, &styles) {
            return;
        }
        render.angles = Vec3I16::default();
        self.missiles[slot_index] = Some(MonsterMissile {
            origin,
            velocity,
            angles: Vec3I16::default(),
            angular_velocity: Vec3I16::default(),
            kind: MonsterMissileKind::Backpack(ammo),
            damage: 0,
            remaining_ticks: BACKPACK_TICKS,
            resting: false,
            owner: u16::MAX,
        });
        self.trail_anchors[MISSILE_TRAIL_BASE + slot_index] = origin;
    }

    /// Where one live projectile is and what it leaves behind, in trail
    /// anchor order: rockets, then grenades, then monster missiles.
    ///
    /// Outlined on purpose. Walking the three pools inline unrolled into
    /// four kilobytes of image, and the regression harness builds boot into
    /// the end of the bump allocator with about one to spare.
    #[inline(never)]
    fn trail_source(&self, slot: usize) -> Option<(Vec3I32, ParticleKind, i32)> {
        /// `EF_ROCKET` and `EF_GRENADE` spacing in world units.
        const SMOKE_STEP_UNITS: i32 = 12;
        /// `EF_GIB` blood. Thrown gibs are slow, so this is already about one
        /// particle every few frames.
        const GIB_STEP_UNITS: i32 = 24;
        /// `EF_ZOMGIB` is the original's "slight blood", at half the rate.
        const ZOMBIE_GIB_STEP_UNITS: i32 = 48;
        if slot < MAX_ROCKETS {
            let rocket = self.rockets.get(slot)?.as_ref()?;
            return Some((rocket.origin, ParticleKind::Fire, SMOKE_STEP_UNITS));
        }
        if slot < MISSILE_TRAIL_BASE {
            let grenade = self.grenades.get(slot - MAX_ROCKETS)?.as_ref()?;
            return Some((grenade.origin, ParticleKind::Smoke, SMOKE_STEP_UNITS));
        }
        let missile = self.missiles.get(slot - MISSILE_TRAIL_BASE)?.as_ref()?;
        let (kind, step_units) = match missile.kind {
            MonsterMissileKind::Grenade => (ParticleKind::Smoke, SMOKE_STEP_UNITS),
            MonsterMissileKind::Debris => (ParticleKind::Blood, GIB_STEP_UNITS),
            MonsterMissileKind::Gib => (ParticleKind::Blood, ZOMBIE_GIB_STEP_UNITS),
            // The spit, the lava ball and the backpack carry no model effect
            // flag, so they leave nothing behind.
            _ => return None,
        };
        Some((missile.origin, kind, step_units))
    }

    /// `CL_RelinkEntities`' model-flag trails, in one pass over the live
    /// projectile pools. The original walks each frame's segment in 3-unit
    /// steps and keeps every particle for two seconds; the port steps far
    /// coarser, caps one projectile at two particles per frame and lives a
    /// fifth of a second, which is what the shared pool can afford.
    ///
    /// The anchors live in their own array rather than in the projectiles so
    /// that the three big update loops, which copy a projectile in and out of
    /// its `Option` slot many times over, do not each grow with them.
    #[inline(never)]
    pub fn emit_projectile_trails(&mut self, particles: &mut ImpactParticles) {
        for slot in 0..self.trail_anchors.len() {
            let Some((origin, kind, step_units)) = self.trail_source(slot) else {
                continue;
            };
            self.trail_anchors[slot] =
                particles.spawn_trail(self.trail_anchors[slot], origin, kind, step_units);
        }
    }

    /// Advance the shared monster missile pool. Ballistic kinds fall and
    /// bounce like Quake's `MOVETYPE_BOUNCE`; straight kinds fly until they
    /// touch. Direct hits land on the player and on any live monster other
    /// than the owner (which is how the original starts most infights); the
    /// ogre grenade and the lava ball also splash whatever takes damage
    /// around them.
    #[inline(never)]
    pub fn update_monster_missiles(
        &mut self,
        map: &ResidentMap,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        weapon: &mut WeaponState,
        elapsed_ticks: u16,
    ) -> Option<MonsterFrameResult> {
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        const GRAVITY_STEP_Q12: i32 = 910;
        const OVERBOUNCE_Q12: i32 = 6_144;
        let mut result = MonsterFrameResult::default();
        self.throw_pending_gibs(map, &mut result);
        let ticks = rocket_elapsed_ticks(elapsed_ticks);
        let mut slot = 0usize;
        while slot < self.missiles.len() {
            let Some(mut missile) = self.missiles[slot] else {
                slot += 1;
                continue;
            };
            let mut tick = 0u16;
            let mut removed = false;
            while tick < ticks {
                if projectile_expires_this_tick(&mut missile.remaining_ticks) {
                    // A timed-out lava ball is `SUB_Remove`d silently; only
                    // the ogre grenade's fuse ends in `OgreGrenadeExplode`.
                    if matches!(missile.kind, MonsterMissileKind::Grenade) {
                        self.explode_monster_missile(
                            map,
                            missile.origin,
                            missile.damage,
                            player_mins,
                            None,
                            self.missile_attacker(missile.owner),
                            weapon,
                            &mut result,
                        )?;
                    }
                    removed = true;
                    break;
                }
                if let MonsterMissileKind::Backpack(ammo) = missile.kind {
                    // `BackpackTouch`: `SOLID_TRIGGER` box '-16 -16 0' '16 16 56'
                    // against the player, whether the pack still flies or rests.
                    let mins = add_vec(
                        missile.origin,
                        Vec3I32 {
                            x: -16 << 12,
                            y: -16 << 12,
                            z: 0,
                        },
                    );
                    let maxs = add_vec(
                        missile.origin,
                        Vec3I32 {
                            x: 16 << 12,
                            y: 16 << 12,
                            z: 56 << 12,
                        },
                    );
                    if weapon.inventory().health() > 0
                        && aabb_overlaps(mins, maxs, player_mins, player_maxs)
                    {
                        for (kind, amount) in [
                            (AmmoKind::Shells, ammo.shells),
                            (AmmoKind::Rockets, ammo.rockets),
                        ] {
                            if amount != 0 {
                                weapon.apply_pickup(Pickup::Ammo {
                                    kind,
                                    amount: u16::from(amount),
                                });
                            }
                        }
                        result.push_sound(
                            SoundEvent::listener(0xc5)
                                .on(crate::audio::OWNER_PLAYER, crate::audio::CHAN_ITEM),
                        );
                        result.backpack_pickup = Some(ammo);
                        removed = true;
                        break;
                    }
                }
                if missile.resting {
                    tick += 1;
                    continue;
                }
                if missile.kind.ballistic() {
                    missile.angles.x = missile.angles.x.wrapping_add(missile.angular_velocity.x);
                    missile.angles.y = missile.angles.y.wrapping_add(missile.angular_velocity.y);
                    missile.angles.z = missile.angles.z.wrapping_add(missile.angular_velocity.z);
                    missile.velocity.z = missile.velocity.z.saturating_sub(GRAVITY_STEP_Q12);
                }
                let end = add_vec(missile.origin, missile.velocity);
                let mut scratch = TraceScratch::default();
                let mut world = Trace::default();
                if !self.trace_point(map, &missile.origin, &end, &mut scratch, &mut world) {
                    return None;
                }
                let hit = if missile.kind.touches_player() {
                    self.missile_first_hit(&missile, end, world.fraction, player_mins, player_maxs)
                } else {
                    None
                };
                if let Some((fraction, hit)) = hit {
                    let attacker = self.missile_attacker(missile.owner);
                    let impact = interpolate_segment(missile.origin, end, fraction);
                    if matches!(missile.kind, MonsterMissileKind::Grenade) {
                        // `OgreGrenadeTouch`: contact with anything that takes
                        // damage detonates the grenade where it is.
                        self.explode_monster_missile(
                            map,
                            impact,
                            missile.damage,
                            player_mins,
                            None,
                            attacker,
                            weapon,
                            &mut result,
                        )?;
                        result.attacks = result.attacks.saturating_add(1);
                        removed = true;
                        break;
                    }
                    match hit {
                        MissileHit::Player => {
                            let before = weapon.inventory().health();
                            let taken = weapon.take_damage(missile.damage);
                            // `T_Damage (other, self, self.owner, ...)`: the
                            // missile is the inflictor, so it pushes from where
                            // it touched.
                            result.player_impulse = add_vec(
                                result.player_impulse,
                                knockback_impulse(
                                    player_origin_from_mins(player_mins),
                                    impact,
                                    missile.damage,
                                ),
                            );
                            if taken > 0 {
                                result.player_damage =
                                    result.player_damage.saturating_add(taken as u16);
                                result.push_sound(SoundEvent::player_voice(0xa0));
                            }
                            if before > 0 && weapon.inventory().health() <= 0 {
                                result.player_killed = true;
                                result.push_sound(SoundEvent::player_voice(0x8e));
                            }
                        }
                        MissileHit::Monster(index) => {
                            let mut damage = DamageResult::default();
                            apply_entity_damage(
                                map,
                                self.nightmare(),
                                &mut self.entities[index],
                                missile.damage,
                                attacker,
                                &mut damage,
                                &mut self.pending_scene_work,
                            );
                            if let Some(sound) = damage.response_sound {
                                result.push_sound(sound);
                            }
                        }
                    }
                    if matches!(missile.kind, MonsterMissileKind::Gib) {
                        result.push_sound(SoundEvent::at(ZOMBIE_HIT_SOUND, impact));
                    }
                    if matches!(missile.kind, MonsterMissileKind::LavaBall) {
                        // `T_MissileTouch`: the direct hit is followed by
                        // `T_RadiusDamage (self, self.owner, 120, other)`,
                        // sparing whoever it just struck.
                        self.explode_monster_missile(
                            map,
                            impact,
                            LAVA_BALL_SPLASH_DAMAGE,
                            player_mins,
                            Some(hit),
                            attacker,
                            weapon,
                            &mut result,
                        )?;
                    }
                    result.attacks = result.attacks.saturating_add(1);
                    removed = true;
                    break;
                }
                if world.fraction < 1 << 12 {
                    if !missile.kind.ballistic() {
                        if matches!(missile.kind, MonsterMissileKind::LavaBall)
                            && world_point_contents(map, end) != Some(CONTENTS_SKY)
                        {
                            // `T_MissileTouch` against the world: no direct
                            // target, so the splash reaches everyone. Into
                            // the sky it is simply removed.
                            self.explode_monster_missile(
                                map,
                                interpolate_segment(missile.origin, end, world.fraction),
                                LAVA_BALL_SPLASH_DAMAGE,
                                player_mins,
                                None,
                                self.missile_attacker(missile.owner),
                                weapon,
                                &mut result,
                            )?;
                        }
                        removed = true;
                        break;
                    }
                    missile.origin = interpolate_segment(missile.origin, end, world.fraction);
                    if matches!(missile.kind, MonsterMissileKind::Gib) {
                        // `ZombieGrenadeTouch` against the world: z_miss, then
                        // the flesh stops dead and waits out its lifetime.
                        result.push_sound(SoundEvent::at(ZOMBIE_MISS_SOUND, missile.origin));
                        missile.velocity = Vec3I32::default();
                        missile.angular_velocity = Vec3I16::default();
                        missile.resting = true;
                        tick += 1;
                        continue;
                    }
                    let normal = world.normal;
                    let dot = mul_q12_i32(missile.velocity.x, i32::from(normal.x))
                        .saturating_add(mul_q12_i32(missile.velocity.y, i32::from(normal.y)))
                        .saturating_add(mul_q12_i32(missile.velocity.z, i32::from(normal.z)));
                    // `MOVETYPE_TOSS` clips with backoff 1: the backpack
                    // slides off walls and lies down instead of bouncing.
                    let overbounce = if matches!(missile.kind, MonsterMissileKind::Backpack(_)) {
                        1 << 12
                    } else {
                        OVERBOUNCE_Q12
                    };
                    let impulse = mul_q12_i32(dot, overbounce);
                    missile.velocity.x = missile
                        .velocity
                        .x
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.x)));
                    missile.velocity.y = missile
                        .velocity
                        .y
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.y)));
                    missile.velocity.z = missile
                        .velocity
                        .z
                        .saturating_sub(mul_q12_i32(impulse, i32::from(normal.z)));
                    if settle_grenade_motion(
                        &mut missile.velocity,
                        &mut missile.angular_velocity,
                        normal.z,
                    ) {
                        missile.resting = true;
                    }
                } else {
                    missile.origin = end;
                }
                tick += 1;
            }
            self.missiles[slot] = (!removed).then_some(missile);
            let render_index = self.missile_render_start as usize + slot;
            let render = self.entities.get_mut(render_index)?;
            if removed {
                render.visible = false;
            } else if !update_projectile_render(
                map,
                render,
                missile.origin,
                missile.velocity,
                &styles,
            ) {
                return None;
            } else if missile.kind.ballistic() {
                render.angles = missile.angles;
            }
            slot += 1;
        }
        Some(result)
    }

    /// Raise Chthon at his authored origin. This is the encounter trigger's
    /// edge; nothing else can start the fight.
    #[optimize(size)]
    fn awaken_boss(&mut self, source_index: u16) -> bool {
        let skill = self.skill;
        let Some(entity) = self
            .entities
            .iter_mut()
            .find(|entity| entity.source_index == source_index)
        else {
            return false;
        };
        let Some(mut runtime) = entity.monster else {
            return false;
        };
        if !runtime.kind().is_boss() || runtime.active() {
            return false;
        }
        runtime.awaken(skill);
        entity.monster = Some(runtime);
        entity.frame = runtime.frame();
        entity.visible = true;
        true
    }

    /// `monster_use`: a targeted monster at rest takes the player as its enemy
    /// and starts hunting (`FoundTarget`, sight sound included). One already
    /// hunting, or dead, ignores the use.
    #[optimize(size)]
    #[cold]
    #[inline(never)]
    fn wake_monster(&mut self, source_index: u16) -> Option<SoundEvent> {
        let entity = self
            .entities
            .iter_mut()
            .find(|entity| entity.source_index == source_index)?;
        let mut runtime = entity.monster?;
        if runtime.active() || !runtime.set_enemy(MonsterEnemy::Player) {
            return None;
        }
        entity.monster = Some(runtime);
        entity.frame = runtime.frame();
        Some(
            SoundEvent::at(runtime.kind().sight_sound(), entity.origin)
                .on(entity.source_index, crate::audio::CHAN_VOICE),
        )
    }

    /// One `event_lightning` shock of the authored Chthon kill chain. The
    /// second element is his origin on the shock that kills him, which is
    /// where `boss_death9` broadcasts `TE_LAVASPLASH`.
    fn shock_boss(&mut self) -> Option<(Option<SoundEvent>, Option<Vec3I32>)> {
        for entity in &mut self.entities {
            let Some(mut runtime) = entity.monster else {
                continue;
            };
            if !runtime.kind().is_boss() {
                continue;
            }
            let transition = runtime.apply_shock()?;
            entity.monster = Some(runtime);
            entity.frame = transition.frame;
            entity.health = runtime.boss_shocks();
            return Some((
                transition.sound_id.map(|sound| {
                    SoundEvent::at(sound, entity.origin)
                        .on(entity.source_index, crate::audio::CHAN_VOICE)
                }),
                runtime.dead().then_some(entity.origin),
            ));
        }
        None
    }

    #[optimize(size)]
    #[allow(clippy::too_many_arguments)]
    fn soldier_attack_damage(
        &self,
        map: &ResidentMap,
        shooter_index: usize,
        player_origin: Vec3I32,
        player_velocity: Vec3I32,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
        spread: [[i16; 2]; 4],
    ) -> Option<i16> {
        const SPREAD_Q12: i32 = 410;
        const RANGE: i32 = 2_048;
        let shooter = self.entities.get(shooter_index)?;
        let aim = predicted_target(player_origin, player_velocity);
        let (forward, right, up) = aim_basis(shooter.origin, aim);
        let height = shooter.hit_maxs.z.saturating_sub(shooter.hit_mins.z);
        let muzzle_height = height / 10 * 7 + height % 10 * 7 / 10;
        let start = Vec3I32 {
            x: shooter
                .origin
                .x
                .saturating_add(forward.x.saturating_mul(10)),
            y: shooter
                .origin
                .y
                .saturating_add(forward.y.saturating_mul(10)),
            z: shooter.hit_mins.z.saturating_add(muzzle_height),
        };
        let mut hits = 0i16;
        for [up_random, right_random] in spread {
            let random_up = mul_q12_i32(i32::from(up_random), SPREAD_Q12);
            let random_right = mul_q12_i32(i32::from(right_random), SPREAD_Q12);
            let direction = Vec3I32 {
                x: forward
                    .x
                    .saturating_add(mul_q12_i32(random_up, up.x))
                    .saturating_add(mul_q12_i32(random_right, right.x)),
                y: forward
                    .y
                    .saturating_add(mul_q12_i32(random_up, up.y))
                    .saturating_add(mul_q12_i32(random_right, right.y)),
                z: forward
                    .z
                    .saturating_add(mul_q12_i32(random_up, up.z))
                    .saturating_add(mul_q12_i32(random_right, right.z)),
            };
            let end = Vec3I32 {
                x: start.x.saturating_add(direction.x.saturating_mul(RANGE)),
                y: start.y.saturating_add(direction.y.saturating_mul(RANGE)),
                z: start.z.saturating_add(direction.z.saturating_mul(RANGE)),
            };
            let Some(player_fraction) = segment_aabb_fraction(start, end, player_mins, player_maxs)
            else {
                continue;
            };
            let mut scratch = TraceScratch::default();
            let mut world = Trace::default();
            if !self.trace_point(map, &start, &end, &mut scratch, &mut world) {
                return None;
            }
            let mut blocking_fraction = world.fraction;
            for (index, entity) in self.entities.iter().enumerate() {
                if index == shooter_index
                    || !entity.visible
                    || !entity.damageable
                    || entity.health <= 0
                {
                    continue;
                }
                if let Some(fraction) =
                    segment_aabb_fraction(start, end, entity.hit_mins, entity.hit_maxs)
                {
                    blocking_fraction = blocking_fraction.min(fraction);
                }
            }
            if player_fraction < blocking_fraction {
                hits += 1;
            }
        }
        Some(hits.saturating_mul(4))
    }

    /// Current skill, carried across map loads like the original cvar.
    pub const fn skill(&self) -> u8 {
        self.skill
    }

    /// Loaded `func_train` count and the corners they have reached so far.
    pub fn train_stats(&self) -> (u16, u16) {
        let mut arrivals = 0u16;
        for train in &self.trains {
            arrivals = arrivals.saturating_add(train.policy.corner_arrivals());
        }
        (self.trains.len().min(u16::MAX as usize) as u16, arrivals)
    }

    /// `found_secrets` and `total_secrets` for this resident map.
    pub const fn secrets(&self) -> (u16, u16) {
        (self.secrets.found(), self.secrets.total())
    }

    /// Copy the authored non-silent teleporter centers into audio's fixed
    /// reservation. This happens once per map; listener updates then stay
    /// inside `AudioBank` and keep the hot call ABI unchanged.
    #[optimize(size)]
    pub fn copy_teleporter_hums(&self, out: &mut [MaybeUninit<Vec3I32>]) -> usize {
        let mut count = 0;
        for trigger in &self.teleports {
            if count == out.len() {
                break;
            }
            let Some(origin) =
                teleport::teleporter_hum_origin(trigger.mins, trigger.maxs, trigger.spawn_flags)
            else {
                continue;
            };
            out[count].write(origin);
            count += 1;
        }
        count
    }

    /// The first moving train's leg inputs, for the guest diagnostic.
    #[cfg(feature = "episode1-regression")]
    pub fn regression_train_leg_debug(out: &mut [i32; 13]) {
        unsafe {
            *out = core::ptr::read_volatile(core::ptr::addr_of!(TRAIN_LEG_DEBUG));
        }
    }

    /// Guest-side `func_train` evidence, written into caller-owned storage.
    ///
    /// A host measurement of a leg length is not evidence that the guest moves
    /// a train correctly: the guest once computed a saturated 27804 tick leg
    /// where the host computed 87 off the same code and the same cooked data.
    /// `map-regress` reads this on E1M5, whose four authored trains start
    /// running the moment the map loads.
    ///
    /// `out` is `[longest leg in ticks, trains moving, sum of |origin| in whole
    /// units]`. The last is a position checksum the caller watches for change,
    /// which is how a moving train proves it moved rather than merely claiming
    /// a leg length.
    #[cfg(feature = "episode1-regression")]
    pub fn regression_train_probe(&self, out: &mut [u32; 3]) {
        out[0] = 0;
        out[1] = 0;
        out[2] = 0;
        for train in &self.trains {
            if out[0] == 0 && train.policy.state() == quake_core::train::TrainState::Moving {
                let mut debug = [0i32; 13];
                train.policy.leg_debug_into(&mut debug);
                unsafe {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(TRAIN_LEG_DEBUG), debug);
                }
            }
            if train.policy.state() == quake_core::train::TrainState::Moving {
                out[1] = out[1].saturating_add(1);
                out[0] = out[0].max(u32::from(train.policy.leg_ticks()));
            }
            let origin = train.policy.origin();
            let sum = (origin.x >> 12).unsigned_abs()
                + (origin.y >> 12).unsigned_abs()
                + (origin.z >> 12).unsigned_abs();
            out[2] = out[2].wrapping_add(sum);
        }
    }

    /// `killed_monsters` and `total_monsters` for this resident map.
    pub const fn kills(&self) -> (u16, u16) {
        (self.monsters.killed(), self.monsters.total())
    }

    /// The authored intermission camera, if this map places one.
    pub const fn intermission_spot(&self) -> Option<IntermissionSpot> {
        self.intermission
    }

    pub fn entities(&self) -> &[RenderEntity] {
        &self.entities
    }

    /// Compose the immutable world hull with solid translated brush entities
    /// for a frame of player traces anchored at `anchor` (the player origin).
    pub fn collision<'a>(
        &'a self,
        map: &'a ResidentMap,
        anchor: Vec3I32,
    ) -> Option<SceneCollision<'a>> {
        let world = map.brush_models().get(0)?;
        Some(SceneCollision {
            map,
            entities: &self.entities,
            collision_indices: &self.collision_indices,
            world_head_node: world.head_nodes[1],
            ignored: u16::MAX,
            near: NearCandidates::gather(&self.entities, &self.collision_indices, anchor),
        })
    }

    /// The same composition with one brush entity taken out of it, for a
    /// pusher's own `SV_PushEntity` trace.
    fn collision_without<'a>(
        &'a self,
        map: &'a ResidentMap,
        render_index: u16,
    ) -> Option<SceneCollision<'a>> {
        let world = map.brush_models().get(0)?;
        Some(SceneCollision {
            map,
            entities: &self.entities,
            collision_indices: &self.collision_indices,
            world_head_node: world.head_nodes[1],
            ignored: render_index,
            near: None,
        })
    }

    /// Quake's `groundentity` narrowed to brush entities: the solid submodel a
    /// resting player box is standing on, as a render index.
    ///
    /// The locomotion motor has already snapped a grounded box onto whatever it
    /// rests on, so re-asking the same two-unit downward probe and keeping the
    /// nearest walkable contact names that surface. The world answering first
    /// means the ground is the world, which no pusher ever carries anything for.
    fn ground_brush_entity(&self, map: &ResidentMap, rider: &Rider) -> Option<u16> {
        if !rider.grounded {
            return None;
        }
        let below = Vec3I32 {
            x: rider.origin.x,
            y: rider.origin.y,
            z: rider.origin.z.saturating_sub(GROUND_PROBE_Q12),
        };
        let world_model = map.brush_models().get(0)?;
        let world = Some(unsafe {
            CollisionHull::from_native_clip_nodes(
                map.collision_planes(),
                map.collision_clip_nodes(),
                world_model.head_nodes[1],
            )
        })?;
        let mut scratch = TraceScratch::default();
        let mut best = Trace::default();
        if !world.trace_into(&rider.origin, &below, &mut scratch, &mut best) {
            return None;
        }
        let mut ground = None;
        for (index, entity) in self.entities.iter().enumerate() {
            if !entity.visible || !entity.solid || entity.model_id >= 0 {
                continue;
            }
            if !swept_player_overlaps_entity(rider.origin, below, entity) {
                continue;
            }
            let Some(model) = map.brush_models().get(entity.model_index as usize) else {
                continue;
            };
            let Some(hull) = Some(unsafe {
                CollisionHull::from_native_clip_nodes(
                    map.collision_planes(),
                    map.collision_clip_nodes(),
                    model.head_nodes[1],
                )
            }) else {
                continue;
            };
            let mut candidate = Trace::default();
            if !trace_translated_hull(
                hull,
                entity.origin,
                &rider.origin,
                &below,
                &mut scratch,
                &mut candidate,
            ) {
                continue;
            }
            if candidate.fraction <= best.fraction
                && candidate.fraction < Q12_ONE
                && i32::from(candidate.normal.z) >= WALKABLE_NORMAL_Q12
            {
                best = candidate;
                ground = Some(index as u16);
            }
        }
        ground
    }

    /// Write one mover's canonical transform into its render entity and rebuild
    /// the bounds and leaf that render, broad phase and collision all read.
    fn place_mover(&mut self, map: &ResidentMap, render_index: usize, mover_index: usize) {
        let origin = self.movers[mover_index].policy.transform().origin;
        let entity = &mut self.entities[render_index];
        if entity.origin == origin {
            return;
        }
        entity.origin = origin;
        let model = map
            .brush_models()
            .get(entity.model_index as usize)
            .expect("mover model index validated at load");
        entity.clip_mins = translated_model_bounds(origin, model.mins);
        entity.clip_maxs = translated_model_bounds(origin, model.maxs);
        let center = bounds_center(entity.clip_mins, entity.clip_maxs);
        if let Some(leaf) = map.point_leaf_index(center) {
            entity.leaf_index = leaf.min(u16::MAX as usize) as u16;
        }
    }

    /// Clear the master door message across one linked chain.
    ///
    /// Original Quake stores the text on the linked door owner and clears it
    /// in both `door_use` and `door_fire`. This port copies the authored text
    /// into each mover instead, so clearing every ordinary door in the group
    /// is the equivalent representation and costs no new runtime storage.
    #[optimize(size)]
    #[inline(never)]
    fn disarm_door_message_group(&mut self, mover_index: usize) {
        let Some(mover) = self.movers.get(mover_index) else {
            return;
        };
        if mover.source.class_name != 0x0c {
            return;
        }
        let group = mover.link_group;
        for linked in &mut self.movers {
            if linked.link_group == group && linked.source.class_name == 0x0c {
                linked.source.string = 0;
            }
        }
    }

    /// `SV_PushMove`'s rider half for one pusher that has already moved by
    /// `delta`. Returns the participant passed to the original's `blocked`
    /// function, or `None` when the step committed.
    ///
    /// Both kinds of body Episode 1 ever puts on a pusher are carried: the
    /// player, and live monsters. E1M4 authors two ogres and a knight standing
    /// on the deck of `func_train` #26, so a player-only carry would leave them
    /// hanging in the air over the slime.
    ///
    /// The step is atomic. Every body is resolved into a
    /// [`PushLedger`](crate::pusher::PushLedger) before
    /// any of them moves, so a block reported by the last monster leaves the
    /// player exactly as unmoved as a block reported by the first. Applying the
    /// player's carry as soon as it was known left a blocked tick with the
    /// pusher and the monsters back where they started and the player still
    /// carried, which is a world the original never produces.
    fn push_riders(
        &mut self,
        map: &ResidentMap,
        rider: &mut Rider,
        ground_entity: Option<u16>,
        render_index: usize,
        delta: Vec3I32,
    ) -> Option<PushBlocker> {
        if delta == (Vec3I32 { x: 0, y: 0, z: 0 }) {
            return None;
        }
        // `R_LightPoint` reads the live style table; copy it so the per-entity
        // relight below never fights the &mut borrow of the entity pool.
        let styles = self.light_styles;
        let mins = whole_units_q12(self.entities[render_index].clip_mins);
        let maxs = whole_units_q12(self.entities[render_index].clip_maxs);
        // `ground_brush_entity` recovers Quake's `groundentity` from a hull
        // trace, but an exact world/mover plane tie can leave the world as the
        // winning trace even though the player's feet are on the mover.  The
        // original already has the ground entity from the physics pass.  Use
        // the same geometric resting contact we use for other carried bodies
        // as a deterministic fallback against the mover's pre-step deck.
        let previous_mins = subtract_vec(mins, delta);
        let previous_maxs = subtract_vec(maxs, delta);
        let player_standing = ground_entity == Some(render_index as u16)
            || rests_on(rider.body(), previous_mins, previous_maxs);
        let mut scratch = TraceScratch::default();
        // Resolved against `&self` and applied after: the composed collision
        // borrows the whole entity list, so nothing in it can be written while
        // the pass runs.
        let ledger = {
            // The pusher is taken out of the world for its own push, exactly
            // like the original's `pusher->v.solid = SOLID_NOT` around
            // `SV_PushEntity`.
            let Some(collision) = self.collision_without(map, render_index as u16) else {
                return None;
            };
            let bodies = self
                .entities
                .iter()
                .enumerate()
                .filter(|(index, entity)| *index != render_index && entity.visible)
                .filter_map(|(index, entity)| {
                    let runtime = entity.monster?;
                    runtime.body_solid().then(|| {
                        (
                            index as u16,
                            RiderBody::new(entity.origin, entity.hit_mins, entity.hit_maxs),
                        )
                    })
                });
            push_pass(
                &collision,
                &mut scratch,
                rider,
                player_standing,
                bodies,
                delta,
                mins,
                maxs,
            )
        };
        // A blocked pusher is rolled back whole, so nothing it moved may keep
        // its move either. The ledger hands out no moves at all in that case.
        if let Some(origin) = ledger.player_move() {
            rider.translate(subtract_vec(origin, rider.origin));
        }
        for (index, origin) in ledger.body_moves().iter().copied() {
            update_moving_alias_origin(
                map,
                &mut self.entities[usize::from(index)],
                origin,
                &styles,
            );
        }
        ledger.blocker()
    }

    /// Run a pusher's `blocked` damage against the participant that actually
    /// stopped the atomic step.
    ///
    /// Capacity exhaustion rolls the pusher back but names no victim: choosing
    /// the player there would fabricate damage unrelated to any collision.
    fn apply_pusher_crush_damage(
        &mut self,
        map: &ResidentMap,
        blocker: PushBlocker,
        damage: u16,
        result: &mut GameplayResult,
    ) {
        if damage == 0 {
            return;
        }
        match blocker {
            PushBlocker::Player => {
                result.crush_damage = result.crush_damage.saturating_add(damage);
            }
            PushBlocker::Body(index) => {
                let nightmare = self.nightmare();
                let Some(entity) = self.entities.get_mut(usize::from(index)) else {
                    return;
                };
                let mut applied = DamageResult::default();
                apply_entity_damage(
                    map,
                    nightmare,
                    entity,
                    damage.min(i16::MAX as u16) as i16,
                    DamageAttacker::World,
                    &mut applied,
                    &mut self.pending_scene_work,
                );
            }
            PushBlocker::Capacity => {}
        }
    }

    fn trace_point(
        &self,
        map: &ResidentMap,
        start: &Vec3I32,
        end: &Vec3I32,
        _scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let Some(world) = map.brush_models().get(0) else {
            return false;
        };
        let mut best = Trace::default();
        let mut render_scratch = RenderTraceScratch::default();
        if !trace_render_bsp_into(
            map.planes(),
            map.nodes(),
            map.leaves(),
            world.head_nodes[0],
            start,
            end,
            &mut render_scratch,
            &mut best,
        ) {
            return false;
        }
        for entity in &self.entities {
            if !entity.visible
                || !entity.solid
                || entity.model_id >= 0
                || !segment_overlaps_entity(*start, *end, entity)
            {
                continue;
            }
            let Some(model) = map.brush_models().get(entity.model_index as usize) else {
                return false;
            };
            let mut candidate = Trace::default();
            if !trace_translated_render_bsp_into(
                map.planes(),
                map.nodes(),
                map.leaves(),
                model.head_nodes[0],
                entity.origin,
                start,
                end,
                &mut render_scratch,
                &mut candidate,
            ) {
                return false;
            }
            if candidate.fraction < best.fraction
                || (candidate.start_solid && !best.start_solid)
                || (candidate.all_solid && !best.all_solid)
            {
                best = candidate;
            }
        }
        *output = best;
        true
    }

    #[cfg(any(feature = "monster-regression", feature = "monsterjump-regression"))]
    pub fn regression_monster_player_eye(
        &self,
        map: &ResidentMap,
        source_index: u16,
        distance: i32,
    ) -> Option<Vec3I32> {
        let entity = self
            .entities
            .iter()
            .find(|entity| entity.source_index == source_index && entity.monster.is_some())?;
        let target = Vec3I32 {
            x: entity.origin.x,
            y: entity.origin.y,
            z: entity.origin.z.saturating_add(25 << 12),
        };
        let offsets = [
            Vec3I32 {
                x: distance << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: -(distance << 12),
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: distance << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -(distance << 12),
                z: 0,
            },
        ];
        let mut scratch = TraceScratch::default();
        for offset in offsets {
            let eye = add_vec(target, offset);
            let mut trace = Trace::default();
            if self.trace_point(map, &eye, &target, &mut scratch, &mut trace)
                && !trace.start_solid
                && !trace.all_solid
                && trace.fraction == 1 << 12
            {
                return Some(eye);
            }
        }
        None
    }

    /// Runtime door-message handle for the E1M1 authored-chain gate.
    #[cfg(feature = "e1m1-chain-regression")]
    pub fn regression_mover_message(&self, source_index: u16) -> Option<u16> {
        self.movers.iter().find_map(|mover| {
            let entity = self.entities.get(mover.render_index as usize)?;
            (entity.source_index == source_index).then_some(mover.source.string)
        })
    }

    #[cfg(any(
        feature = "monster-regression",
        feature = "bestiary-regression",
        feature = "monsterjump-regression"
    ))]
    pub fn regression_monster_snapshot(
        &self,
        source_index: u16,
    ) -> Option<MonsterRegressionSnapshot> {
        let entity = self
            .entities
            .iter()
            .find(|entity| entity.source_index == source_index)?;
        let monster = entity.monster?;
        Some(MonsterRegressionSnapshot {
            origin: entity.origin,
            frame: entity.frame,
            state: monster.state(),
            model_id: entity.model_id,
            health: entity.health,
            active: monster.active(),
            corpse_finished: monster.corpse_finished(),
            leaping: monster.leaping(),
            forced_jump: monster.forced_jump(),
        })
    }

    /// Put the authored E1M6 ogre nearest the sole monster-jump brush into
    /// that brush for the focused guest oracle. The shipping trigger lookup,
    /// launch, flight, collision and landing paths remain untouched.
    #[cfg(feature = "monsterjump-regression")]
    pub fn regression_stage_monsterjump(&mut self, map: &ResidentMap) -> Option<(u16, Vec3I32)> {
        if map.map() != EpisodeMap::E1M6 {
            return None;
        }
        let trigger = map.entities().get(E1M6_MONSTERJUMP_SOURCE)?;
        if trigger.class_name != CLASS_TRIGGER_MONSTERJUMP {
            return None;
        }
        let (trigger_mins, trigger_maxs) = entity_brush_bounds(map, trigger)?;
        let trigger_center = midpoint_vec_all(trigger_mins, trigger_maxs);
        let mut selected = None;
        let mut best_distance = i32::MAX;
        for (index, entity) in self.entities.iter().enumerate() {
            let Some(monster) = entity.monster else {
                continue;
            };
            if monster.kind() != MonsterKind::Ogre || monster.dead() {
                continue;
            }
            let distance = distance_units(entity.origin, trigger_center);
            if distance < best_distance {
                selected = Some(index);
                best_distance = distance;
            }
        }
        let index = selected?;
        let original = self.entities[index].origin;
        let mut origin = original;
        // The focused boot has not opened the authored mover around this
        // six-unit brush. Put the leading face one unit across it only long
        // enough to prove the shipping touch path, then the oracle restores
        // `original` before the first flight step.
        let touch_y = trigger_maxs.y.saturating_sub(1 << 12);
        origin.y = origin
            .y
            .saturating_add(touch_y.saturating_sub(self.entities[index].hit_mins.y));
        let styles = self.light_styles;
        update_moving_alias_origin(map, &mut self.entities[index], origin, &styles)
            .then_some((self.entities[index].source_index, original))
    }

    #[cfg(feature = "monsterjump-regression")]
    pub fn regression_restore_monsterjump(
        &mut self,
        map: &ResidentMap,
        source_index: u16,
        origin: Vec3I32,
    ) -> bool {
        let Some(index) = self
            .entities
            .iter()
            .position(|entity| entity.source_index == source_index)
        else {
            return false;
        };
        let styles = self.light_styles;
        update_moving_alias_origin(map, &mut self.entities[index], origin, &styles)
    }

    #[cfg(feature = "monster-regression")]
    pub fn regression_damage_monster(
        &mut self,
        map: &ResidentMap,
        source_index: u16,
        damage: i16,
    ) -> Option<DamageResult> {
        let nightmare = self.nightmare();
        let entity = self
            .entities
            .iter_mut()
            .find(|entity| entity.source_index == source_index)?;
        let mut result = DamageResult::default();
        apply_entity_damage(
            map,
            nightmare,
            entity,
            damage,
            DamageAttacker::Player,
            &mut result,
            &mut self.pending_scene_work,
        );
        Some(result)
    }

    /// Nearest authored, skill-admitted monster of `class_name` to `from`,
    /// skipping the `skip` closest. The bestiary gate derives every waypoint
    /// from this, so no coordinate in its route is hand written.
    #[cfg(feature = "bestiary-regression")]
    pub fn regression_nearest_monster(
        &self,
        map: &ResidentMap,
        class_name: u8,
        from: Vec3I32,
        skip: u8,
    ) -> Option<(u16, Vec3I32)> {
        let mut used = [u16::MAX; 8];
        let mut skipped = 0u8;
        loop {
            let mut best: Option<(u16, Vec3I32, i32)> = None;
            for entity in &self.entities {
                if entity.monster.is_none() || used.contains(&entity.source_index) {
                    continue;
                }
                let source = map.entities().get(entity.source_index as usize)?;
                if source.class_name != class_name {
                    continue;
                }
                let distance = distance_units(from, entity.origin);
                if best.is_none_or(|(_, _, current)| distance < current) {
                    best = Some((entity.source_index, entity.origin, distance));
                }
            }
            let (source_index, origin, _) = best?;
            if skipped == skip {
                return Some((source_index, origin));
            }
            used[skipped as usize] = source_index;
            skipped += 1;
            if skipped as usize >= used.len() {
                return None;
            }
        }
    }

    #[cfg(any(feature = "combat-regression", feature = "arsenal-regression"))]
    pub fn regression_shot_setup(&self, map: &ResidentMap) -> Option<(u16, Vec3I32, Vec3I32)> {
        const OFFSETS: [Vec3I32; 12] = [
            Vec3I32 {
                x: -96 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 96 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -96 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 96 << 12,
                z: 0,
            },
            Vec3I32 {
                x: -128 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 128 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -128 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 128 << 12,
                z: 0,
            },
            Vec3I32 {
                x: -64 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 64 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -64 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 64 << 12,
                z: 0,
            },
        ];
        self.regression_shot_setup_from(map, &OFFSETS)
    }

    /// Exercise two authored E1M1 contracts through the shipping paths: a
    /// splash-killed `trigger_multiple` and a live solid `misc_explobox`.
    #[cfg(feature = "combat-regression")]
    pub fn regression_authored_combat_brushes(&mut self, map: &ResidentMap) -> (u32, u16, u16) {
        let mut flags = 0u32;
        let mut explobox_source = 0u16;
        if let Some(explobox) = self.entities.iter().find(|entity| {
            map.entities()
                .get(entity.source_index as usize)
                .is_some_and(|source| is_explobox(source.class_name))
        }) {
            flags |= 1;
            explobox_source = explobox.source_index;
            if dynamic_body(explobox).is_some_and(|body| !body.dead) {
                flags |= 1 << 1;
            }
        }
        let Some((trigger_index, trigger)) = self
            .triggers
            .iter()
            .enumerate()
            .find(|(_, trigger)| trigger.multi.takes_damage())
        else {
            return (flags, 0, explobox_source);
        };
        flags |= 1 << 2;
        let trigger_source = trigger.source_index;
        let impact = midpoint_vec_all(trigger.mins, trigger.maxs);
        let mut weapon = WeaponState::new();
        let mut result = RocketResult::default();
        if self
            .apply_rocket_impact(
                map,
                impact,
                0,
                120,
                None,
                false,
                None,
                DamageAttacker::Player,
                &mut weapon,
                &mut result,
            )
            .is_some()
        {
            flags |= 1 << 3;
        }
        if !self.triggers[trigger_index].multi.takes_damage() {
            flags |= 1 << 4;
        }
        if result.killed_targets != 0 {
            flags |= 1 << 5;
        }
        if result.last_source_index == Some(trigger_source) {
            flags |= 1 << 6;
        }
        (flags, trigger_source, explobox_source)
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_arsenal_shot_setup(
        &self,
        map: &ResidentMap,
    ) -> Option<(u16, Vec3I32, Vec3I32)> {
        // Prefer a shot which survives at least one four-tick catch-up step
        // while remaining close enough to prove player splash damage.
        const OFFSETS: [Vec3I32; 12] = [
            Vec3I32 {
                x: -128 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 128 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -128 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 128 << 12,
                z: 0,
            },
            Vec3I32 {
                x: -112 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 112 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -112 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 112 << 12,
                z: 0,
            },
            Vec3I32 {
                x: -96 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 96 << 12,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -96 << 12,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: 96 << 12,
                z: 0,
            },
        ];
        // The arsenal fires every weapon at one authored target. Picking the
        // first damageable entity made this probe depend on source order: a
        // low-health monster could die to the two shotguns before the nails
        // and grenade had a chance to prove their damage paths. Prefer the
        // healthiest visible target with a clear authored firing position.
        let mut scratch = TraceScratch::default();
        let mut best = None;
        for entity in &self.entities {
            if !entity.visible || !entity.damageable || entity.health <= 0 {
                continue;
            }
            let target = midpoint_vec(entity.hit_mins, entity.hit_maxs);
            for &offset in &OFFSETS {
                let eye = add_vec(target, offset);
                let mut trace = Trace::default();
                if self.trace_point(map, &eye, &target, &mut scratch, &mut trace)
                    && !trace.start_solid
                    && !trace.all_solid
                    && trace.fraction == 1 << 12
                {
                    if best.is_none_or(|(health, _, _, _)| entity.health > health) {
                        best = Some((entity.health, entity.source_index, eye, target));
                    }
                    break;
                }
            }
        }
        best.map(|(_, source, eye, target)| (source, eye, target))
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_offset_eye(
        &self,
        map: &ResidentMap,
        eye: Vec3I32,
        target: Vec3I32,
    ) -> Option<Vec3I32> {
        let lateral = if eye.x != target.x {
            Vec3I32 {
                x: 0,
                y: 16 << 12,
                z: 0,
            }
        } else {
            Vec3I32 {
                x: 16 << 12,
                y: 0,
                z: 0,
            }
        };
        let mut scratch = TraceScratch::default();
        for candidate in [add_vec(eye, lateral), subtract_vec(eye, lateral)] {
            let mut side = Trace::default();
            let mut sight = Trace::default();
            if self.trace_point(map, &eye, &candidate, &mut scratch, &mut side)
                && !side.start_solid
                && !side.all_solid
                && side.fraction == 1 << 12
                && self.trace_point(map, &candidate, &target, &mut scratch, &mut sight)
                && !sight.start_solid
                && !sight.all_solid
                && sight.fraction == 1 << 12
            {
                return Some(candidate);
            }
        }
        None
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_fill_nail_pool(&mut self, map: &ResidentMap, origin: Vec3I32) -> bool {
        self.regression_clear_nails();
        let spawn = NailSpawn {
            origin,
            step: Vec3I32::default(),
            lifetime_ticks: quake_core::combat::NAIL_LIFETIME_TICKS,
            damage: quake_core::combat::NAIL_DAMAGE,
            sound_id: 0xcc,
        };
        let mut count = 0usize;
        while count < NAIL_POOL_CAPACITY {
            if !self.spawn_nail(map, spawn) {
                self.regression_clear_nails();
                return false;
            }
            count += 1;
        }
        true
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_nail_count(&self) -> usize {
        self.nails.iter().filter(|slot| slot.is_some()).count()
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_clear_nails(&mut self) {
        self.nails = [None; NAIL_POOL_CAPACITY];
        let start = self.nail_render_start as usize;
        let end = start.saturating_add(NAIL_POOL_CAPACITY);
        for render in self.entities.get_mut(start..end).into_iter().flatten() {
            render.visible = false;
        }
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_wall_lightning_probe(
        &mut self,
        map: &ResidentMap,
        origin: Vec3I32,
    ) -> Option<(u32, Vec3I32)> {
        const Q12_ONE: i32 = 1 << 12;
        const DIRECTIONS: [(Vec3I32, Vec3I32); 6] = [
            (
                Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: 0,
                    y: Q12_ONE,
                    z: 0,
                },
            ),
            (
                Vec3I32 {
                    x: -Q12_ONE,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: 0,
                    y: Q12_ONE,
                    z: 0,
                },
            ),
            (
                Vec3I32 {
                    x: 0,
                    y: Q12_ONE,
                    z: 0,
                },
                Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                },
            ),
            (
                Vec3I32 {
                    x: 0,
                    y: -Q12_ONE,
                    z: 0,
                },
                Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                },
            ),
            (
                Vec3I32 {
                    x: 0,
                    y: 0,
                    z: Q12_ONE,
                },
                Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                },
            ),
            (
                Vec3I32 {
                    x: 0,
                    y: 0,
                    z: -Q12_ONE,
                },
                Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                },
            ),
        ];
        for (forward, right) in DIRECTIONS {
            let attack = LightningAttack {
                beam_start: origin,
                start: origin,
                end: Vec3I32 {
                    x: origin.x.saturating_add(forward.x.saturating_mul(600)),
                    y: origin.y.saturating_add(forward.y.saturating_mul(600)),
                    z: origin.z.saturating_add(forward.z.saturating_mul(600)),
                },
                forward,
                right,
                damage: quake_core::combat::LIGHTNING_DAMAGE,
                sound_id: None,
            };
            let result = self.fire_lightning(map, attack)?;
            if !result.world_clipped {
                continue;
            }
            let beam = self.lightning_beam?;
            let expected_side = Vec3I32 {
                x: result
                    .trace_end
                    .x
                    .saturating_add(right.x.saturating_mul(16)),
                y: result
                    .trace_end
                    .y
                    .saturating_add(right.y.saturating_mul(16)),
                z: result
                    .trace_end
                    .z
                    .saturating_add(right.z.saturating_mul(16)),
            };
            let mut flags = 1;
            if beam.start == origin && beam.end == result.trace_end {
                flags |= 1 << 1;
            }
            if result.trace_end != attack.end && result.side_end == expected_side {
                flags |= 1 << 2;
            }
            return Some((flags, forward));
        }
        None
    }

    #[cfg(any(feature = "combat-regression", feature = "arsenal-regression"))]
    fn regression_shot_setup_from(
        &self,
        map: &ResidentMap,
        offsets: &[Vec3I32],
    ) -> Option<(u16, Vec3I32, Vec3I32)> {
        let mut scratch = TraceScratch::default();
        for entity in &self.entities {
            if !entity.visible || !entity.damageable || entity.health <= 0 {
                continue;
            }
            let target = midpoint_vec(entity.hit_mins, entity.hit_maxs);
            for &offset in offsets {
                let eye = add_vec(target, offset);
                let mut trace = Trace::default();
                if self.trace_point(map, &eye, &target, &mut scratch, &mut trace)
                    && !trace.start_solid
                    && !trace.all_solid
                    && trace.fraction == 1 << 12
                {
                    return Some((entity.source_index, eye, target));
                }
            }
        }
        None
    }

    #[cfg(any(feature = "combat-regression", feature = "arsenal-regression"))]
    pub fn regression_monster_health(&self, source_index: u16) -> Option<i16> {
        self.entities
            .iter()
            .find(|entity| entity.source_index == source_index)
            .map(|entity| entity.health)
    }

    #[cfg(feature = "arsenal-regression")]
    pub fn regression_pickup_origin(
        &self,
        map: &ResidentMap,
        class_name: u8,
    ) -> Option<(u16, Vec3I32)> {
        self.entities.iter().find_map(|entity| {
            let source = entity.source_index as usize;
            (entity.visible
                && entity.pickup.is_some()
                && map.entities().get(source)?.class_name == class_name)
                .then_some((entity.source_index, entity.origin))
        })
    }

    /// Return the destination of the first change-level volume touched by the
    /// player's Quake hull.
    #[optimize(size)]
    pub fn touched_change_level(
        &self,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
    ) -> Option<EpisodeMap> {
        self.touched_change_level_trigger(player_mins, player_maxs)
            .map(|(destination, _, _)| destination)
    }

    /// The first change-level volume the player's Quake hull is inside: where
    /// it leads, and `changelevel_touch`'s `NO_INTERMISSION` spawnflag, which
    /// loads that map straight away instead of raising the panel first.
    #[optimize(size)]
    pub fn touched_change_level_trigger(
        &self,
        player_mins: Vec3I32,
        player_maxs: Vec3I32,
    ) -> Option<(EpisodeMap, bool, u16)> {
        let mut index = 0usize;
        while index < self.change_level_count {
            let trigger = self.change_levels[index].expect("populated change-level slot");
            if self.targets.is_enabled(trigger.source_index)
                && aabb_overlaps(player_mins, player_maxs, trigger.mins, trigger.maxs)
            {
                return Some((
                    trigger.destination,
                    trigger.no_intermission,
                    trigger.source_index,
                ));
            }
            index += 1;
        }
        None
    }

    /// `changelevel_touch`'s `SUB_UseTargets`, before the intermission begins.
    ///
    /// E1M7's exit targets a relay which fans out into the authored finale
    /// timing chain. The level transition wins before those delayed uses can
    /// mature, just as the original schedules `execute_changelevel` for 0.1
    /// seconds later, but the target graph must still receive and schedule
    /// every edge. The touch function is removed after its first fire.
    #[optimize(size)]
    pub fn fire_change_level_targets(
        &mut self,
        map: &ResidentMap,
        source_index: u16,
        result: &mut GameplayResult,
    ) -> u16 {
        let before = result.fired_target_edges;
        let sources = map.entities();
        let mut actions = TargetActions::new();
        if let Err(error) = self.targets.fire_source_by(
            &sources,
            source_index,
            TargetActivator::Player,
            &mut actions,
        ) {
            result.target_error.get_or_insert(error);
        }
        self.apply_target_actions(map, &mut actions, result);
        if let Err(error) = self.targets.disable_entity(source_index) {
            result.target_error.get_or_insert(error);
        }
        result.fired_target_edges.saturating_sub(before)
    }

    /// Number of live scene entities cooked from a given class.
    #[cfg(feature = "episode1-route-regression")]
    pub fn regression_class_present(&self, map: &ResidentMap, class_name: u8) -> u16 {
        let sources = map.entities();
        self.entities
            .iter()
            .filter(|entity| {
                sources
                    .get(entity.source_index as usize)
                    .is_some_and(|source| source.class_name == class_name)
            })
            .count()
            .min(u16::MAX as usize) as u16
    }

    /// True when a rune gate of the given class and rune bit both spawned and
    /// presents a solid brush to the player.
    #[cfg(feature = "episode1-route-regression")]
    pub fn regression_solid_gate(&self, map: &ResidentMap, class_name: u8, rune: u16) -> bool {
        let sources = map.entities();
        self.entities.iter().any(|entity| {
            entity.solid
                && sources
                    .get(entity.source_index as usize)
                    .is_some_and(|source| {
                        source.class_name == class_name && source.spawn_flags & 0x0f == rune
                    })
        })
    }

    /// Whether every mover the cooked map names `target_name` has finished
    /// its travel, or `None` when the map names no mover at all.
    ///
    /// A route that walks onto geometry a trigger chain builds has to know
    /// the build finished. E1M7's lava bridge is the case that needs it: its
    /// two halves are `DOOR_START_OPEN`, so they only become a floor once
    /// they have travelled all the way back to the brush they were authored
    /// in, and stepping off the ring before then is a fall into the lava.
    #[cfg(feature = "episode1-route-regression")]
    pub fn regression_movers_settled(&self, map: &ResidentMap, target_name: u16) -> Option<bool> {
        if target_name == 0 {
            return None;
        }
        let sources = map.entities();
        let mut named = false;
        let mut settled = true;
        for mover in &self.movers {
            let Some(entity) = self.entities.get(mover.render_index as usize) else {
                continue;
            };
            let Some(source) = sources.get(entity.source_index as usize) else {
                continue;
            };
            if source.target_name != target_name {
                continue;
            }
            named = true;
            settled &= mover.policy.state() == QuakeMoverState::Top;
        }
        named.then_some(settled)
    }

    /// Read-only mover state for authored route gates. The gate still has to
    /// earn every transition through ordinary input; this only lets it assert
    /// that a reached mechanism settled in the state Quake authored.
    #[cfg(feature = "e1m2-e1m3-route-regression")]
    pub fn regression_route_mover_state(&self, source_index: u16) -> Option<QuakeMoverState> {
        self.movers
            .iter()
            .find(|mover| {
                self.entities
                    .get(mover.render_index as usize)
                    .is_some_and(|entity| entity.source_index == source_index)
            })
            .map(|mover| mover.policy.state())
    }

    /// Chthon's live runtime state.
    #[cfg(feature = "episode1-route-regression")]
    pub fn regression_boss(&self, map: &ResidentMap) -> Option<BossSnapshot> {
        let sources = map.entities();
        self.entities.iter().find_map(|entity| {
            let runtime = entity.monster?;
            if !runtime.kind().is_boss() {
                return None;
            }
            let _ = sources.get(entity.source_index as usize)?;
            Some(BossSnapshot {
                frame: entity.frame,
                shocks: runtime.boss_shocks(),
                active: runtime.active(),
                dead: runtime.dead(),
                visible: entity.visible,
                throwing: runtime.state() == quake_core::monster::MonsterState::Missile,
            })
        })
    }

    /// Return a point inside a real cooked change-level volume. This is only
    /// present in the emulator regression image; shipping gameplay must reach
    /// the volume through normal player movement.
    #[cfg(any(
        feature = "episode1-regression",
        feature = "episode1-route-regression",
        feature = "arsenal-regression"
    ))]
    pub fn regression_change_level_origin(&self, destination: EpisodeMap) -> Option<Vec3I32> {
        let trigger = self.change_levels[..self.change_level_count]
            .iter()
            .flatten()
            .find(|trigger| trigger.destination == destination)?;
        Some(Vec3I32 {
            x: midpoint(trigger.mins.x, trigger.maxs.x),
            y: midpoint(trigger.mins.y, trigger.maxs.y),
            z: midpoint(trigger.mins.z, trigger.maxs.z),
        })
    }

    /// Match Quake's four-second bonus-item rotation in its 4096-unit turn.
    pub fn rotating_yaw(&self) -> i16 {
        (self.frame.wrapping_mul(34) & 0x0fff) as i16
    }
}

const fn brush_model_is_visible(class_name: u8) -> bool {
    matches!(
        class_name,
        CLASS_FUNC_BOSSGATE
            | CLASS_FUNC_EPISODEGATE
            | 0x0b // func_button
            | 0x0c // func_door
            | 0x0d // func_door_secret
            | 0x0f // func_illusionary
            | 0x10 // func_plat
            | 0x11 // func_train
            | 0x12 // func_wall
            | 0x35 // misc_teleporttrain
    )
}

const fn is_explobox(class_name: u8) -> bool {
    matches!(class_name, CLASS_MISC_EXPLOBOX | CLASS_MISC_EXPLOBOX2)
}

/// `misc_explobox` / `misc_explobox2`: `SOLID_BBOX`, `setsize '0 0 0' '32 32 64'`,
/// `health = 20`, `th_die = barrel_explode`. The barrel is a brush model, so
/// its origin is the box's low corner rather than its centre.
const fn explobox_profile(class_name: u8) -> Option<MonsterProfile> {
    if !is_explobox(class_name) {
        return None;
    }
    Some(MonsterProfile {
        mins: Vec3I16 { x: 0, y: 0, z: 0 },
        maxs: Vec3I16 {
            x: 32,
            y: 32,
            z: 64,
        },
        health: 20,
    })
}

const fn brush_model_is_solid(class_name: u8) -> bool {
    matches!(
        class_name,
        CLASS_FUNC_BOSSGATE | CLASS_FUNC_EPISODEGATE | 0x0b | 0x0c | 0x0d | 0x10 | 0x11 | 0x12
    )
}

const fn touch_trigger(class_name: u8) -> bool {
    matches!(
        class_name,
        0x4b | 0x4c | 0x50 | CLASS_TRIGGER_ONLY_REGISTERED | CLASS_TRIGGER_SETSKILL
    )
}

/// `trigger_multiple`'s `sounds` selector. `trigger_secret` defaults to 1.
///
/// `sounds 3` selects misc/trigger1, which is not in the cooked resource list,
/// so those triggers stay silent.
const fn trigger_noise(class_name: u8, sounds: i8) -> Option<i16> {
    // `trigger_onlyregistered_touch` answers with misc/talk in shareware.
    if class_name == CLASS_TRIGGER_ONLY_REGISTERED {
        return Some(0x7b);
    }
    let sounds = if class_name == 0x50 && sounds == 0 {
        1
    } else {
        sounds
    };
    match sounds {
        1 => Some(0x7a),
        2 => Some(0x7b),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SceneMover {
    render_index: u16,
    /// The authored fields the per-tick mover loop reads, copied from the
    /// cooked record at load.
    source: MoverSource,
    policy: QuakeMover,
    activator: TargetActivator,
    /// `LinkDoors` chain id. Every mover starts in its own group and only
    /// touching, linkable `func_door` bodies are merged.
    link_group: u8,
    key_cooldown: u16,
    key_spent: bool,
    crush: BlockCrush,
    /// `func_button`'s authored `health`. Non-zero means the original spawn
    /// gave it `th_die = button_killed` and `takedamage = DAMAGE_YES` INSTEAD
    /// of a touch function, so it is shot open and cannot be walked into.
    /// `button_killed` hands the health straight back, so a re-usable button
    /// can be shot again once it has returned.
    health: i16,
    max_health: i16,
    /// A kill this frame, consumed by the mover loop exactly where a touch
    /// would have been.
    shot_open: bool,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct MoverSource {
    class_name: u8,
    noise: i8,
    spawn_flags: u16,
    target_name: u16,
    string: u16,
    damage: i16,
}

impl MoverSource {
    fn from_entity(source: &MapEntity) -> Self {
        Self {
            class_name: source.class_name,
            noise: source.noise,
            spawn_flags: source.spawn_flags,
            target_name: source.target_name,
            string: source.string,
            damage: source.damage,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SceneTrain {
    render_index: u16,
    policy: QuakeTrain,
    crush: BlockCrush,
}

/// A brush entity a shot can hurt. Both are `SOLID_BBOX` in the original, so
/// both stop the shot; which one a segment reaches first decides who takes it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ShotBrush {
    /// Index into `self.triggers`.
    Trigger(usize),
    /// Index into `self.movers`.
    Button(usize),
}

#[derive(Copy, Clone, Debug)]
struct Trigger {
    source_index: u16,
    mins: Vec3I32,
    maxs: Vec3I32,
    once: bool,
    armed: bool,
    cooldown: u16,
    wait_ticks: u16,
    /// `trigger_multiple`'s shootable half, from the cooked `health` key.
    multi: MultiTrigger,
}

#[derive(Copy, Clone, Debug)]
struct TeleportTrigger {
    source_index: u16,
    /// Authored `spawnflags`, copied at load for the per-tick gate test.
    spawn_flags: u16,
    mins: Vec3I32,
    maxs: Vec3I32,
    gate: TeleportGate,
    cooldown: u16,
}

/// `func_button`'s authored `health`, and only a button's. The spawn rule
/// itself lives in `quake_core::mover` so it is host-tested: E1M2, E1M3 and
/// E1M4 are the shareware maps that author a shootable one, all at health 1.
#[optimize(size)]
fn button_health(source: MapEntity) -> i16 {
    if quake_core::mover::button_is_shootable(source.class_name, source.health) {
        source.health
    } else {
        0
    }
}

#[optimize(size)]
fn entity_brush_bounds(map: &ResidentMap, source: MapEntity) -> Option<(Vec3I32, Vec3I32)> {
    let model_index = source.model.checked_neg()? as usize;
    let model = map.brush_models().get(model_index)?;
    Some((
        bounds_q12(source.origin, model.mins),
        bounds_q12(source.origin, model.maxs),
    ))
}

const fn grown_whole_units(bounds: [i16; 3], margin: i32) -> Vec3I32 {
    Vec3I32 {
        x: bounds[0] as i32 + margin,
        y: bounds[1] as i32 + margin,
        z: bounds[2] as i32 + margin,
    }
}

const fn whole_units_q12(bounds: [i16; 3]) -> Vec3I32 {
    Vec3I32 {
        x: (bounds[0] as i32) << 12,
        y: (bounds[1] as i32) << 12,
        z: (bounds[2] as i32) << 12,
    }
}

fn bounds_q12(origin: Vec3I32, bounds: Vec3I16) -> Vec3I32 {
    Vec3I32 {
        x: origin.x.saturating_add(i32::from(bounds.x) << 12),
        y: origin.y.saturating_add(i32::from(bounds.y) << 12),
        z: origin.z.saturating_add(i32::from(bounds.z) << 12),
    }
}

#[optimize(size)]
fn fixed_seconds_to_ticks(value: i32, default_ticks: u16) -> u16 {
    if value <= 0 {
        default_ticks
    } else {
        let whole_seconds = value >> 12;
        let fractional_seconds = value & 0x0fff;
        let ticks = whole_seconds * 60 + (fractional_seconds * 60 >> 12);
        ticks.clamp(1, i32::from(u16::MAX)) as u16
    }
}

/// The union of every [`pickup_touch_bounds`] box: `[-16, -16, -24]` to
/// `[32, 32, 56]` around the origin plus the touch slack, with the same
/// saturating translation, so each class's box is contained in it whatever
/// the class.
fn pickup_touch_hull(origin: Vec3I32) -> (Vec3I32, Vec3I32) {
    (
        Vec3I32 {
            x: origin.x.saturating_add((-16 - PICKUP_TOUCH_SLACK_XY) << 12),
            y: origin.y.saturating_add((-16 - PICKUP_TOUCH_SLACK_XY) << 12),
            z: origin.z.saturating_add((-24 - PICKUP_TOUCH_SLACK_Z) << 12),
        },
        Vec3I32 {
            x: origin.x.saturating_add((32 + PICKUP_TOUCH_SLACK_XY) << 12),
            y: origin.y.saturating_add((32 + PICKUP_TOUCH_SLACK_XY) << 12),
            z: origin.z.saturating_add((56 + PICKUP_TOUCH_SLACK_Z) << 12),
        },
    )
}

/// `SV_LinkEdict` widens every `FL_ITEM` entity's absolute box by 15 units in
/// x and y ("to make items easier to pick up") and the moving player's own
/// box by one unit on all axes, and `SV_TouchLinks` compares those absolute
/// boxes. Folded into the item side of the test here: 16 units of horizontal
/// slack and one vertical, which is what lets a jump reach an item on a ledge
/// the player cannot stand on.
const PICKUP_TOUCH_SLACK_XY: i32 = 15 + 1;
const PICKUP_TOUCH_SLACK_Z: i32 = 1;

fn pickup_touch_bounds(class_name: u8, origin: Vec3I32) -> (Vec3I32, Vec3I32) {
    let (mins, maxs) = match class_name {
        0x21 | 0x22 | 0x25 | 0x26 | 0x28 => ([0, 0, 0], [32, 32, 56]),
        // Keys, the four artifacts, and the runes share `setsize
        // '-16 -16 -24', '16 16 32'` in the original spawn functions.
        0x1d | 0x1e | 0x1f | 0x20 | 0x23 | 0x24 | CLASS_ITEM_SIGIL => {
            ([-16, -16, -24], [16, 16, 32])
        }
        _ => ([-16, -16, 0], [16, 16, 56]),
    };
    let translate = |bounds: [i32; 3], slack_xy: i32, slack_z: i32| Vec3I32 {
        x: origin.x.saturating_add((bounds[0] + slack_xy) << 12),
        y: origin.y.saturating_add((bounds[1] + slack_xy) << 12),
        z: origin.z.saturating_add((bounds[2] + slack_z) << 12),
    };
    (
        translate(mins, -PICKUP_TOUCH_SLACK_XY, -PICKUP_TOUCH_SLACK_Z),
        translate(maxs, PICKUP_TOUCH_SLACK_XY, PICKUP_TOUCH_SLACK_Z),
    )
}

fn update_projectile_render(
    map: &ResidentMap,
    render: &mut RenderEntity,
    origin: Vec3I32,
    step: Vec3I32,
    styles: &[u16; lightstyle::DUMMY_STYLE + 1],
) -> bool {
    let Some(model) = map.alias_models().model_at(render.model_index as usize) else {
        return false;
    };
    if model.header().id != render.model_id {
        return false;
    }
    let Some(leaf_index) = map.point_leaf_index(origin) else {
        return false;
    };
    let Some(leaf) = map.leaves().get(leaf_index) else {
        return false;
    };
    let light = leaf_light(leaf.lightmap, leaf.light_styles, styles);
    // Fast, narrow alias models need a conservative culling sphere between
    // fixed physics samples. Collision still uses the traced point segment.
    let clip_radius = model_clip_radius(model.header()).max(PROJECTILE_CLIP_RADIUS_UNITS);
    let (clip_mins, clip_maxs) = alias_clip_bounds(origin, clip_radius);
    let dx = step.x >> 12;
    let dy = step.y >> 12;
    let dz = step.z >> 12;
    let horizontal = isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));
    render.origin = origin;
    render.angles = Vec3I16 {
        x: atan2_q12(-dz, horizontal) as i16,
        y: atan2_q12(dy, dx) as i16,
        z: 0,
    };
    render.clip_mins = clip_mins;
    render.clip_maxs = clip_maxs;
    render.leaf_index = leaf_index.min(u16::MAX as usize) as u16;
    render.light = light;
    render.visible = true;
    true
}

fn update_moving_alias_origin(
    map: &ResidentMap,
    render: &mut RenderEntity,
    origin: Vec3I32,
    styles: &[u16; lightstyle::DUMMY_STYLE + 1],
) -> bool {
    let Some(model) = map.alias_models().model_at(render.model_index as usize) else {
        return false;
    };
    let Some(leaf_index) = map.point_leaf_index(origin) else {
        return false;
    };
    let Some(leaf) = map.leaves().get(leaf_index) else {
        return false;
    };
    let delta = Vec3I32 {
        x: origin.x.saturating_sub(render.origin.x),
        y: origin.y.saturating_sub(render.origin.y),
        z: origin.z.saturating_sub(render.origin.z),
    };
    render.origin = origin;
    render.hit_mins = add_vec(render.hit_mins, delta);
    render.hit_maxs = add_vec(render.hit_maxs, delta);
    (render.clip_mins, render.clip_maxs) =
        alias_clip_bounds(origin, model_clip_radius(model.header()));
    render.leaf_index = leaf_index.min(u16::MAX as usize) as u16;
    render.light = leaf_light(leaf.lightmap, leaf.light_styles, styles);
    true
}

fn set_alias_model(map: &ResidentMap, render: &mut RenderEntity, model_id: i16) -> bool {
    let models = map.alias_models();
    let Some(model_index) = (0..models.len()).find(|&index| {
        models
            .model_at(index)
            .is_some_and(|model| model.header().id == model_id)
    }) else {
        return false;
    };
    if model_index > u8::MAX as usize {
        return false;
    }
    let Some(model) = models.model_at(model_index) else {
        return false;
    };
    render.model_id = model_id;
    render.model_index = model_index as u8;
    render.frame = 0;
    render.animation_start = 0;
    render.animation_end = 0;
    (render.clip_mins, render.clip_maxs) =
        alias_clip_bounds(render.origin, model_clip_radius(model.header()));
    render.visible = true;
    true
}

fn aim_basis(from: Vec3I32, to: Vec3I32) -> (Vec3I32, Vec3I32, Vec3I32) {
    let dx = (to.x.saturating_sub(from.x)) >> 12;
    let dy = (to.y.saturating_sub(from.y)) >> 12;
    let dz = (to.z.saturating_sub(from.z)) >> 12;
    let horizontal = isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));
    let pitch = atan2_q12(-dz, horizontal) as u16 & 0x0fff;
    let yaw = atan2_q12(dy, dx) as u16 & 0x0fff;
    let sp = sin_q12(pitch);
    let cp = cos_q12(pitch);
    let sy = sin_q12(yaw);
    let cy = cos_q12(yaw);
    let forward = Vec3I32 {
        x: mul_q12_i32(cp, cy),
        y: mul_q12_i32(cp, sy),
        z: -sp,
    };
    let right = Vec3I32 {
        x: sy,
        y: -cy,
        z: 0,
    };
    let up = Vec3I32 {
        x: mul_q12_i32(sp, cy),
        y: mul_q12_i32(sp, sy),
        z: cp,
    };
    (forward, right, up)
}

fn target_in_front(origin: Vec3I32, yaw: i16, target: Vec3I32) -> bool {
    let dx = (target.x.saturating_sub(origin.x)) >> 12;
    let dy = (target.y.saturating_sub(origin.y)) >> 12;
    let distance = isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));
    if distance == 0 {
        return true;
    }
    let angle = yaw as u16 & 0x0fff;
    let dot = dx
        .saturating_mul(cos_q12(angle))
        .saturating_add(dy.saturating_mul(sin_q12(angle)));
    dot > distance.saturating_mul(1_228)
}

/// The Q20.12 offset a leap covers in `ticks`, from a velocity in units per
/// second and the yaw the monster launched with.
fn leap_step(leap: MonsterLeap, yaw: i16, ticks: u16) -> Vec3I32 {
    let heading = yaw as u16 & 0x0fff;
    let ticks = i32::from(ticks.min(4));
    let forward = i32::from(leap.forward);
    Vec3I32 {
        x: cos_q12(heading)
            .saturating_mul(forward)
            .saturating_mul(ticks)
            / 60,
        y: sin_q12(heading)
            .saturating_mul(forward)
            .saturating_mul(ticks)
            / 60,
        z: (i32::from(leap.up) << 12).saturating_mul(ticks) / 60,
    }
}

fn dog_leap_height_ok(
    monster_mins: Vec3I32,
    monster_maxs: Vec3I32,
    player_mins: Vec3I32,
    player_maxs: Vec3I32,
) -> bool {
    let player_height = player_maxs.z.saturating_sub(player_mins.z);
    monster_mins.z
        <= player_mins
            .z
            .saturating_add(player_height.saturating_mul(3) / 4)
        && monster_maxs.z >= player_mins.z.saturating_add(player_height / 4)
}

/// `t_movetarget` fires when the monster's box touches the corner's 16-unit
/// box. Every authored monster is at least 32 wide and spans -24..40 up, so
/// the touch is a fixed distance test rather than a per-kind box overlap.
fn path_corner_reached(origin: Vec3I32, corner: Vec3I32) -> bool {
    let axis = |a: i32, b: i32| (a.saturating_sub(b) >> 12).abs();
    axis(origin.x, corner.x) <= 24
        && axis(origin.y, corner.y) <= 24
        && axis(origin.z, corner.z) <= 48
}

/// A corner's authored `wait`, Q20.12 seconds, in 60 Hz ticks.
fn path_corner_wait_ticks(wait: i32) -> u16 {
    if wait <= 0 {
        return 0;
    }
    ((wait >> 12).saturating_mul(60) + (((wait & 0x0fff) * 60) >> 12)).clamp(0, i32::from(u16::MAX))
        as u16
}

fn turn_toward_yaw(current: i16, target: i16, maximum: i16) -> i16 {
    let current = i32::from(current) & 0x0fff;
    let target = i32::from(target) & 0x0fff;
    let mut delta = (target - current) & 0x0fff;
    if delta > 2_048 {
        delta -= 4_096;
    }
    let step = delta.clamp(-i32::from(maximum), i32::from(maximum));
    ((current + step) & 0x0fff) as i16
}

#[inline(never)]
fn alias_clip_bounds(origin: Vec3I32, clip_radius: i16) -> ([i16; 3], [i16; 3]) {
    let center = [origin.x >> 12, origin.y >> 12, origin.z >> 12];
    let mins = center.map(|axis| {
        axis.saturating_sub(clip_radius as i32)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16
    });
    let maxs = center.map(|axis| {
        axis.saturating_add(clip_radius as i32)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16
    });
    (mins, maxs)
}

/// `T_Damage`'s `attacker`, as far as a monster's retargeting cares.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DamageAttacker {
    /// The world, a barrel, a crusher, a telefrag: nobody retargets.
    World,
    Player,
    /// Another monster, by scene entity index and cooked class.
    Monster {
        index: u16,
        class_name: u8,
    },
}

#[optimize(size)]
#[inline(never)]
fn apply_entity_damage(
    map: &ResidentMap,
    nightmare: bool,
    entity: &mut RenderEntity,
    damage: i16,
    attacker: DamageAttacker,
    result: &mut DamageResult,
    scene_work: &mut bool,
) {
    let damage = damage.max(0);
    if damage == 0 || !entity.damageable || entity.health <= 0 {
        return;
    }
    entity.health = entity.health.saturating_sub(damage);
    result.damaged_targets = result.damaged_targets.saturating_add(1);
    result.total_damage = result.total_damage.saturating_add(damage as u16);
    result.last_source_index = Some(entity.source_index);
    let class_name = map
        .entities()
        .get(entity.source_index as usize)
        .unwrap_or_default()
        .class_name;
    if let Some(mut monster) = entity.monster {
        let transition = monster.take_damage(damage, entity.health, nightmare);
        // `T_Damage`: a monster hit by something other than its enemy turns
        // on the attacker, unless it is another monster of its own class
        // (soldiers excepted, which do fight each other).
        if !transition.killed {
            match attacker {
                DamageAttacker::World => {}
                DamageAttacker::Player => {
                    if monster.enemy() != MonsterEnemy::Player {
                        monster.set_enemy(MonsterEnemy::Player);
                    }
                }
                DamageAttacker::Monster {
                    index,
                    class_name: attacker_class,
                } => {
                    if monster.enemy() != MonsterEnemy::Monster(index)
                        && (attacker_class != class_name || class_name == CLASS_ARMY)
                    {
                        monster.set_enemy(MonsterEnemy::Monster(index));
                    }
                }
            }
        }
        entity.monster = Some(monster);
        entity.frame = transition.frame;
        result.response_sound = transition.sound_id.map(|sound| {
            SoundEvent::at(sound, entity.origin).on(entity.source_index, crate::audio::CHAN_VOICE)
        });
        if transition.reset_health {
            entity.health = entity.max_health;
        }
        if transition.killed {
            entity.damageable = false;
            result.killed_targets = result.killed_targets.saturating_add(1);
            if transition.gibbed {
                // `ThrowHead`: the corpse becomes the flying remains; the
                // scene throws the three gib chunks on its next pass.
                entity.pending_gib = true;
                *scene_work = true;
                entity.origin.z = entity.origin.z.saturating_add(24 << 12);
                if !monster
                    .kind()
                    .gib_head_model_id()
                    .is_some_and(|model_id| set_alias_model(map, entity, model_id))
                {
                    entity.visible = false;
                }
                entity.hit_mins = entity.origin;
                entity.hit_maxs = entity.origin;
            }
        }
        return;
    }
    if entity.health <= 0 && is_explobox(class_name) {
        // `barrel_explode` runs as `th_die`: the box stops taking damage at
        // once and stays visible until the scene's next detonation pass
        // deals its radius damage and removes it.
        entity.damageable = false;
        entity.pending_explosion = true;
        *scene_work = true;
        result.killed_targets = result.killed_targets.saturating_add(1);
    } else if entity.health <= 0 {
        entity.damageable = false;
        entity.visible = false;
        result.killed_targets = result.killed_targets.saturating_add(1);
        result.response_sound = monster_death_sound(class_name).map(|sound| {
            SoundEvent::at(sound, entity.origin).on(entity.source_index, crate::audio::CHAN_VOICE)
        });
    } else {
        result.response_sound = monster_pain_sound(class_name).map(|sound| {
            SoundEvent::at(sound, entity.origin).on(entity.source_index, crate::audio::CHAN_VOICE)
        });
    }
}

fn merge_rocket_damage(result: &mut RocketResult, damage: DamageResult) {
    result.total_damage = result.total_damage.saturating_add(damage.total_damage);
    result.killed_targets = result.killed_targets.saturating_add(damage.killed_targets);
    if damage.last_source_index.is_some() {
        result.last_source_index = damage.last_source_index;
    }
    if damage.response_sound.is_some() {
        result.response_sound = damage.response_sound;
    }
}

/// `interpolate_segment` for the sites that only run on a discrete impact.
///
/// Kept out of line on purpose. `mul_q12_i32` saturates, so one expansion of
/// the inline form costs about a hundred bytes an axis, and thirteen callers
/// of it came to nearly eight kilobytes of image. The four sites that run per
/// projectile per frame keep the inline form: outlining those as well cost
/// measurable frames on the E1M1 bench for no extra saving here.
#[inline(never)]
fn interpolate_impact(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    interpolate_segment(start, end, fraction)
}

fn interpolate_segment(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    let component =
        |from: i32, to: i32| from.saturating_add(mul_q12_i32(to.saturating_sub(from), fraction));
    Vec3I32 {
        x: component(start.x, end.x),
        y: component(start.y, end.y),
        z: component(start.z, end.z),
    }
}

#[inline(never)]
#[optimize(size)]
fn aabb_impact_normal(impact: Vec3I32, mins: Vec3I32, maxs: Vec3I32) -> Vec3I16 {
    let distances = [
        impact.x.saturating_sub(mins.x).unsigned_abs(),
        impact.x.saturating_sub(maxs.x).unsigned_abs(),
        impact.y.saturating_sub(mins.y).unsigned_abs(),
        impact.y.saturating_sub(maxs.y).unsigned_abs(),
        impact.z.saturating_sub(mins.z).unsigned_abs(),
        impact.z.saturating_sub(maxs.z).unsigned_abs(),
    ];
    let mut face = 0usize;
    let mut distance = distances[0];
    let mut index = 1usize;
    while index < distances.len() {
        if distances[index] < distance {
            face = index;
            distance = distances[index];
        }
        index += 1;
    }
    match face {
        0 => Vec3I16 {
            x: -4096,
            y: 0,
            z: 0,
        },
        1 => Vec3I16 {
            x: 4096,
            y: 0,
            z: 0,
        },
        2 => Vec3I16 {
            x: 0,
            y: -4096,
            z: 0,
        },
        3 => Vec3I16 {
            x: 0,
            y: 4096,
            z: 0,
        },
        4 => Vec3I16 {
            x: 0,
            y: 0,
            z: -4096,
        },
        _ => Vec3I16 {
            x: 0,
            y: 0,
            z: 4096,
        },
    }
}

/// `T_Damage`'s push on a `MOVETYPE_WALK` target:
/// `normalize(targ.origin - inflictor centre) * damage * 8`, in Q12 units per
/// second. `damage` is the pre-armour amount, as in the original. A zero
/// offset normalises to zero, so a self-centred inflictor pushes nothing.
#[cold]
#[inline(never)]
fn knockback_impulse(target_origin: Vec3I32, inflictor_center: Vec3I32, damage: i16) -> Vec3I32 {
    let damage = i32::from(damage.max(0));
    let raw = subtract_vec(target_origin, inflictor_center);
    // Whole units keep the squares in range; inside one unit the raw Q12
    // offset is small enough to use directly.
    let shift = if raw.x.abs() >= 1 << 12 || raw.y.abs() >= 1 << 12 || raw.z.abs() >= 1 << 12 {
        12
    } else {
        0
    };
    let component = |value: i32| (value >> shift).clamp(-32_767, 32_767);
    let x = component(raw.x);
    let y = component(raw.y);
    let z = component(raw.z);
    let length = isqrt_i32(
        x.saturating_mul(x)
            .saturating_add(y.saturating_mul(y))
            .saturating_add(z.saturating_mul(z)),
    );
    if length <= 0 || damage == 0 {
        return Vec3I32::default();
    }
    let scale = damage.saturating_mul(8);
    let axis = |value: i32| ((value << 12) / length).saturating_mul(scale);
    Vec3I32 {
        x: axis(x),
        y: axis(y),
        z: axis(z),
    }
}

/// Recover the player origin from the hull box a caller already holds
/// (mins + the hull's 16/16/24 offsets).
fn player_origin_from_mins(player_mins: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: player_mins.x.saturating_add(16 << 12),
        y: player_mins.y.saturating_add(16 << 12),
        z: player_mins.z.saturating_add(24 << 12),
    }
}

fn world_point_contents(map: &ResidentMap, point: Vec3I32) -> Option<i16> {
    let world = map.brush_models().get(0)?;
    Some(unsafe {
        CollisionHull::from_native_clip_nodes(
            map.collision_planes(),
            map.collision_clip_nodes(),
            world.head_nodes[0],
        )
    })?
    .point_contents(point)
}

#[inline(never)]
fn midpoint_vec_all(mins: Vec3I32, maxs: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: midpoint_all(mins.x, maxs.x),
        y: midpoint_all(mins.y, maxs.y),
        z: midpoint_all(mins.z, maxs.z),
    }
}

fn midpoint_all(min: i32, max: i32) -> i32 {
    min.saturating_add(max.saturating_sub(min) / 2)
}

#[inline(never)]
fn distance_units(left: Vec3I32, right: Vec3I32) -> i32 {
    let component = |a: i32, b: i32| (a.saturating_sub(b) >> 12).clamp(-32_767, 32_767);
    let x = component(left.x, right.x);
    let y = component(left.y, right.y);
    let z = component(left.z, right.z);
    isqrt_i32(
        x.saturating_mul(x)
            .saturating_add(y.saturating_mul(y))
            .saturating_add(z.saturating_mul(z)),
    )
}

#[inline(never)]
fn expanded_overlap(
    player_mins: Vec3I32,
    player_maxs: Vec3I32,
    entity_mins: [i16; 3],
    entity_maxs: [i16; 3],
    horizontal: i32,
    vertical: i32,
) -> bool {
    let mins = Vec3I32 {
        x: (i32::from(entity_mins[0]) - horizontal) << 12,
        y: (i32::from(entity_mins[1]) - horizontal) << 12,
        z: (i32::from(entity_mins[2]) - vertical) << 12,
    };
    let maxs = Vec3I32 {
        x: (i32::from(entity_maxs[0]) + horizontal) << 12,
        y: (i32::from(entity_maxs[1]) + horizontal) << 12,
        z: (i32::from(entity_maxs[2]) + vertical) << 12,
    };
    aabb_overlaps(player_mins, player_maxs, mins, maxs)
}

fn segment_overlaps_entity(start: Vec3I32, end: Vec3I32, entity: &RenderEntity) -> bool {
    segment_overlaps_i16_bounds(start, end, entity.clip_mins, entity.clip_maxs)
}

#[inline(never)]
fn bounds_center(mins: [i16; 3], maxs: [i16; 3]) -> Vec3I32 {
    let center: [i32; 3] = core::array::from_fn(|axis| {
        (i32::from(mins[axis]) + (i32::from(maxs[axis]) - i32::from(mins[axis])) / 2) << 12
    });
    Vec3I32 {
        x: center[0],
        y: center[1],
        z: center[2],
    }
}

#[inline(never)]
fn translated_q12(origin: Vec3I32, bounds: Vec3I16) -> Vec3I32 {
    Vec3I32 {
        x: origin.x.saturating_add(i32::from(bounds.x) << 12),
        y: origin.y.saturating_add(i32::from(bounds.y) << 12),
        z: origin.z.saturating_add(i32::from(bounds.z) << 12),
    }
}

#[cfg(any(feature = "combat-regression", feature = "arsenal-regression"))]
fn midpoint_vec(mins: Vec3I32, maxs: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: midpoint(mins.x, maxs.x),
        y: midpoint(mins.y, maxs.y),
        z: midpoint(mins.z, maxs.z),
    }
}

#[optimize(size)]
fn monster_pain_sound(class_name: u8) -> Option<i16> {
    match class_name {
        0x36 => Some(0xbc),
        0x38 => Some(0x24),
        0x39 => Some(0x29),
        0x3a => Some(0x44),
        0x3c => Some(0x52),
        0x3d => Some(0x67),
        0x3e => Some(0x83),
        0x41 => Some(0xb6),
        0x43 => Some(0xd6),
        _ => None,
    }
}

#[optimize(size)]
fn monster_death_sound(class_name: u8) -> Option<i16> {
    match class_name {
        0x36 => Some(0xba),
        0x38 => Some(0x20),
        0x39 => Some(0x28),
        0x3a => Some(0x40),
        0x3b => Some(0x4b),
        0x3c => Some(0x4e),
        0x3d => Some(0x66),
        0x3e => Some(0x80),
        0x40 => Some(0xad),
        0x41 => Some(0xb5),
        0x43 => Some(0xd3),
        _ => None,
    }
}

#[cfg(feature = "episode1-regression")]
static mut TRAIN_LEG_DEBUG: [i32; 13] = [0; 13];

pub struct SceneCollision<'a> {
    map: &'a ResidentMap,
    entities: &'a [RenderEntity],
    collision_indices: &'a [u16],
    world_head_node: i16,
    /// Render index the trace must pretend is `SOLID_NOT`, or `u16::MAX`.
    ignored: u16,
    /// Candidates near this frame's player, when the composition has one.
    near: Option<NearCandidates>,
}

/// Half-extent, in whole units, of the region a frame's player traces are
/// gathered for. Anything sweeping outside it scans every candidate instead.
const NEAR_REGION_UNITS: i32 = 256;
/// More candidates than this near one player disables the prefilter for the
/// frame rather than truncating it.
const NEAR_CANDIDATES_MAX: usize = 32;

/// The subsequence of `collision_indices` whose margin-expanded clip boxes reach
/// a region around the player, in the same ascending order. Skipping the rest
/// for a swept box inside the region cannot change a trace's answer
/// (`quake_core::body::BroadPhaseRegion`).
#[derive(Copy, Clone)]
struct NearCandidates {
    region: BroadPhaseRegion,
    len: u8,
    indices: [u16; NEAR_CANDIDATES_MAX],
}

impl NearCandidates {
    fn gather(
        entities: &[RenderEntity],
        collision_indices: &[u16],
        anchor: Vec3I32,
    ) -> Option<Self> {
        let region = BroadPhaseRegion::around(anchor, NEAR_REGION_UNITS);
        let mut near = Self {
            region,
            len: 0,
            indices: [0; NEAR_CANDIDATES_MAX],
        };
        for &index in collision_indices {
            let entity = entities.get(index as usize)?;
            if !region.may_overlap(entity.clip_mins, entity.clip_maxs) {
                continue;
            }
            if near.len as usize == NEAR_CANDIDATES_MAX {
                return None;
            }
            near.indices[near.len as usize] = index;
            near.len += 1;
        }
        Some(near)
    }
}

#[cfg(feature = "e1m1-chain-regression")]
static mut LAST_PLAYER_COLLISION_SOURCE: u16 = u16::MAX;

#[cfg(feature = "e1m1-chain-regression")]
pub fn regression_last_player_collision_source() -> u16 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(LAST_PLAYER_COLLISION_SOURCE)) }
}

#[cfg(feature = "bestiary-regression")]
static mut LAST_PLAYER_BODY_BLOCK_SOURCE: u16 = u16::MAX;

/// Authored source index of the last monster body that stopped the player, or
/// `u16::MAX` if no body has blocked the player yet.
#[cfg(feature = "bestiary-regression")]
pub fn regression_last_player_body_block() -> u16 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(LAST_PLAYER_BODY_BLOCK_SOURCE)) }
}

/// The player moves through hull 1, so every player body block expands the
/// blocker by hull 1's extents.
const PLAYER_HULL_INDEX: usize = 1;

/// Build the dynamic body a live monster or solid explobox contributes.
/// Corpses, detonated boxes, gibs, and projectile slots are never candidates.
fn dynamic_body(entity: &RenderEntity) -> Option<Body> {
    let dead = match entity.monster {
        Some(runtime) => !runtime.body_solid() || !entity.visible,
        None if entity.solid && entity.model_id >= 0 => {
            !entity.visible || !entity.damageable || entity.health <= 0
        }
        None => return None,
    };
    Some(Body {
        source_index: entity.source_index,
        mins: entity.hit_mins,
        maxs: entity.hit_maxs,
        dead,
    })
}

impl MovementTrace for SceneCollision<'_> {
    fn trace(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        let Some(world) = Some(unsafe {
            CollisionHull::from_native_clip_nodes(
                self.map.collision_planes(),
                self.map.collision_clip_nodes(),
                self.world_head_node,
            )
        }) else {
            return false;
        };
        let mut best = Trace::default();
        if !world.trace_into(start, end, scratch, &mut best) {
            return false;
        }
        let mut blocking_body: Option<u16> = None;
        // Live monster bodies join the same broad phase as the solid brush
        // submodels, in ascending authored source index (the render slots are
        // filled in load order), so the candidate set is deterministic.
        let mut bodies = BodyBlockers::new();
        // The broad phase runs once per candidate per trace, dozens of times a
        // frame, so the swept box is reduced to whole units once here and the
        // monster discriminant is only read for candidates that overlap.
        let swept = SweptUnitBox::new(*start, *end);
        let candidates: &[u16] = match &self.near {
            Some(near) if near.region.contains(&swept) => &near.indices[..near.len as usize],
            _ => self.collision_indices,
        };
        for &index in candidates {
            if index == self.ignored {
                continue;
            }
            let entity = unsafe { self.entities.get_unchecked(index as usize) };
            if !swept.overlaps(entity.clip_mins, entity.clip_maxs) {
                continue;
            }
            if entity.monster.is_some() || entity.model_id >= 0 {
                if let Some(body) = dynamic_body(entity) {
                    bodies.push(body);
                }
                continue;
            }
            let Some(model) = self.map.brush_models().get(entity.model_index as usize) else {
                continue;
            };
            let Some(hull) = Some(unsafe {
                CollisionHull::from_native_clip_nodes(
                    self.map.collision_planes(),
                    self.map.collision_clip_nodes(),
                    model.head_nodes[1],
                )
            }) else {
                continue;
            };
            let mut candidate = Trace::default();
            if !trace_translated_hull(hull, entity.origin, start, end, scratch, &mut candidate) {
                continue;
            }
            if candidate.fraction < best.fraction
                || (candidate.start_solid && !best.start_solid)
                || (candidate.all_solid && !best.all_solid)
            {
                best = candidate;
                blocking_body = None;
                #[cfg(feature = "e1m1-chain-regression")]
                unsafe {
                    core::ptr::write_volatile(
                        core::ptr::addr_of_mut!(LAST_PLAYER_COLLISION_SOURCE),
                        entity.source_index,
                    );
                }
            }
        }
        // A body block never reports start_solid, so a world hit at the same
        // fraction keeps its plane and the mover can never be frozen by a body.
        if let Some(impact) = bodies.resolve(*start, *end, PLAYER_HULL_INDEX) {
            if !best.start_solid && !best.all_solid && impact.fraction < best.fraction {
                best.fraction = impact.fraction;
                best.end = impact.end;
                best.normal = impact.normal;
                blocking_body = self
                    .entities
                    .iter()
                    .position(|entity| entity.source_index == impact.source_index)
                    .map(|index| index.min(u16::MAX as usize) as u16);
                #[cfg(feature = "bestiary-regression")]
                unsafe {
                    core::ptr::write_volatile(
                        core::ptr::addr_of_mut!(LAST_PLAYER_BODY_BLOCK_SOURCE),
                        impact.source_index,
                    );
                }
            }
        }
        *output = MovementTraceResult {
            all_solid: best.all_solid,
            start_solid: best.start_solid,
            fraction: best.fraction,
            end: best.end,
            normal: best.normal,
            blocking_body,
        };
        true
    }
}

/// Trace one translated Quake brush without paying the general rotated-brush
/// matrix path. Every shipping door, platform and train is translation-only;
/// this is algebraically identical to `TransformedCollisionHull` with an
/// identity rotation, but removes eighteen Q12 multiplies per candidate.
fn trace_translated_hull(
    hull: CollisionHull<'_>,
    origin: Vec3I32,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut TraceScratch,
    output: &mut Trace,
) -> bool {
    let local_start = subtract_vec(*start, origin);
    let local_end = subtract_vec(*end, origin);
    let mut trace = Trace::default();
    if !hull.trace_into(&local_start, &local_end, scratch, &mut trace) {
        return false;
    }
    trace.end = Vec3I32 {
        x: start
            .x
            .saturating_add(mul_q12_i32(end.x.saturating_sub(start.x), trace.fraction)),
        y: start
            .y
            .saturating_add(mul_q12_i32(end.y.saturating_sub(start.y), trace.fraction)),
        z: start
            .z
            .saturating_add(mul_q12_i32(end.z.saturating_sub(start.z), trace.fraction)),
    };
    trace.plane_distance = trace.plane_distance.saturating_add(
        mul_q12_i32(origin.x, trace.normal.x as i32)
            .saturating_add(mul_q12_i32(origin.y, trace.normal.y as i32))
            .saturating_add(mul_q12_i32(origin.z, trace.normal.z as i32)),
    );
    *output = trace;
    true
}

fn swept_player_overlaps_entity(start: Vec3I32, end: Vec3I32, entity: &RenderEntity) -> bool {
    const PLAYER_BROAD_PHASE_MARGIN_Q12: i32 = 32 << 12;
    let start = [start.x, start.y, start.z];
    let end = [end.x, end.y, end.z];
    let mut axis = 0usize;
    while axis < 3 {
        let swept_min = start[axis].min(end[axis]);
        let swept_max = start[axis].max(end[axis]);
        let entity_min =
            (i32::from(entity.clip_mins[axis]) << 12).saturating_sub(PLAYER_BROAD_PHASE_MARGIN_Q12);
        let entity_max =
            (i32::from(entity.clip_maxs[axis]) << 12).saturating_add(PLAYER_BROAD_PHASE_MARGIN_Q12);
        if swept_max < entity_min || swept_min > entity_max {
            return false;
        }
        axis += 1;
    }
    true
}

const fn add_vec(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}

const fn subtract_vec(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_sub(right.x),
        y: left.y.saturating_sub(right.y),
        z: left.z.saturating_sub(right.z),
    }
}

/// Q12 dot product. `left` may carry a large Q12 offset; `right` should be
/// a unit-scale vector so the products stay in range.
#[inline(never)]
fn dot_q12(left: Vec3I32, right: Vec3I32) -> i32 {
    mul_q12_i32(left.x, right.x)
        .saturating_add(mul_q12_i32(left.y, right.y))
        .saturating_add(mul_q12_i32(left.z, right.z))
}

/// Unit Q12 direction of a Q12 offset; zero stays zero. The offset is
/// scaled down to 14 bits per axis first so the squares fit an i32 while
/// keeping more precision than whole units would.
#[inline(never)]
fn normalize_q12(vector: Vec3I32) -> Vec3I32 {
    let largest = vector
        .x
        .unsigned_abs()
        .max(vector.y.unsigned_abs())
        .max(vector.z.unsigned_abs());
    let mut shift = 0u32;
    while (largest >> shift) >= 1 << 14 {
        shift += 1;
    }
    let x = vector.x >> shift;
    let y = vector.y >> shift;
    let z = vector.z >> shift;
    let length = isqrt_i32(x * x + y * y + z * z);
    if length <= 0 {
        return Vec3I32::default();
    }
    Vec3I32 {
        x: (x << 12) / length,
        y: (y << 12) / length,
        z: (z << 12) / length,
    }
}

#[cfg(any(
    feature = "episode1-regression",
    feature = "combat-regression",
    feature = "episode1-route-regression",
    feature = "arsenal-regression"
))]
fn midpoint(min: i32, max: i32) -> i32 {
    min.saturating_add(max.saturating_sub(min) / 2)
}

#[optimize(size)]
fn change_level(
    map: &ResidentMap,
    source_index: u16,
    entity: MapEntity,
    destination: EpisodeMap,
) -> Option<ChangeLevel> {
    if entity.model >= 0 {
        return None;
    }
    let model_index = entity.model.checked_neg()? as usize;
    let model = map.brush_models().get(model_index)?;
    let offset = entity.origin;
    let mins = Vec3I32 {
        x: offset.x.saturating_add((model.mins.x as i32) << 12),
        y: offset.y.saturating_add((model.mins.y as i32) << 12),
        z: offset.z.saturating_add((model.mins.z as i32) << 12),
    };
    let maxs = Vec3I32 {
        x: offset.x.saturating_add((model.maxs.x as i32) << 12),
        y: offset.y.saturating_add((model.maxs.y as i32) << 12),
        z: offset.z.saturating_add((model.maxs.z as i32) << 12),
    };
    Some(ChangeLevel {
        source_index,
        mins,
        maxs,
        destination,
        no_intermission: entity.spawn_flags & CHANGELEVEL_NO_INTERMISSION != 0,
    })
}

#[inline(never)]
fn aabb_overlaps(
    left_mins: Vec3I32,
    left_maxs: Vec3I32,
    right_mins: Vec3I32,
    right_maxs: Vec3I32,
) -> bool {
    left_maxs.x >= right_mins.x
        && left_mins.x <= right_maxs.x
        && left_maxs.y >= right_mins.y
        && left_mins.y <= right_maxs.y
        && left_maxs.z >= right_mins.z
        && left_mins.z <= right_maxs.z
}

#[derive(Copy, Clone)]
struct RenderSpawn {
    model_id: i16,
    skin: u8,
    animation: SpawnAnimation,
}

#[derive(Copy, Clone)]
enum SpawnAnimation {
    Static,
    Range(u16, u16),
    All { initial: u16 },
}

impl SpawnAnimation {
    fn resolve(self, header: AliasModelHeader) -> (u16, u16, u16) {
        let last = header.frame_count.saturating_sub(1);
        match self {
            Self::Static => (0, 0, 0),
            Self::Range(start, end) if start <= last => (start, start, end.min(last).max(start)),
            Self::Range(_, _) => (0, 0, 0),
            Self::All { initial } => (initial.min(last), 0, last),
        }
    }
}

#[optimize(size)]
fn render_spawn(entity: MapEntity) -> Option<RenderSpawn> {
    if entity.model < 0 {
        return None;
    }
    if entity.model > 0 {
        return Some(spawn(entity.model, 0, SpawnAnimation::Static));
    }

    let spawn = match entity.class_name {
        0x1a => spawn(0x0e, 0, SpawnAnimation::Static),
        0x1b => spawn(0x0e, 1, SpawnAnimation::Static),
        0x1c => spawn(0x0e, 2, SpawnAnimation::Static),
        0x1d => spawn(0x4b, 0, SpawnAnimation::Static),
        0x1e => spawn(0x38, 0, SpawnAnimation::Static),
        0x1f => spawn(0x39, 0, SpawnAnimation::Static),
        0x20 => spawn(0x44, 0, SpawnAnimation::Static),
        0x21 => spawn(
            if entity.spawn_flags & SPAWNFLAG_BIG_OR_SMALL != 0 {
                0x02
            } else {
                0x01
            },
            0,
            SpawnAnimation::Static,
        ),
        0x22 => spawn(
            if entity.spawn_flags & SPAWNFLAG_BIG_OR_SMALL != 0 {
                0x03
            } else if entity.spawn_flags & SPAWNFLAG_SUPER_HEALTH != 0 {
                0x04
            } else {
                0x05
            },
            0,
            SpawnAnimation::Static,
        ),
        0x23 => spawn(0x58, 0, SpawnAnimation::Static),
        0x24 => spawn(0x57, 0, SpawnAnimation::Static),
        0x25 => spawn(
            if entity.spawn_flags & SPAWNFLAG_BIG_OR_SMALL != 0 {
                0x0b
            } else {
                0x0a
            },
            0,
            SpawnAnimation::Static,
        ),
        0x26 => spawn(
            if entity.spawn_flags & SPAWNFLAG_BIG_OR_SMALL != 0 {
                0x0d
            } else {
                0x0c
            },
            0,
            SpawnAnimation::Static,
        ),
        0x27 if entity.spawn_flags & 1 != 0 => spawn(0x18, 0, SpawnAnimation::Static),
        0x28 => spawn(
            if entity.spawn_flags & SPAWNFLAG_BIG_OR_SMALL != 0 {
                0x09
            } else {
                0x08
            },
            0,
            SpawnAnimation::Static,
        ),
        0x2b => spawn(0x20, 0, SpawnAnimation::All { initial: 1 }),
        0x2c | 0x2d => spawn(0x20, 0, SpawnAnimation::All { initial: 0 }),
        0x31 => spawn(0x1f, 0, SpawnAnimation::All { initial: 0 }),
        0x32 => spawn(0x07, 0, SpawnAnimation::Static),
        0x33 => spawn(0x06, 0, SpawnAnimation::Static),
        0x36 => spawn(0x49, 0, SpawnAnimation::Range(0, 7)),
        // Chthon spawns hidden at his authored origin and is revealed by his
        // encounter trigger, which starts the rise sequence.
        0x37 => spawn(0x15, 0, SpawnAnimation::Range(0, 16)),
        0x38 => spawn(0x16, 0, SpawnAnimation::Range(0, 12)),
        0x39 => spawn(0x17, 0, SpawnAnimation::Range(69, 77)),
        0x3a => spawn(0x1c, 0, SpawnAnimation::Range(0, 6)),
        0x3b => spawn(0x1e, 0, SpawnAnimation::Range(40, 57)),
        0x3c => spawn(0x37, 0, SpawnAnimation::Range(0, 8)),
        0x3d => spawn(0x3b, 0, SpawnAnimation::Range(0, 8)),
        0x3e => spawn(0x41, 0, SpawnAnimation::Range(0, 8)),
        0x3f => spawn(0x42, 0, SpawnAnimation::Static),
        0x40 => spawn(0x47, 0, SpawnAnimation::Static),
        0x41 => spawn(0x48, 0, SpawnAnimation::Range(0, 16)),
        0x42 => spawn(0x4c, 0, SpawnAnimation::Static),
        0x43 => spawn(0x5a, 0, SpawnAnimation::Range(0, 7)),
        0x44 if entity.spawn_flags & SPAWNFLAG_ZOMBIE_CRUCIFIED != 0 => {
            spawn(0x5c, 0, SpawnAnimation::Range(192, 197))
        }
        0x44 => spawn(0x5c, 0, SpawnAnimation::Range(0, 7)),
        0x53 => spawn(0x24, 0, SpawnAnimation::Static),
        0x54 => spawn(0x21, 0, SpawnAnimation::Static),
        0x55 => spawn(0x22, 0, SpawnAnimation::Static),
        0x56 => spawn(0x25, 0, SpawnAnimation::Static),
        0x57 => spawn(0x23, 0, SpawnAnimation::Static),
        0x58 => spawn(0x26, 0, SpawnAnimation::Static),
        _ => return None,
    };
    Some(spawn)
}

/// Choose the BSP leaf used to cull one static alias entity.
///
/// Quake links an entity's bounds into every touched leaf. This renderer keeps
/// one representative leaf instead. Authored floor pickups commonly sit
/// exactly on the floor plane, where the raw origin resolves to solid leaf 0
/// even though the model lies in the room and remains collectible. Probe 16
/// world units above that boundary for pickups only; the complete Episode 1
/// corpus resolves every such floor item to a renderable leaf at this point.
/// Point leaf and baked light for a static alias entity standing at `origin`.
/// The spawn loop and `droptofloor` share one copy of it.
#[inline(never)]
fn alias_leaf_light(
    map: &ResidentMap,
    origin: Vec3I32,
    floor_pickup: bool,
    styles: &[u16; lightstyle::DUMMY_STYLE + 1],
) -> Option<(u16, u8)> {
    let leaf_index = static_alias_leaf_index(map, origin, floor_pickup)?;
    let leaf = map.leaves().get(leaf_index)?;
    Some((
        leaf_index as u16,
        leaf_light(leaf.lightmap, leaf.light_styles, styles),
    ))
}

fn static_alias_leaf_index(
    map: &ResidentMap,
    origin: Vec3I32,
    floor_pickup: bool,
) -> Option<usize> {
    let leaf = map.point_leaf_index(origin)?;
    if leaf != 0 || !floor_pickup {
        return Some(leaf);
    }
    map.point_leaf_index(Vec3I32 {
        z: origin.z.saturating_add(16 << 12),
        ..origin
    })
}

const fn spawn(model_id: i16, skin: u8, animation: SpawnAnimation) -> RenderSpawn {
    RenderSpawn {
        model_id,
        skin,
        animation,
    }
}

pub fn model_rotates(header: AliasModelHeader) -> bool {
    header.flags & EF_ROTATE != 0
}

fn model_clip_radius(header: AliasModelHeader) -> i16 {
    let extent = |min_q12: i32, max_q12: i32| {
        let min = min_q12 >> 12;
        let max = max_q12.saturating_add(0x0fff) >> 12;
        min.saturating_abs().max(max.saturating_abs())
    };
    let x = extent(header.mins.x, header.maxs.x);
    let y = extent(header.mins.y, header.maxs.y);
    let z = extent(header.mins.z, header.maxs.z);
    let squared = x
        .saturating_mul(x)
        .saturating_add(y.saturating_mul(y))
        .saturating_add(z.saturating_mul(z));
    let floor = isqrt_i32(squared);
    let radius = floor.saturating_add(i32::from(floor.saturating_mul(floor) != squared));
    radius.clamp(1, i16::MAX as i32) as i16
}
