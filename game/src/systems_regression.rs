//! Deterministic authored-map probe for the entity-graph systems.
//!
//! Start's three authored `misc_fireball` spouts lob lava balls that arc and
//! impact, driven only by the shipping gameplay loop and never by a helper
//! that moves the player: the spouts run themselves exactly like the original.
//!
//! The load also reads the authored `trigger_secret` census the secret counter
//! builds and the skill the entity loader used, so a map that stops cooking
//! its secrets, or a skill that drifts off Easy, fails here.
//!
//! An E1M5 `func_train` stage was written and then removed: the trains ride
//! their authored corner chains correctly under host tests but compute a leg
//! length of 27804 ticks on the guest where the host computes 87, so the gate
//! cannot pin them until that divergence is root-caused.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use crate::asset::EpisodeMap;
use crate::entity::{FireballResult, GameplayResult};
use crate::input::InputFrame;

const PROBE_MAGIC: u32 = 0x5150_5358;
// Version 8: the merged branch already spends 3 through 7 on the map,
// combat, arsenal, monster, Start route and E1M1 chain probes.
const PROBE_VERSION: u32 = 8;
const PHASE_FIREBALLS: u32 = 1;
const PHASE_COMPLETE: u32 = 0x81;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_TARGET_GRAPH: u32 = 3;
const FAILURE_SECRET_CENSUS: u32 = 4;
const FAILURE_SKILL: u32 = 5;

/// Start authors three `misc_fireball` spouts, each re-arming between three
/// and eight seconds with a five second ball life.
const FIREBALL_LAUNCHES_REQUIRED: u32 = 3;
const FIREBALL_STAGE_FRAMES: u32 = 1_800;
/// Authored `trigger_secret` count on Easy, from Start's cooked entity lump.
const START_SECRETS: u32 = 0;

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
            phase: PHASE_FIREBALLS,
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    Fireballs,
    Complete,
}

#[used]
static mut PROBE: Probe = Probe::new();
static mut STAGE: Stage = Stage::Fireballs;

/// The gate boots straight into Start, whose lava spouts need no activation.
pub const fn initial_map() -> EpisodeMap {
    EpisodeMap::Start
}

pub fn map_loaded(map: EpisodeMap, secrets: (u16, u16), skill: u8) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let stage = &mut *addr_of_mut!(STAGE);
        let expected = match (*stage, map) {
            (Stage::Fireballs, EpisodeMap::Start) => START_SECRETS,
            _ => {
                fail(FAILURE_BAD_MAP, map_index(map), *stage as u32);
                return;
            }
        };
        if skill != 0 {
            fail(FAILURE_SKILL, map_index(map), u32::from(skill));
            return;
        }
        // `total_secrets` is counted once per spawned `trigger_secret`, so it
        // is a direct read of the authored census for this map and skill.
        if u32::from(secrets.1) != expected || secrets.0 != 0 {
            fail(
                FAILURE_SECRET_CENSUS,
                map_index(map),
                u32::from(secrets.1) | (u32::from(secrets.0) << 16),
            );
            return;
        }
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | 1 << map_index(map),
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | 1 << map_index(map),
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).wrapping_add(1),
        );
    }
}

/// The player never moves: both stages watch authored entities run on their
/// own, which is what the original does with an untriggered train and a lava
/// spout.
pub fn controls() -> InputFrame {
    InputFrame::default()
}

/// Advance the stage machine and report the next map to load, if any.
pub fn drive(
    map: EpisodeMap,
    gameplay: GameplayResult,
    fireballs: FireballResult,
) -> Option<EpisodeMap> {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let stage = &mut *addr_of_mut!(STAGE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 || *stage == Stage::Complete {
            return None;
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        let stage_frames = read_volatile(addr_of_mut!((*probe).stage_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), stage_frames);
        write_volatile(
            addr_of_mut!((*probe).target_edges),
            read_volatile(addr_of_mut!((*probe).target_edges))
                .saturating_add(u32::from(gameplay.fired_target_edges)),
        );
        if let Some(error) = gameplay.target_error {
            fail(FAILURE_TARGET_GRAPH, map_index(map), error as u32);
            return None;
        }

        match *stage {
            Stage::Fireballs => {
                if map != EpisodeMap::Start {
                    fail(FAILURE_BAD_MAP, map_index(map), *stage as u32);
                    return None;
                }
                let launched = read_volatile(addr_of_mut!((*probe).intermission_state))
                    .saturating_add(u32::from(fireballs.launched));
                let impacts = read_volatile(addr_of_mut!((*probe).boss))
                    .saturating_add(u32::from(fireballs.impacts));
                write_volatile(addr_of_mut!((*probe).intermission_state), launched);
                write_volatile(addr_of_mut!((*probe).boss), impacts);
                if launched >= FIREBALL_LAUNCHES_REQUIRED && impacts >= 1 {
                    write_volatile(addr_of_mut!((*probe).route_index), 1);
                    write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
                    write_volatile(addr_of_mut!((*probe).complete), 1);
                    *stage = Stage::Complete;
                }
                if stage_frames > FIREBALL_STAGE_FRAMES {
                    fail(FAILURE_TIMEOUT, map_index(map), launched | (impacts << 16));
                }
            }
            Stage::Complete => {}
        }
        None
    }
}

fn fail(code: u32, map: u32, detail: u32) {
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
