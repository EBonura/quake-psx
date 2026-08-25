//! Image-free authored-map monster regression.
//!
//! The route is ordinary input only: the player spawns at the map's authored
//! `info_player_start` and walks with the normal movement path toward an
//! authored monster origin read out of the cooked entity table. Nothing in
//! this module teleports, places, or nudges the player, and no coordinate in
//! the route is hand written: every waypoint is the authored origin of the
//! monster the stage is proving.
//!
//! Each stage checks that one monster acquires the
//! player, moves from its authored spot, enters an attack state, takes pain
//! from an ordinary weapon, dies, and finishes its corpse frames. The stage
//! also requires the player to be stopped by a live monster body at least
//! once, which is why the approach keeps walking into the monster instead of
//! holding at range.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::atan2_q12;
use psx_math::int32::{isqrt_i32, mul_q12_i32, square_i32_saturating};
use psx_pad::button;
use quake_core::combat::WeaponState;
use quake_core::monster::MonsterState;
use quake_formats::Vec3I32;

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::input::InputFrame;
use crate::player::Player;

const PROBE_MAGIC: u32 = 0x5150_5358;
/// Version 10: the convergence probes hold 3 through 7 and 9.
const PROBE_VERSION: u32 = 10;
const PHASE_APPROACH: u32 = 0xa0;
const PHASE_COMPLETE: u32 = 0xaf;
const PHASE_ERROR: u32 = 0xff;

const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_NO_AUTHORED_MONSTER: u32 = 2;
const FAILURE_LOST_MONSTER: u32 = 3;
const FAILURE_TIMEOUT: u32 = 4;
const FAILURE_PLAYER_DIED: u32 = 5;

/// Authored stages, in route order. Each entry names a real Episode 1 map and
/// the monster class it proves; the stage itself picks the nearest authored,
/// skill-admitted instance of that class to the map's own player start, so no
/// coordinate in this table is a position. Class ids are from
/// `tools/cfg/id1/entmap.txt`.
const STAGES: [(EpisodeMap, u8); 2] = [
    (EpisodeMap::E1M2, 0x3e), // monster_ogre
    (EpisodeMap::E1M4, 0x3d), // monster_knight
];

const MAX_STAGE_FRAMES: u32 = 3_600;
/// Frames to spend on one authored candidate before trying the next-nearest.
const CANDIDATE_FRAMES: u32 = 1_800;

const ACQUIRED: u32 = 1 << 0;
const MOVED: u32 = 1 << 1;
const ATTACKED: u32 = 1 << 2;
const PAIN: u32 = 1 << 3;
const DIED: u32 = 1 << 4;
const CORPSE: u32 = 1 << 5;
const BODY_BLOCKED: u32 = 1 << 6;
const REQUIRED: u32 = ACQUIRED | MOVED | ATTACKED | PAIN | DIED | CORPSE | BODY_BLOCKED;

#[repr(C)]
#[derive(Copy, Clone)]
struct Probe {
    magic: u32,
    version: u32,
    complete: u32,
    phase: u32,
    failure_code: u32,
    failure_map: u32,
    failure_entity: u32,
    failure_detail: u32,
    total_frames: u32,
    maps_loaded: u32,
    maps_validated: u32,
    transitions: u32,
    weapon_selected: u32,
    weapon_fired: u32,
    weapon_animated: u32,
    monster_present: u32,
    monster_animated: u32,
    monster_state_bounds: u32,
    monster_attack: u32,
    monster_pain: u32,
    monster_death: u32,
    boss: u32,
    current_map: u32,
    route_index: u32,
    last_health: u32,
    state_ranges: u32,
    valid_state_ranges: u32,
    map_loads: u32,
    stage_frames: u32,
    shock_count: u32,
    intermission_state: u32,
    player_state: u32,
    weapon_pickups: u32,
    target_edges: u32,
}

impl Probe {
    const fn new() -> Self {
        Self {
            magic: PROBE_MAGIC,
            version: PROBE_VERSION,
            complete: 0,
            phase: PHASE_APPROACH,
            failure_code: 0,
            failure_map: 0,
            failure_entity: u32::MAX,
            failure_detail: 0,
            total_frames: 0,
            maps_loaded: 0,
            maps_validated: 0,
            transitions: 0,
            weapon_selected: 0,
            weapon_fired: 0,
            weapon_animated: 0,
            monster_present: 0,
            monster_animated: 0,
            monster_state_bounds: 0,
            monster_attack: 0,
            monster_pain: 0,
            monster_death: 0,
            boss: 0,
            current_map: u32::MAX,
            route_index: 0,
            last_health: 0,
            state_ranges: 0,
            valid_state_ranges: 0,
            map_loads: 0,
            stage_frames: 0,
            shock_count: 0,
            intermission_state: 0,
            player_state: 0,
            weapon_pickups: 0,
            target_edges: 0,
        }
    }
}

const _: [(); 136] = [(); core::mem::size_of::<Probe>()];

#[used]
static mut PROBE: Probe = Probe::new();

#[derive(Copy, Clone)]
struct State {
    initialized: bool,
    stage: u8,
    stage_frames: u32,
    contract: u32,
    source_index: u16,
    waypoint: Vec3I32,
    spawn_origin: Vec3I32,
    candidate: u8,
    candidate_frames: u32,
    /// Last approach distance and how long it has failed to shrink, which is
    /// what steers the bounded detour fan.
    best_distance: i32,
    stalled_frames: u32,
    detour: u8,
}

impl State {
    const fn new() -> Self {
        Self {
            initialized: false,
            stage: 0,
            stage_frames: 0,
            contract: 0,
            source_index: u16::MAX,
            waypoint: Vec3I32 { x: 0, y: 0, z: 0 },
            spawn_origin: Vec3I32 { x: 0, y: 0, z: 0 },
            candidate: 0,
            candidate_frames: 0,
            best_distance: i32::MAX,
            stalled_frames: 0,
            detour: 0,
        }
    }
}

static mut STATE: State = State::new();

/// Begin the stage the route is currently on. Called once at boot and once per
/// harness map load.
pub fn setup(world: &ResidentMap, entities: &EntityScene, player: &Player) -> bool {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        let stage = state.stage as usize;
        let Some(&(map, _)) = STAGES.get(stage) else {
            return true;
        };
        if world.map() != map {
            fail(FAILURE_BAD_MAP, map_index(world.map()));
            return false;
        }
        if !select_candidate(world, entities, player, state, 0) {
            fail(FAILURE_NO_AUTHORED_MONSTER, map_index(map));
            return false;
        }
        state.initialized = true;
        state.stage_frames = 0;
        state.contract = 0;
        let probe = addr_of_mut!(PROBE);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_APPROACH);
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
        write_volatile(addr_of_mut!((*probe).map_loads), state.stage as u32 + 1);
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | 1 << map_index(map),
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | 1 << map_index(map),
        );
        write_volatile(
            addr_of_mut!((*probe).monster_present),
            read_volatile(addr_of_mut!((*probe).monster_present)) | 1 << stage,
        );
        true
    }
}

/// The map the route should be on, so the game loop can request the next one.
pub fn requested_map() -> Option<EpisodeMap> {
    unsafe {
        let state = &*addr_of_mut!(STATE);
        STAGES.get(state.stage as usize).map(|&(map, _)| map)
    }
}

/// The map this stage boots into.
pub const fn initial_map() -> EpisodeMap {
    STAGES[0].0
}

const fn map_index(map: EpisodeMap) -> u32 {
    match map {
        EpisodeMap::Start => 0,
        EpisodeMap::E1M1 => 1,
        EpisodeMap::E1M2 => 2,
        EpisodeMap::E1M3 => 3,
        EpisodeMap::E1M4 => 4,
        EpisodeMap::E1M5 => 5,
        EpisodeMap::E1M6 => 6,
        EpisodeMap::E1M7 => 7,
        EpisodeMap::E1M8 => 8,
    }
}

/// Pick the `index`-th nearest authored monster of the stage class. The origin
/// it returns is the only waypoint the route ever uses.
unsafe fn select_candidate(
    world: &ResidentMap,
    entities: &EntityScene,
    player: &Player,
    state: &mut State,
    index: u8,
) -> bool {
    let spawn = player.origin();
    let class_name = STAGES[state.stage as usize].1;
    let Some((source_index, origin)) =
        entities.regression_nearest_monster(world, class_name, spawn, index)
    else {
        return false;
    };
    state.source_index = source_index;
    state.waypoint = origin;
    state.spawn_origin = origin;
    state.candidate = index;
    state.candidate_frames = 0;
    state.best_distance = i32::MAX;
    state.stalled_frames = 0;
    state.detour = 0;
    unsafe {
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).failure_entity),
            u32::from(source_index),
        );
        write_volatile(addr_of_mut!((*probe).route_index), u32::from(index));
        write_volatile(
            addr_of_mut!((*probe).weapon_pickups),
            u32::from(state.stage),
        );
    }
    true
}

pub fn controls(world: &ResidentMap, entities: &EntityScene, player: &Player) -> InputFrame {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized
            || read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        state.stage_frames = state.stage_frames.wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), state.stage_frames);
        if state.stage_frames > MAX_STAGE_FRAMES {
            fail(FAILURE_TIMEOUT, state.contract);
            return InputFrame::default();
        }

        // Track the live monster where it stands so the approach follows a
        // charging monster instead of its empty authored spot.
        if let Some(snapshot) = entities.regression_monster_snapshot(state.source_index) {
            if !snapshot.state.is_death() {
                state.waypoint = snapshot.origin;
            }
        }

        let origin = player.origin();
        let dx = (state.waypoint.x.saturating_sub(origin.x)) >> 12;
        let dy = (state.waypoint.y.saturating_sub(origin.y)) >> 12;
        let distance =
            isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));

        // A stalled approach walks the authored heading through a bounded
        // detour fan rather than pressing into a wall forever.
        state.candidate_frames = state.candidate_frames.wrapping_add(1);
        if distance < state.best_distance {
            state.best_distance = distance;
            state.stalled_frames = 0;
            state.detour = 0;
        } else {
            state.stalled_frames = state.stalled_frames.wrapping_add(1);
            if state.stalled_frames > 90 {
                state.stalled_frames = 0;
                state.detour = (state.detour + 1) & 7;
            }
        }
        if state.candidate_frames > CANDIDATE_FRAMES && state.contract & ACQUIRED == 0 {
            let next = state.candidate.saturating_add(1);
            if !select_candidate(world, entities, player, state, next) {
                fail(FAILURE_NO_AUTHORED_MONSTER, u32::from(next));
                return InputFrame::default();
            }
        }

        // Diagnostic trace: where the route actually got to.
        write_volatile(
            addr_of_mut!((*probe).weapon_animated),
            (origin.x >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).weapon_selected),
            (origin.y >> 12) as u32,
        );
        write_volatile(addr_of_mut!((*probe).weapon_fired), distance as u32);
        write_volatile(addr_of_mut!((*probe).boss), u32::from(state.candidate));
        write_volatile(addr_of_mut!((*probe).shock_count), u32::from(state.detour));
        write_volatile(
            addr_of_mut!((*probe).monster_state_bounds),
            (state.waypoint.x >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).intermission_state),
            (state.waypoint.y >> 12) as u32,
        );

        const DETOURS: [i16; 8] = [0, 512, -512, 1_024, -1_024, 1_536, -1_536, 2_048];
        let heading = (atan2_q12(dy, dx) as i16).wrapping_add(DETOURS[state.detour as usize]);
        // Face the monster while walking a possibly detoured heading, so the
        // ordinary weapon fire below aims where the shot must land.
        movement_input(player, heading, atan2_q12(dy, dx) as i16)
    }
}

/// Ordinary pad intent: walk along `heading`, turn the view toward `aim`, and
/// hold the fire button. Every field goes through the same `InputFrame` the
/// human controller fills in.
fn movement_input(player: &Player, heading: i16, aim: i16) -> InputFrame {
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let relative = (heading as u16).wrapping_sub(yaw) & 0x0fff;
    let forward = psx_math::cos_q12(relative);
    let strafe = psx_math::sin_q12(relative);
    let mut turn = (i32::from(aim as u16 & 0x0fff) - i32::from(yaw)) & 0x0fff;
    if turn > 2_048 {
        turn -= 4_096;
    }
    InputFrame {
        movement: [
            mul_q12_i32(forward, 127).clamp(-127, 127) as i16,
            mul_q12_i32(strafe, 127).clamp(-127, 127) as i16,
        ],
        // The look axis is intent, not an angle: steer toward the monster and
        // let the ordinary view integrator close the gap.
        look: [turn.clamp(-127, 127) as i16, 0],
        held: button::R2,
        ..InputFrame::default()
    }
}

pub fn observe(entities: &EntityScene, weapon: &WeaponState) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized
            || read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return;
        }
        write_volatile(
            addr_of_mut!((*probe).last_health),
            u32::from(weapon.inventory().health().max(0) as u16),
        );
        if weapon.inventory().health() <= 0 {
            fail(FAILURE_PLAYER_DIED, state.contract);
            return;
        }
        let Some(snapshot) = entities.regression_monster_snapshot(state.source_index) else {
            fail(FAILURE_LOST_MONSTER, u32::from(state.source_index));
            return;
        };
        if snapshot.active {
            state.contract |= ACQUIRED;
        }
        if snapshot.origin != state.spawn_origin {
            state.contract |= MOVED;
        }
        if matches!(
            snapshot.state,
            MonsterState::Missile
                | MonsterState::Melee
                | MonsterState::MeleeB
                | MonsterState::MeleeC
        ) {
            state.contract |= ATTACKED;
        }
        if snapshot.state.is_pain() {
            state.contract |= PAIN;
        }
        if snapshot.state.is_death() {
            state.contract |= DIED;
        }
        if snapshot.corpse_finished {
            state.contract |= CORPSE;
        }
        let block_source = crate::entity::regression_last_player_body_block();
        if block_source != u16::MAX {
            state.contract |= BODY_BLOCKED;
        }
        write_volatile(
            addr_of_mut!((*probe).monster_animated),
            u32::from(snapshot.frame),
        );
        write_volatile(addr_of_mut!((*probe).target_edges), u32::from(block_source));
        write_volatile(
            addr_of_mut!((*probe).state_ranges),
            u32::from(snapshot.health.max(0) as u16),
        );
        if state.contract & REQUIRED != REQUIRED {
            return;
        }
        // This stage is fully proved. Record it and hand the route to the next
        // authored map, or finish.
        let stage = state.stage;
        write_volatile(
            addr_of_mut!((*probe).player_state),
            read_volatile(addr_of_mut!((*probe).player_state)) | 1 << stage,
        );
        for (field, bit) in [
            (addr_of_mut!((*probe).monster_attack), ATTACKED),
            (addr_of_mut!((*probe).monster_pain), PAIN),
            (addr_of_mut!((*probe).monster_death), DIED),
        ] {
            if state.contract & bit != 0 {
                write_volatile(field, read_volatile(field) | 1 << stage);
            }
        }
        write_volatile(
            addr_of_mut!((*probe).valid_state_ranges),
            read_volatile(addr_of_mut!((*probe).valid_state_ranges)) | 1 << stage,
        );
        state.initialized = false;
        state.stage = stage.saturating_add(1);
        if state.stage as usize >= STAGES.len() {
            write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
            write_volatile(addr_of_mut!((*probe).complete), 1);
            write_volatile(addr_of_mut!((*probe).transitions), u32::from(state.stage));
        }
    }
}

fn fail(code: u32, detail: u32) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).failure_code), code);
        write_volatile(addr_of_mut!((*probe).failure_map), 2);
        write_volatile(addr_of_mut!((*probe).failure_detail), detail);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_ERROR);
    }
}
