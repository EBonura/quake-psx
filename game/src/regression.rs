//! Emulator-only deterministic Episode 1 map-transition probe.
//!
//! The driver waits for rendered gameplay in every map, then moves the real
//! player hull into a cooked `trigger_changelevel`. E1M4 is visited twice so
//! both its secret and normal exits are covered. The second visit is a direct
//! regression load because Start has no path back to E1M4. Shipping images do
//! not compile this module.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::player::Player;
use crate::renderer::RenderStats;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 3;
/// E1M5's authored trains, the only guest-side `func_train` evidence there is.
/// The probe struct is a fixed 136 bytes shared by every gate, so this reuses
/// two fields the map route has no other use for: `state_ranges` carries the
/// longest leg in ticks and `valid_state_ranges` the whole units the trains
/// actually travelled.
const TRAIN_MAP: EpisodeMap = EpisodeMap::E1M5;
const SETTLE_FRAMES: u32 = 8;
const PHASE_SETTLE: u32 = 1;
const PHASE_MAP_ROUTE_COMPLETE: u32 = 0x40;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TRANSITION: u32 = 2;
const FAILURE_CHANGE_LEVEL: u32 = 5;

const ROUTE: [EpisodeMap; 12] = [
    EpisodeMap::Start,
    EpisodeMap::E1M1,
    EpisodeMap::E1M2,
    EpisodeMap::E1M3,
    EpisodeMap::E1M4,
    EpisodeMap::E1M8,
    EpisodeMap::E1M5,
    EpisodeMap::E1M6,
    EpisodeMap::E1M7,
    EpisodeMap::Start,
    EpisodeMap::E1M4,
    EpisodeMap::E1M5,
];

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
            phase: 0,
            failure_code: 0,
            failure_map: 0,
            failure_entity: 0,
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

struct RouteState {
    initialized: bool,
    route_index: usize,
    stage_frames: u32,
    pending_destination: EpisodeMap,
    pending_transition: u32,
    pending: bool,
    /// Previous train position checksum, `u32::MAX` before the first sample.
    train_checksum: u32,
}

impl RouteState {
    const fn new() -> Self {
        Self {
            initialized: false,
            route_index: 0,
            stage_frames: 0,
            pending_destination: EpisodeMap::Start,
            pending_transition: 0,
            pending: false,
            train_checksum: u32::MAX,
        }
    }
}

static mut STATE: RouteState = RouteState::new();

pub fn map_loaded(map: EpisodeMap) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        if state.pending {
            if map != state.pending_destination || state.route_index + 1 >= ROUTE.len() {
                fail(
                    FAILURE_TRANSITION,
                    map_index(map),
                    map_index(state.pending_destination),
                );
                return;
            }
            state.route_index += 1;
            if ROUTE[state.route_index] != map {
                fail(FAILURE_BAD_MAP, map_index(map), state.route_index as u32);
                return;
            }
            if state.pending_transition != 0 {
                let field = addr_of_mut!((*addr_of_mut!(PROBE)).transitions);
                write_volatile(field, read_volatile(field) | state.pending_transition);
            }
            state.pending = false;
            state.pending_transition = 0;
        } else if state.initialized || map != ROUTE[0] {
            fail(FAILURE_BAD_MAP, map_index(map), state.route_index as u32);
            return;
        } else {
            state.initialized = true;
        }

        state.stage_frames = 0;
        let probe = addr_of_mut!(PROBE);
        let bit = 1 << map_index(map);
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | bit,
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | bit,
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
        write_volatile(addr_of_mut!((*probe).route_index), state.route_index as u32);
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).wrapping_add(1),
        );
        write_volatile(addr_of_mut!((*probe).stage_frames), 0);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_SETTLE);
    }
}

/// Advance the real trigger route. A returned map requests the one deliberate
/// direct reload used to revisit E1M4 and test its normal exit.
pub fn drive(
    world: &ResidentMap,
    entities: &EntityScene,
    player: &mut Player,
) -> Option<EpisodeMap> {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).total_frames),
            read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1),
        );

        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized
            || state.pending
            || read_volatile(addr_of_mut!((*probe).failure_code)) != 0
        {
            return None;
        }
        if world.map() != ROUTE[state.route_index] {
            fail(
                FAILURE_BAD_MAP,
                map_index(world.map()),
                state.route_index as u32,
            );
            return None;
        }
        if state.route_index + 1 == ROUTE.len() {
            write_volatile(addr_of_mut!((*probe).phase), PHASE_MAP_ROUTE_COMPLETE);
            return None;
        }

        if world.map() == TRAIN_MAP {
            observe_trains(entities);
        }

        state.stage_frames = state.stage_frames.wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), state.stage_frames);
        if state.stage_frames < SETTLE_FRAMES {
            return None;
        }

        let destination = ROUTE[state.route_index + 1];
        state.pending_destination = destination;
        state.pending_transition = transition_bit(state.route_index);
        state.pending = true;

        if state.route_index == 9 {
            return Some(destination);
        }
        let Some(origin) = entities.regression_change_level_origin(destination) else {
            state.pending = false;
            fail(
                FAILURE_CHANGE_LEVEL,
                map_index(world.map()),
                map_index(destination),
            );
            return None;
        };
        player.teleport(origin);
        None
    }
}

/// Retain the exact packet-arena high-water across the all-map route. This
/// reuses `weapon_pickups`, which the map-load probe otherwise leaves unused.
pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let high_water = addr_of_mut!((*probe).weapon_pickups);
        write_volatile(
            high_water,
            read_volatile(high_water).max(stats.packet_arena_words),
        );
        if stats.packet_overflow_avoided {
            let overflows = addr_of_mut!((*probe).target_edges);
            write_volatile(overflows, read_volatile(overflows).wrapping_add(1));
        }
    }
}

/// Watch E1M5's four self-starting trains: the longest leg the guest itself
/// computes, and how far the guest actually moved them.
fn observe_trains(entities: &EntityScene) {
    unsafe {
        let mut sample = [0u32; 3];
        entities.regression_train_probe(&mut sample);
        if sample[1] == 0 {
            return;
        }
        let probe = addr_of_mut!(PROBE);
        let longest = read_volatile(addr_of_mut!((*probe).state_ranges)).max(sample[0]);
        write_volatile(addr_of_mut!((*probe).state_ranges), longest);
        let state = &mut *addr_of_mut!(STATE);
        if state.train_checksum == u32::MAX {
            state.train_checksum = sample[2];
            return;
        }
        let travelled = sample[2].abs_diff(state.train_checksum);
        state.train_checksum = sample[2];
        let total =
            read_volatile(addr_of_mut!((*probe).valid_state_ranges)).saturating_add(travelled);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), total);
        // The one calculation the guest and the host disagree on, reported in
        // full so the failure names its own inputs instead of a theory.
        let mut leg = [0i32; 13];
        EntityScene::regression_train_leg_debug(&mut leg);
        write_volatile(addr_of_mut!((*probe).weapon_selected), leg[0] as u32);
        write_volatile(addr_of_mut!((*probe).weapon_fired), leg[1] as u32);
        write_volatile(addr_of_mut!((*probe).weapon_animated), leg[2] as u32);
        write_volatile(addr_of_mut!((*probe).monster_present), leg[3] as u32);
        write_volatile(addr_of_mut!((*probe).monster_animated), leg[4] as u32);
        write_volatile(addr_of_mut!((*probe).monster_state_bounds), leg[5] as u32);
        write_volatile(addr_of_mut!((*probe).monster_attack), leg[6] as u32);
        write_volatile(addr_of_mut!((*probe).monster_pain), leg[7] as u32);
        write_volatile(addr_of_mut!((*probe).monster_death), leg[8] as u32);
        write_volatile(addr_of_mut!((*probe).boss), leg[9] as u32);
        write_volatile(addr_of_mut!((*probe).last_health), leg[10] as u32);
        write_volatile(addr_of_mut!((*probe).shock_count), leg[11] as u32);
        write_volatile(addr_of_mut!((*probe).intermission_state), leg[12] as u32);
    }
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

const fn transition_bit(route_index: usize) -> u32 {
    match route_index {
        0..=8 => 1 << route_index,
        10 => 1 << 9,
        _ => 0,
    }
}

unsafe fn fail(code: u32, map: u32, detail: u32) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).failure_code), code);
        write_volatile(addr_of_mut!((*probe).failure_map), map);
        write_volatile(addr_of_mut!((*probe).failure_detail), detail);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_ERROR);
    }
}
