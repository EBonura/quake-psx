//! Headless Start-to-E1M1 route driven only through normal movement and map triggers.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::int32::mul_q12_i32;

use crate::asset::EpisodeMap;
use crate::entity::GameplayResult;
use crate::input::InputFrame;
use crate::player::Player;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 4;
const PHASE_MOVE_TO_EASY: u32 = 1;
const PHASE_ENTER_EASY_SLIPGATE: u32 = 2;
const PHASE_MOVE_TO_EPISODE_ONE: u32 = 3;
const PHASE_CLIMB_EPISODE_ONE: u32 = 4;
const PHASE_ENTER_EPISODE_ONE: u32 = 5;
const PHASE_COMPLETE: u32 = 0x31;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_TARGET_GRAPH: u32 = 3;
const FAILURE_SKILL: u32 = 4;
const FAILURE_TELEPORT: u32 = 5;
const MAX_ROUTE_FRAMES: u32 = 900;

const PLAYER_SKILL_TRIGGER: u32 = 1 << 0;
const PLAYER_SLIPGATE_TELEPORT: u32 = 1 << 1;
const PLAYER_E1_CHANGELEVEL: u32 = 1 << 2;

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
            phase: PHASE_MOVE_TO_EASY,
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
enum RouteStage {
    MoveToEasy,
    EnterEasySlipgate,
    MoveToEpisodeOne,
    ClimbEpisodeOne,
    EnterEpisodeOne,
    AwaitE1M1,
    Complete,
}

struct RouteState {
    initialized: bool,
    stage: RouteStage,
    stage_frames: u32,
}

impl RouteState {
    const fn new() -> Self {
        Self {
            initialized: false,
            stage: RouteStage::MoveToEasy,
            stage_frames: 0,
        }
    }
}

#[used]
static mut PROBE: Probe = Probe::new();
static mut STATE: RouteState = RouteState::new();

pub fn map_loaded(map: EpisodeMap) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        let probe = addr_of_mut!(PROBE);
        match (state.initialized, state.stage, map) {
            (false, RouteStage::MoveToEasy, EpisodeMap::Start) => {
                state.initialized = true;
                record_map(probe, EpisodeMap::Start);
            }
            (true, RouteStage::AwaitE1M1, EpisodeMap::E1M1) => {
                state.stage = RouteStage::Complete;
                record_map(probe, EpisodeMap::E1M1);
                write_volatile(addr_of_mut!((*probe).transitions), 1);
                write_volatile(addr_of_mut!((*probe).route_index), 1);
                write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
                write_volatile(addr_of_mut!((*probe).complete), 1);
                let player = read_volatile(addr_of_mut!((*probe).player_state));
                write_volatile(
                    addr_of_mut!((*probe).player_state),
                    player | PLAYER_E1_CHANGELEVEL,
                );
            }
            _ => fail(FAILURE_BAD_MAP, map_index(map), state.stage as u32),
        }
    }
}

pub fn controls(map: EpisodeMap, player: &Player) -> InputFrame {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        let probe = addr_of_mut!(PROBE);
        if !state.initialized
            || state.stage == RouteStage::Complete
            || read_volatile(addr_of_mut!((*probe).failure_code)) != 0
        {
            return InputFrame::default();
        }
        if map != EpisodeMap::Start {
            fail(FAILURE_BAD_MAP, map_index(map), state.stage as u32);
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        state.stage_frames = state.stage_frames.wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), state.stage_frames);
        if total > MAX_ROUTE_FRAMES {
            fail(FAILURE_TIMEOUT, map_index(map), state.stage as u32);
            return InputFrame::default();
        }

        let camera = player.camera();
        let x = camera.origin.x >> 12;
        let y = camera.origin.y >> 12;
        let z = camera.origin.z >> 12;
        write_volatile(addr_of_mut!((*probe).last_health), x as u32);
        write_volatile(addr_of_mut!((*probe).state_ranges), y as u32);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), z as u32);
        let mut input = InputFrame::default();
        match state.stage {
            RouteStage::MoveToEasy => {
                if x > 240 {
                    input.movement[1] = 127;
                } else {
                    set_stage(
                        state,
                        probe,
                        RouteStage::EnterEasySlipgate,
                        PHASE_ENTER_EASY_SLIPGATE,
                    );
                }
            }
            RouteStage::EnterEasySlipgate => {
                input.movement[0] = 127;
                input.movement[1] = x_correction(x, 232);
            }
            // `teleport_touch` now leaves the player with the destination's
            // authored `v_forward * 300`, so the arrival is a flying start
            // that lands off the destination's own lane and a fixed strafe
            // walks into the wall beside the slipgate. Steering to route
            // points instead reaches the Episode One gate from either arrival.
            RouteStage::MoveToEpisodeOne => {
                if approach(&mut input, player, x, y, -64, 1740, 24) {
                    set_stage(
                        state,
                        probe,
                        RouteStage::ClimbEpisodeOne,
                        PHASE_CLIMB_EPISODE_ONE,
                    );
                }
            }
            // The Episode One slipgate sits above the arrival floor; its ramp
            // climbs past the changelevel volume, so the route turns around at
            // the top and comes back down through it.
            RouteStage::ClimbEpisodeOne => {
                if approach(&mut input, player, x, y, -64, 1690, 20) {
                    set_stage(
                        state,
                        probe,
                        RouteStage::EnterEpisodeOne,
                        PHASE_ENTER_EPISODE_ONE,
                    );
                }
            }
            RouteStage::EnterEpisodeOne => {
                let _ = approach(&mut input, player, x, y, -64, 1628, 12);
            }
            RouteStage::AwaitE1M1 | RouteStage::Complete => {}
        }
        input
    }
}

pub fn observe(map: EpisodeMap, gameplay: GameplayResult) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        let probe = addr_of_mut!(PROBE);
        if map != EpisodeMap::Start || read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(
            addr_of_mut!((*probe).target_edges),
            read_volatile(addr_of_mut!((*probe).target_edges))
                .saturating_add(u32::from(gameplay.fired_target_edges)),
        );
        if let Some(error) = gameplay.target_error {
            fail(FAILURE_TARGET_GRAPH, map_index(map), error as u32);
            return;
        }
        if let Some(skill) = gameplay.selected_skill {
            if skill != 0 {
                fail(FAILURE_SKILL, map_index(map), u32::from(skill));
                return;
            }
            let player = read_volatile(addr_of_mut!((*probe).player_state));
            write_volatile(
                addr_of_mut!((*probe).player_state),
                player | PLAYER_SKILL_TRIGGER,
            );
        }
        if let Some(destination) = gameplay.teleport {
            let player = read_volatile(addr_of_mut!((*probe).player_state));
            let expected = PLAYER_SKILL_TRIGGER;
            let canonical_destination = destination.origin.x == 544 << 12
                && destination.origin.y == 1536 << 12
                && destination.origin.z == 43 << 12;
            if state.stage != RouteStage::EnterEasySlipgate
                || player & expected != expected
                || !canonical_destination
            {
                fail(
                    FAILURE_TELEPORT,
                    map_index(map),
                    u32::from(destination.source_index),
                );
                return;
            }
            write_volatile(
                addr_of_mut!((*probe).player_state),
                player | PLAYER_SLIPGATE_TELEPORT,
            );
            set_stage(
                state,
                probe,
                RouteStage::MoveToEpisodeOne,
                PHASE_MOVE_TO_EPISODE_ONE,
            );
        }
    }
}

pub fn transition_requested(map: EpisodeMap, destination: EpisodeMap) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        let probe = addr_of_mut!(PROBE);
        let player = read_volatile(addr_of_mut!((*probe).player_state));
        let prerequisites = PLAYER_SKILL_TRIGGER | PLAYER_SLIPGATE_TELEPORT;
        if map != EpisodeMap::Start
            || destination != EpisodeMap::E1M1
            || state.stage != RouteStage::EnterEpisodeOne
            || player & prerequisites != prerequisites
        {
            fail(FAILURE_BAD_MAP, map_index(map), map_index(destination));
            return;
        }
        // The shipping loop called `touched_change_level` after normal player
        // movement. No regression helper can manufacture this transition.
        state.stage = RouteStage::AwaitE1M1;
    }
}

/// Steer toward one route point and report arrival. Same taper the E1M1 chain
/// route uses: one route decision spans several fixed ticks, so full throttle
/// would cross a small acceptance window and orbit it forever.
fn approach(
    input: &mut InputFrame,
    player: &Player,
    x: i32,
    y: i32,
    target_x: i32,
    target_y: i32,
    radius: i32,
) -> bool {
    let dx = target_x.saturating_sub(x);
    let dy = target_y.saturating_sub(y);
    if dx.abs() <= radius && dy.abs() <= radius {
        return true;
    }
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    let limit = dx.abs().max(dy.abs()).saturating_mul(6).clamp(16, 127);
    input.movement = [
        (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
    ];
    false
}

fn x_correction(x: i32, target: i32) -> i16 {
    if x > target + 10 {
        127
    } else if x < target - 10 {
        -127
    } else {
        0
    }
}

unsafe fn set_stage(state: &mut RouteState, probe: *mut Probe, stage: RouteStage, phase: u32) {
    state.stage = stage;
    state.stage_frames = 0;
    unsafe {
        write_volatile(addr_of_mut!((*probe).stage_frames), 0);
        write_volatile(addr_of_mut!((*probe).phase), phase);
    }
}

unsafe fn record_map(probe: *mut Probe, map: EpisodeMap) {
    let bit = 1 << map_index(map);
    unsafe {
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | bit,
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | bit,
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).wrapping_add(1),
        );
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
