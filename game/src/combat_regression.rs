//! Emulator shotgun regression using cooked E1M1 data.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::atan2_q12;
use psx_math::int32::{isqrt_i32, square_i32_saturating};
use quake_core::combat::{WeaponState, WeaponView};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::player::Player;
use crate::renderer::RenderStats;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 3;
const E1M1_BIT: u32 = 1 << 1;
const SHOTGUN_BIT: u32 = 1 << 1;
const SETTLE_FRAMES: u32 = 8;
const MAX_FRAMES: u32 = 180;
const PHASE_SETTLE: u32 = 0x41;
const PHASE_FIRE: u32 = 0x42;
const PHASE_COMPLETE: u32 = 0x50;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_NO_CLEAR_TARGET: u32 = 6;
const FAILURE_NO_DAMAGE: u32 = 7;
const FAILURE_AUTHORED_BRUSH: u32 = 8;

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

struct State {
    initialized: bool,
    target_source: u16,
    initial_health: i16,
    stage_frames: u32,
}

impl State {
    const fn new() -> Self {
        Self {
            initialized: false,
            target_source: 0,
            initial_health: 0,
            stage_frames: 0,
        }
    }
}

static mut STATE: State = State::new();

pub fn setup(world: &ResidentMap, entities: &mut EntityScene, player: &mut Player) -> bool {
    unsafe {
        if world.map() != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, 1, 0);
            return false;
        }
        let (brush_flags, trigger_source, explobox_source) =
            entities.regression_authored_combat_brushes(world);
        if brush_flags != 0x7f {
            fail(FAILURE_AUTHORED_BRUSH, 1, brush_flags);
            return false;
        }
        let Some((source, eye, target)) = entities.regression_shot_setup(world) else {
            fail(FAILURE_NO_CLEAR_TARGET, 1, 0);
            return false;
        };
        let Some(health) = entities.regression_monster_health(source) else {
            fail(FAILURE_NO_CLEAR_TARGET, 1, u32::from(source));
            return false;
        };
        player.place_camera(eye, aim_angles(eye, target));
        let state = &mut *addr_of_mut!(STATE);
        state.initialized = true;
        state.target_source = source;
        state.initial_health = health;
        state.stage_frames = 0;

        let probe = addr_of_mut!(PROBE);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_SETTLE);
        write_volatile(addr_of_mut!((*probe).maps_loaded), E1M1_BIT);
        write_volatile(addr_of_mut!((*probe).maps_validated), E1M1_BIT);
        write_volatile(addr_of_mut!((*probe).map_loads), 1);
        write_volatile(addr_of_mut!((*probe).current_map), 1);
        write_volatile(
            addr_of_mut!((*probe).transitions),
            u32::from(trigger_source),
        );
        write_volatile(addr_of_mut!((*probe).boss), u32::from(explobox_source));
        write_volatile(addr_of_mut!((*probe).weapon_selected), SHOTGUN_BIT);
        write_volatile(addr_of_mut!((*probe).monster_present), E1M1_BIT);
        write_volatile(addr_of_mut!((*probe).monster_state_bounds), E1M1_BIT);
        write_volatile(addr_of_mut!((*probe).failure_entity), u32::from(source));
        write_volatile(addr_of_mut!((*probe).last_health), health as u32);
        true
    }
}

pub fn fire_held() -> bool {
    unsafe {
        let state = &*addr_of_mut!(STATE);
        state.initialized
            && state.stage_frames >= SETTLE_FRAMES
            && read_volatile(addr_of_mut!(PROBE.complete)) == 0
            && read_volatile(addr_of_mut!(PROBE.failure_code)) == 0
    }
}

pub fn observe(entities: &EntityScene, weapon: &WeaponState) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized {
            return;
        }
        state.stage_frames = state.stage_frames.wrapping_add(1);
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).total_frames),
            read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1),
        );
        write_volatile(addr_of_mut!((*probe).stage_frames), state.stage_frames);
        if state.stage_frames >= SETTLE_FRAMES
            && read_volatile(addr_of_mut!((*probe).phase)) == PHASE_SETTLE
        {
            write_volatile(addr_of_mut!((*probe).phase), PHASE_FIRE);
        }
        if weapon.shots_fired() != 0 {
            write_volatile(addr_of_mut!((*probe).weapon_fired), SHOTGUN_BIT);
        }
        let Some(health) = entities.regression_monster_health(state.target_source) else {
            fail(FAILURE_NO_CLEAR_TARGET, 1, u32::from(state.target_source));
            return;
        };
        write_volatile(
            addr_of_mut!((*probe).last_health),
            u32::from(health.max(0) as u16),
        );
        if health < state.initial_health && health > 0 {
            write_volatile(addr_of_mut!((*probe).monster_pain), E1M1_BIT);
        }
        if health <= 0 {
            write_volatile(addr_of_mut!((*probe).monster_death), E1M1_BIT);
            if read_volatile(addr_of_mut!((*probe).monster_pain)) == E1M1_BIT
                && read_volatile(addr_of_mut!((*probe).weapon_animated)) == SHOTGUN_BIT
            {
                write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
                write_volatile(addr_of_mut!((*probe).complete), 1);
            }
        } else if state.stage_frames > MAX_FRAMES {
            fail(
                FAILURE_NO_DAMAGE,
                1,
                ((weapon.shots_fired() & 0xffff) << 16) | u32::from(health as u16),
            );
        }
    }
}

/// Record animation coverage only when an animated shotgun frame actually
/// produced view-model packets. Weapon state advancement alone is not render
/// evidence and previously allowed this regression to pass with a missing or
/// fully rejected view model.
pub fn observe_render(stats: RenderStats, weapon: WeaponView) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if weapon.frame != 0 && stats.view_model_packets != 0 {
            write_volatile(addr_of_mut!((*probe).weapon_animated), SHOTGUN_BIT);
        }
        if stats.impact_particle_packets != 0 {
            write_volatile(addr_of_mut!((*probe).target_edges), 1);
        }
    }
}

fn aim_angles(from: quake_formats::Vec3I32, to: quake_formats::Vec3I32) -> [i16; 3] {
    let dx = (to.x.saturating_sub(from.x)) >> 12;
    let dy = (to.y.saturating_sub(from.y)) >> 12;
    let dz = (to.z.saturating_sub(from.z)) >> 12;
    let horizontal = isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dy)));
    [
        atan2_q12(-dz, horizontal) as i16,
        atan2_q12(dy, dx) as i16,
        0,
    ]
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
