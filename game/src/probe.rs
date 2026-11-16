//! The probe block every regression gate publishes to RAM.
//!
//! The host driver finds it by `MAGIC` followed by the module's version (`read_probe_version` in
//! `host/quake-build/main.rs`) and reads the fields back. The 34 words are shared; each gate gives
//! the fields it needs their own meaning and documents that where it writes them, so a gate adds
//! its own phase constants, failure codes and a `PROBE_VERSION`, not another copy of this struct.

/// `"XSPQ"`: marks the block in a RAM dump.
pub const MAGIC: u32 = 0x5150_5358;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Probe {
    pub magic: u32,
    pub version: u32,
    pub complete: u32,
    pub phase: u32,
    pub failure_code: u32,
    pub failure_map: u32,
    pub failure_entity: u32,
    pub failure_detail: u32,
    pub total_frames: u32,
    pub maps_loaded: u32,
    pub maps_validated: u32,
    pub transitions: u32,
    pub weapon_selected: u32,
    pub weapon_fired: u32,
    pub weapon_animated: u32,
    pub monster_present: u32,
    pub monster_animated: u32,
    pub monster_state_bounds: u32,
    pub monster_attack: u32,
    pub monster_pain: u32,
    pub monster_death: u32,
    pub boss: u32,
    pub current_map: u32,
    pub route_index: u32,
    pub last_health: u32,
    pub state_ranges: u32,
    pub valid_state_ranges: u32,
    pub map_loads: u32,
    pub stage_frames: u32,
    pub shock_count: u32,
    pub intermission_state: u32,
    pub player_state: u32,
    pub weapon_pickups: u32,
    pub target_edges: u32,
}

impl Probe {
    /// A zeroed block for a gate at `version`, starting in `phase`. No map is current yet.
    pub const fn new(version: u32, phase: u32) -> Self {
        Self {
            magic: MAGIC,
            version,
            complete: 0,
            phase,
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
