//! Focused real-guest proof for E1M6's authored `trigger_monsterjump`.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::input::InputFrame;
use crate::player::Player;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 13;
const PHASE_FLIGHT: u32 = 0xb0;
const PHASE_COMPLETE: u32 = 0xb1;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_BAD_SOURCE: u32 = 2;
const FAILURE_TIMEOUT: u32 = 4;
const MAX_FRAMES: u32 = 360;

const TRIGGERED: u32 = 1 << 0;
const ROSE: u32 = 1 << 1;
const LANDED: u32 = 1 << 2;
const REQUIRED: u32 = TRIGGERED | ROSE | LANDED;

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
            phase: PHASE_FLIGHT,
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
static mut SOURCE: u16 = u16::MAX;
static mut ORIGINAL: quake_formats::Vec3I32 = quake_formats::Vec3I32 { x: 0, y: 0, z: 0 };
static mut START_Z: i32 = 0;
static mut EVIDENCE: u32 = 0;

pub const fn initial_map() -> EpisodeMap {
    EpisodeMap::E1M6
}

pub fn setup(world: &ResidentMap, entities: &mut EntityScene, player: &mut Player) -> bool {
    if world.map() != EpisodeMap::E1M6 {
        fail(FAILURE_BAD_MAP, 0, 0);
        return false;
    }
    let Some((source, original)) = entities.regression_stage_monsterjump(world) else {
        fail(FAILURE_BAD_SOURCE, 0, 0);
        return false;
    };
    if entities.regression_monster_snapshot(source).is_none() {
        fail(FAILURE_BAD_SOURCE, source, 1);
        return false;
    }
    // Look along the authored +X launch from behind the ogre. Keeping the
    // player behind also ensures the dynamic body sweep cannot end the flight.
    let eye = quake_formats::Vec3I32 {
        x: original.x.saturating_sub(96 << 12),
        y: original.y,
        z: original.z.saturating_add(25 << 12),
    };
    player.place_camera(eye, aim_angles(eye, original));
    unsafe {
        SOURCE = source;
        ORIGINAL = original;
        START_Z = original.z;
        EVIDENCE = 0;
        let probe = addr_of_mut!(PROBE);
        write_volatile(addr_of_mut!((*probe).maps_loaded), 1 << 6);
        write_volatile(addr_of_mut!((*probe).maps_validated), 1 << 6);
        write_volatile(addr_of_mut!((*probe).current_map), 6);
        write_volatile(addr_of_mut!((*probe).map_loads), 1);
        write_volatile(addr_of_mut!((*probe).route_index), u32::from(source));
        write_volatile(addr_of_mut!((*probe).monster_present), 1);
        write_volatile(addr_of_mut!((*probe).transitions), 192);
        write_volatile(
            addr_of_mut!((*probe).weapon_selected),
            (original.x >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).weapon_fired),
            (original.y >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).weapon_animated),
            (original.z >> 12) as u32,
        );
    }
    true
}

pub fn controls() -> InputFrame {
    InputFrame::default()
}

pub fn observe(world: &ResidentMap, entities: &mut EntityScene) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return;
        }
        let frames = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), frames);
        write_volatile(addr_of_mut!((*probe).stage_frames), frames);
        let Some(snapshot) = entities.regression_monster_snapshot(SOURCE) else {
            fail(FAILURE_BAD_SOURCE, SOURCE, 2);
            return;
        };
        if snapshot.leaping && snapshot.forced_jump {
            if EVIDENCE & TRIGGERED == 0
                && !entities.regression_restore_monsterjump(world, SOURCE, ORIGINAL)
            {
                fail(FAILURE_BAD_SOURCE, SOURCE, 3);
                return;
            }
            EVIDENCE |= TRIGGERED;
        }
        if EVIDENCE & TRIGGERED != 0 && snapshot.origin.z > START_Z.saturating_add(2 << 12) {
            EVIDENCE |= ROSE;
        }
        if EVIDENCE & ROSE != 0 && !snapshot.leaping && !snapshot.forced_jump {
            EVIDENCE |= LANDED;
        }
        write_volatile(addr_of_mut!((*probe).monster_animated), EVIDENCE);
        write_volatile(
            addr_of_mut!((*probe).monster_state_bounds),
            (snapshot.origin.z >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).monster_attack),
            u32::from(snapshot.leaping) | (u32::from(snapshot.forced_jump) << 1),
        );
        write_volatile(
            addr_of_mut!((*probe).monster_pain),
            (snapshot.origin.x >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).monster_death),
            (snapshot.origin.y >> 12) as u32,
        );
        write_volatile(
            addr_of_mut!((*probe).state_ranges),
            u32::from(snapshot.frame),
        );
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), REQUIRED);
        if EVIDENCE == REQUIRED {
            write_volatile(addr_of_mut!((*probe).player_state), EVIDENCE);
            write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
            write_volatile(addr_of_mut!((*probe).complete), 1);
        } else if frames > MAX_FRAMES {
            fail(FAILURE_TIMEOUT, SOURCE, EVIDENCE);
        }
    }
}

fn aim_angles(from: quake_formats::Vec3I32, to: quake_formats::Vec3I32) -> [i16; 3] {
    let dx = (to.x.saturating_sub(from.x)) >> 12;
    let dy = (to.y.saturating_sub(from.y)) >> 12;
    let dz = (to.z.saturating_sub(from.z)) >> 12;
    let horizontal = psx_math::int32::isqrt_i32(
        psx_math::int32::square_i32_saturating(dx)
            .saturating_add(psx_math::int32::square_i32_saturating(dy)),
    );
    [
        psx_math::atan2_q12(-dz, horizontal) as i16,
        psx_math::atan2_q12(dy, dx) as i16,
        0,
    ]
}

fn fail(code: u32, entity: u16, detail: u32) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).failure_code), code);
        write_volatile(addr_of_mut!((*probe).failure_map), 6);
        write_volatile(addr_of_mut!((*probe).failure_entity), u32::from(entity));
        write_volatile(addr_of_mut!((*probe).failure_detail), detail);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_ERROR);
    }
}
