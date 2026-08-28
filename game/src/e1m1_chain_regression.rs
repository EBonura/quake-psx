//! Normal-input E1M1 route from the map's own spawn to the map's own exit.
//!
//! The chain half walks the authored progression: the `t1` lift, the `t2`
//! bridge, the four ordered spiral lamps, the three `wait -1` counter buttons
//! in order, the completed `trigger_counter`, the `t10` exit door, and the
//! crossing through its doorway. The route pins both targeted-door messages
//! while they are armed, then proves each disappears when its door fires.
//! The exit half also crosses the `t15` shortcut trigger and proves firing the
//! door permanently disarms its touch-only message, matching the original
//! QuakeC.
//!
//! The exit half then walks the slipgate corridor behind that door all the
//! way into E1M1's own `trigger_changelevel` and lets the shipping map-load
//! path carry the player into E1M2. That makes this a complete per-map route:
//! it starts at `info_player_start` and finishes on the authored edge to the
//! next map, with nothing placed and nothing skipped.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::int32::mul_q12_i32;
use psx_pad::button;

use crate::asset::EpisodeMap;
use crate::entity::{EntityScene, GameplayResult};
use crate::input::InputFrame;
use crate::player::Player;
#[cfg(any(
    feature = "renderer-topology-cache",
    feature = "renderer-indexed-projection",
    feature = "renderer-subdivision-cache",
    feature = "renderer-scene-object-gate"
))]
use crate::renderer::RenderStats;

const PROBE_MAGIC: u32 = 0x5150_5358;
// Version 9 also pins the t15 shortcut door's one-shot message semantics.
const PROBE_VERSION: u32 = 9;
const PHASE_ROUTE: u32 = 1;
const PHASE_COMPLETE: u32 = 0x51;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_TARGET_GRAPH: u32 = 3;
const FAILURE_CHAIN_ORDER: u32 = 5;
/// The chain half used most of 3,600; the slipgate corridor to the exit
/// volume is another long walk on top of it.
const MAX_ROUTE_FRAMES: u32 = 6_000;

const DOOR_MESSAGES_ARMED: u32 = 1 << 0;
const BUTTON_213: u32 = 1 << 1;
const BUTTON_211: u32 = 1 << 2;
const BUTTON_212: u32 = 1 << 3;
const COUNTER_T9: u32 = 1 << 4;
const DOOR_T10: u32 = 1 << 5;
const CROSSED_T10: u32 = 1 << 6;
const ACCESS_T1: u32 = 1 << 7;
const BRIDGE_T2: u32 = 1 << 8;
const GATE_T11: u32 = 1 << 9;
const GATE_T12: u32 = 1 << 10;
const GATE_T13: u32 = 1 << 11;
const GATE_T14: u32 = 1 << 12;
/// The authored `trigger_changelevel` fired and E1M2 loaded through the
/// shipping map-load path.
const EXIT_E1M2: u32 = 1 << 13;
/// The late slipgate corridor crossed `trigger_once` #70, opening the t15
/// shortcut and permanently disarming its touch-only message.
const DOOR_T15: u32 = 1 << 14;
const SOUND_BUTTON: u32 = 1 << 0;
const SOUND_DOOR_MOVE: u32 = 1 << 1;
const SOUND_DOOR_STOP: u32 = 1 << 2;
const SOUND_PLAT_MOVE: u32 = 1 << 3;
const SOUND_PLAT_STOP: u32 = 1 << 4;
const REQUIRED_ROUTE_MOVER_SOUNDS: u32 = SOUND_BUTTON | SOUND_DOOR_MOVE | SOUND_DOOR_STOP;
const REQUIRED_MECHANISMS: u32 = DOOR_MESSAGES_ARMED
    | BUTTON_213
    | BUTTON_211
    | BUTTON_212
    | COUNTER_T9
    | DOOR_T10
    | CROSSED_T10
    | ACCESS_T1
    | BRIDGE_T2
    | GATE_T11
    | GATE_T12
    | GATE_T13
    | GATE_T14;
/// Everything the whole per-map route has to have in hand by the end: the
/// authored chain plus the crossing into E1M2 through E1M1's own exit volume.
const FULL_ROUTE: u32 = REQUIRED_MECHANISMS | DOOR_T15 | EXIT_E1M2;

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
            phase: PHASE_ROUTE,
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

#[derive(Copy, Clone)]
struct Waypoint {
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    jump: bool,
    require: u32,
    /// Mechanism bits recorded on leaving this point. The doorway crossing is
    /// a position, not an event the runtime reports, so the route is what
    /// witnesses it.
    records: u32,
}

const fn waypoint(x: i32, y: i32) -> Waypoint {
    Waypoint {
        x,
        y,
        z: i32::MIN,
        radius: 20,
        jump: false,
        require: 0,
        records: 0,
    }
}

const ROUTE: &[Waypoint] = &[
    waypoint(432, -352),
    waypoint(432, 16),
    waypoint(368, 16),
    waypoint(368, 64),
    waypoint(336, 64),
    waypoint(336, 528),
    waypoint(96, 528),
    waypoint(0, 576),
    Waypoint {
        x: -45,
        y: 576,
        z: i32::MIN,
        radius: 8,
        jump: false,
        records: 0,
        require: ACCESS_T1,
    },
    Waypoint {
        x: 0,
        y: 576,
        z: -207,
        radius: 16,
        jump: false,
        records: 0,
        require: ACCESS_T1,
    },
    waypoint(0, 672),
    waypoint(80, 720),
    waypoint(80, 1040),
    waypoint(80, 1072),
    waypoint(80, 1360),
    waypoint(80, 1408),
    waypoint(128, 1696),
    waypoint(128, 1856),
    waypoint(-144, 1856),
    waypoint(-152, 2000),
    waypoint(-152, 2080),
    waypoint(-152, 2112),
    waypoint(-152, 2480),
    waypoint(-152, 2592),
    waypoint(-152, 2720),
    waypoint(-48, 2720),
    Waypoint {
        x: -48,
        y: 2680,
        z: i32::MIN,
        radius: 9,
        jump: false,
        records: 0,
        require: BRIDGE_T2,
    },
    waypoint(-16, 2720),
    waypoint(0, 2720),
    waypoint(352, 2720),
    waypoint(640, 2784),
    waypoint(800, 2736),
    waypoint(848, 2544),
    waypoint(864, 2448),
    waypoint(864, 2128),
    Waypoint {
        x: 800,
        y: 2008,
        z: i32::MIN,
        radius: 9,
        jump: false,
        records: 0,
        require: BUTTON_213,
    },
    Waypoint {
        x: 1040,
        y: 2048,
        z: i32::MIN,
        radius: 20,
        jump: true,
        records: 0,
        require: 0,
    },
    waypoint(1240, 2048),
    Waypoint {
        x: 1276,
        y: 2048,
        z: i32::MIN,
        radius: 9,
        jump: false,
        records: 0,
        require: BUTTON_211,
    },
    waypoint(1232, 2136),
    waypoint(1232, 2240),
    waypoint(1232, 2352),
    waypoint(1232, 2464),
    Waypoint {
        x: 1232,
        y: 2492,
        z: i32::MIN,
        radius: 9,
        jump: false,
        records: 0,
        require: BUTTON_212 | COUNTER_T9 | DOOR_T10,
    },
    waypoint(1160, 2464),
    waypoint(1000, 2448),
    waypoint(840, 2448),
    waypoint(840, 2320),
    waypoint(840, 2096),
    waypoint(840, 1960),
    waypoint(864, 1904),
    waypoint(800, 1904),
    waypoint(848, 1904),
    Waypoint {
        x: 848,
        y: 1824,
        z: i32::MIN,
        radius: 20,
        jump: false,
        records: CROSSED_T10,
        require: REQUIRED_MECHANISMS & !(CROSSED_T10 | EXIT_E1M2),
    },
    // The slipgate corridor behind the exit door, walked to E1M1's own
    // `trigger_changelevel` (cooked brush x 1289..1335, y 521..567,
    // z -279..-129). Every point is `routesim path` output over the cooked
    // hull from the doorway to that volume, and the corridor climbs about
    // 170 units on the way, so the legs are long and the acceptance radii
    // stay wide enough to absorb the ramp.
    waypoint(1248, 1696),
    waypoint(1264, 1664),
    waypoint(1264, 1344),
    waypoint(1264, 1216),
    waypoint(1296, 560),
    Waypoint {
        x: 1312,
        y: 544,
        z: i32::MIN,
        radius: 24,
        jump: false,
        records: 0,
        require: EXIT_E1M2,
    },
];

#[used]
static mut PROBE: Probe = Probe::new();
static mut ROUTE_INDEX: usize = 0;
/// Bounded detour state. Dynamic body blocking made the authored E1M1 dog at
/// (88, 1520, -200) a real obstacle: its live body sits exactly across the
/// straight leg from (80, 1408) to (128, 1696), and the route stalled at
/// y = 1471, one unit outside the dog box expanded by the player hull
/// (1520 - 32 - 16 = 1472). A player walks around it, so the route does too:
/// when the approach stops shrinking, the heading steps through a bounded fan
/// of offsets. No waypoint moved and no mechanism requirement changed.
static mut STALL_FRAMES: u32 = 0;
static mut DETOUR: usize = 0;
static mut BEST_DISTANCE: i32 = i32::MAX;

/// Accumulate resident-packet cache efficacy in fields unused by this route.
/// Keeping the shared 136-byte probe shape lets the existing PSoXide host
/// reader report the experiment without adding debug I/O to timed frames.
#[cfg(feature = "renderer-topology-cache")]
pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        for (field, value) in [
            (
                addr_of_mut!((*probe).monster_present),
                stats.topology_cache_hits,
            ),
            (
                addr_of_mut!((*probe).monster_animated),
                stats.topology_cache_misses,
            ),
            (
                addr_of_mut!((*probe).monster_state_bounds),
                stats.topology_invariant_hit_slots,
            ),
            (
                addr_of_mut!((*probe).monster_attack),
                stats.topology_invariant_miss_slots,
            ),
        ] {
            write_volatile(field, read_volatile(field).wrapping_add(value));
        }
    }
}

/// Accumulate exact subdivision-slab activity in route-unused fields. The
/// host labels these values separately for the cache benchmark; keeping the
/// established probe ABI avoids debug output in the timed renderer.
#[cfg(feature = "renderer-subdivision-cache")]
pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        for (field, value) in [
            (
                addr_of_mut!((*probe).monster_present),
                stats.subdivision_cache_hits,
            ),
            (
                addr_of_mut!((*probe).monster_animated),
                stats.subdivision_cache_allocations,
            ),
            (
                addr_of_mut!((*probe).monster_state_bounds),
                stats.subdivision_cache_replacements,
            ),
            (
                addr_of_mut!((*probe).monster_attack),
                stats.subdivision_cache_fallbacks,
            ),
            (
                addr_of_mut!((*probe).monster_pain),
                stats.subdivision_cache_initializations,
            ),
            (
                addr_of_mut!((*probe).monster_death),
                stats.subdivision_cache_packets,
            ),
        ] {
            write_volatile(field, read_volatile(field).wrapping_add(value));
        }
    }
}

/// Accumulate the actual number of per-corner projections replaced by dense
/// shared-position projections. These route-unused fields preserve the probe
/// ABI and keep timed frames free of debug output.
#[cfg(feature = "renderer-indexed-projection")]
pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        for (field, value) in [
            (
                addr_of_mut!((*probe).monster_pain),
                stats.indexed_projection_corners,
            ),
            (
                addr_of_mut!((*probe).monster_death),
                stats.indexed_projection_unique,
            ),
        ] {
            write_volatile(field, read_volatile(field).wrapping_add(value));
        }
    }
}

/// Accumulate cooker scene-object gate work without timed debug output.
#[cfg(feature = "renderer-scene-object-gate")]
pub fn observe_render(stats: RenderStats) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        for (field, value) in [
            (
                addr_of_mut!((*probe).monster_pain),
                stats.scene_object_tests,
            ),
            (
                addr_of_mut!((*probe).monster_death),
                stats.scene_object_rejected_faces,
            ),
        ] {
            write_volatile(field, read_volatile(field).wrapping_add(value));
        }
    }
}

pub fn map_loaded(map: EpisodeMap) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        match map {
            EpisodeMap::E1M1 => {
                write_volatile(addr_of_mut!((*probe).maps_loaded), 0x002);
                write_volatile(addr_of_mut!((*probe).maps_validated), 0x002);
                write_volatile(addr_of_mut!((*probe).current_map), 1);
                write_volatile(addr_of_mut!((*probe).map_loads), 1);
            }
            // The map E1M1's own `trigger_changelevel` names. Reaching it is
            // the route's last mechanism, and only the shipping map-load path
            // can deliver it.
            EpisodeMap::E1M2 => {
                let state = read_volatile(addr_of_mut!((*probe).player_state));
                if state & CROSSED_T10 == 0 {
                    fail(FAILURE_CHAIN_ORDER, map_index(map), state);
                    return;
                }
                let sounds = read_volatile(addr_of_mut!((*probe).weapon_selected));
                if sounds & REQUIRED_ROUTE_MOVER_SOUNDS != REQUIRED_ROUTE_MOVER_SOUNDS {
                    fail(FAILURE_CHAIN_ORDER, map_index(map), sounds);
                    return;
                }
                write_volatile(addr_of_mut!((*probe).player_state), state | EXIT_E1M2);
                write_volatile(addr_of_mut!((*probe).maps_loaded), 0x006);
                write_volatile(addr_of_mut!((*probe).maps_validated), 0x006);
                write_volatile(addr_of_mut!((*probe).current_map), 2);
                write_volatile(addr_of_mut!((*probe).map_loads), 2);
                write_volatile(addr_of_mut!((*probe).transitions), 1);
            }
            _ => fail(FAILURE_BAD_MAP, map_index(map), 0),
        }
    }
}

pub fn controls(map: EpisodeMap, player: &Player) -> InputFrame {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return InputFrame::default();
        }
        // E1M2 is the map E1M1's exit hands over to. The route's last
        // waypoint waits on that arrival, so once it happens the loop below
        // has one waypoint left to retire and does it standing still.
        if map == EpisodeMap::E1M2 {
            let state = read_volatile(addr_of_mut!((*probe).player_state));
            if state & FULL_ROUTE != FULL_ROUTE {
                fail(FAILURE_CHAIN_ORDER, map_index(map), state);
                return InputFrame::default();
            }
            write_volatile(addr_of_mut!((*probe).route_index), ROUTE.len() as u32);
            write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
            write_volatile(addr_of_mut!((*probe).complete), 1);
            return InputFrame::default();
        }
        if map != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, map_index(map), 0);
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        write_volatile(addr_of_mut!((*probe).stage_frames), total);
        if total > MAX_ROUTE_FRAMES {
            fail(FAILURE_TIMEOUT, map_index(map), ROUTE_INDEX as u32);
            return InputFrame::default();
        }

        let camera = player.camera();
        let x = camera.origin.x >> 12;
        let y = camera.origin.y >> 12;
        let z = (camera.origin.z >> 12) - 22;
        write_volatile(addr_of_mut!((*probe).last_health), x as u32);
        write_volatile(addr_of_mut!((*probe).state_ranges), y as u32);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), z as u32);
        write_volatile(
            addr_of_mut!((*probe).failure_entity),
            u32::from(crate::entity::regression_last_player_collision_source()),
        );

        while let Some(target) = ROUTE.get(ROUTE_INDEX).copied() {
            let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
            if target.z == i32::MIN
                && target.require.count_ones() == 1
                && mechanisms & target.require == target.require
            {
                ROUTE_INDEX += 1;
                write_volatile(addr_of_mut!((*probe).route_index), ROUTE_INDEX as u32);
                BEST_DISTANCE = i32::MAX;
                STALL_FRAMES = 0;
                DETOUR = 0;
                continue;
            }
            let dx = target.x.saturating_sub(x);
            let dy = target.y.saturating_sub(y);
            let wrong_height = target.z != i32::MIN && (target.z - z).abs() > 4;
            if dx.abs() > target.radius || dy.abs() > target.radius || wrong_height {
                let distance = dx.abs().saturating_add(dy.abs());
                if distance < BEST_DISTANCE {
                    BEST_DISTANCE = distance;
                    STALL_FRAMES = 0;
                    DETOUR = 0;
                } else {
                    STALL_FRAMES += 1;
                    if STALL_FRAMES > 45 {
                        STALL_FRAMES = 0;
                        DETOUR = (DETOUR + 1) & 7;
                    }
                }
                let (dx, dy) = detoured(dx, dy, DETOUR);
                return movement_input(player, dx, dy, target.jump && total & 7 < 2);
            }
            if mechanisms & target.require != target.require {
                // ACCESS_T1 waits at the lift button, which answers a use
                // press exactly like a touch.
                return if target.require == ACCESS_T1 {
                    InputFrame {
                        pressed: button::SQUARE,
                        ..InputFrame::default()
                    }
                } else {
                    InputFrame::default()
                };
            }
            if target.records != 0 {
                write_volatile(
                    addr_of_mut!((*probe).player_state),
                    mechanisms | target.records,
                );
            }
            ROUTE_INDEX += 1;
            write_volatile(addr_of_mut!((*probe).route_index), ROUTE_INDEX as u32);
            BEST_DISTANCE = i32::MAX;
            STALL_FRAMES = 0;
            DETOUR = 0;
        }

        let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
        if mechanisms & FULL_ROUTE != FULL_ROUTE {
            fail(FAILURE_CHAIN_ORDER, map_index(map), mechanisms);
            return InputFrame::default();
        }
        write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
        write_volatile(addr_of_mut!((*probe).complete), 1);
        InputFrame::default()
    }
}

pub fn observe(map: EpisodeMap, gameplay: GameplayResult, entities: &EntityScene) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if map != EpisodeMap::E1M1 || read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
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
        let mut sounds = read_volatile(addr_of_mut!((*probe).weapon_selected));
        for sound in gameplay.mover_sounds() {
            sounds |= match sound.id() {
                quake_core::mover::BUTTON_ACTIVATE_SOUND => SOUND_BUTTON,
                quake_core::mover::DOOR_MOVE_SOUND => SOUND_DOOR_MOVE,
                quake_core::mover::DOOR_STOP_SOUND => SOUND_DOOR_STOP,
                quake_core::mover::PLAT_MOVE_SOUND => SOUND_PLAT_MOVE,
                quake_core::mover::PLAT_STOP_SOUND => SOUND_PLAT_STOP,
                _ => 0,
            };
        }
        write_volatile(addr_of_mut!((*probe).weapon_selected), sounds);
        let mut mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
        if mechanisms & DOOR_MESSAGES_ARMED == 0 {
            let t10 = entities.regression_mover_message(56);
            let t15 = entities.regression_mover_message(69);
            if !matches!(t10, Some(message) if message != 0)
                || !matches!(t15, Some(message) if message != 0)
            {
                fail(FAILURE_TARGET_GRAPH, map_index(map), 56);
                return;
            }
            mechanisms |= DOOR_MESSAGES_ARMED;
        }
        // A 15 Hz route frame spans several fixed ticks, so the third button
        // press, its top arrival, the counter completion, and the exit-door
        // activation can all arrive in one gameplay result. Walk the
        // activations in engine order and fold the counter completion in as
        // soon as every button is down so the door prerequisite stays strict.
        let mut counters_pending = gameplay.completed_counters;
        for source_index in gameplay.player_activated_movers.iter().copied().flatten() {
            // At faster presentation rates the third button, counter and door
            // may span two gameplay results. Source 56 is reachable only after
            // all three permanent buttons complete t9.
            if source_index == 56 {
                let buttons = BUTTON_213 | BUTTON_211 | BUTTON_212;
                if mechanisms & buttons == buttons {
                    mechanisms |= COUNTER_T9;
                    counters_pending = 0;
                }
            }
            let required_before = match source_index {
                54 => {
                    mechanisms |= BUTTON_213;
                    0
                }
                52 => {
                    mechanisms |= BUTTON_211;
                    BUTTON_213
                }
                53 => {
                    mechanisms |= BUTTON_212;
                    BUTTON_213 | BUTTON_211
                }
                56 => {
                    if entities.regression_mover_message(56) != Some(0) {
                        fail(FAILURE_TARGET_GRAPH, map_index(map), 56);
                        return;
                    }
                    mechanisms |= DOOR_T10;
                    DOOR_MESSAGES_ARMED | BUTTON_213 | BUTTON_211 | BUTTON_212 | COUNTER_T9
                }
                6 => {
                    mechanisms |= ACCESS_T1;
                    0
                }
                11 => {
                    mechanisms |= BRIDGE_T2;
                    0
                }
                61 => {
                    mechanisms |= GATE_T11;
                    0
                }
                62 => {
                    mechanisms |= GATE_T12;
                    GATE_T11
                }
                63 => {
                    mechanisms |= GATE_T13;
                    GATE_T11 | GATE_T12
                }
                64 => {
                    mechanisms |= GATE_T14;
                    GATE_T11 | GATE_T12 | GATE_T13
                }
                69 => {
                    if entities.regression_mover_message(69) != Some(0) {
                        fail(FAILURE_TARGET_GRAPH, map_index(map), 69);
                        return;
                    }
                    mechanisms |= DOOR_T15;
                    DOOR_MESSAGES_ARMED | CROSSED_T10
                }
                _ => 0,
            };
            if mechanisms & required_before != required_before {
                fail(FAILURE_CHAIN_ORDER, map_index(map), u32::from(source_index));
                return;
            }
            if counters_pending != 0 {
                let buttons = BUTTON_213 | BUTTON_211 | BUTTON_212;
                if mechanisms & buttons == buttons {
                    if counters_pending != 1 {
                        fail(
                            FAILURE_CHAIN_ORDER,
                            map_index(map),
                            u32::from(counters_pending),
                        );
                        return;
                    }
                    mechanisms |= COUNTER_T9;
                    counters_pending = 0;
                }
            }
        }
        if counters_pending != 0 {
            let buttons = BUTTON_213 | BUTTON_211 | BUTTON_212;
            if counters_pending != 1 || mechanisms & buttons != buttons {
                fail(
                    FAILURE_CHAIN_ORDER,
                    map_index(map),
                    u32::from(counters_pending),
                );
                return;
            }
            mechanisms |= COUNTER_T9;
        }
        write_volatile(addr_of_mut!((*probe).player_state), mechanisms);
    }
}

/// Rotate an approach vector by one of eight bounded offsets. Offset zero is
/// the straight authored approach; the rest let the route slide past a solid
/// body without moving a waypoint.
fn detoured(dx: i32, dy: i32, detour: usize) -> (i32, i32) {
    const OFFSETS: [u16; 8] = [0, 512, 3_584, 1_024, 3_072, 1_536, 2_560, 2_048];
    let offset = OFFSETS[detour & 7];
    if offset == 0 {
        return (dx, dy);
    }
    let cos = psx_math::cos_q12(offset);
    let sin = psx_math::sin_q12(offset);
    (
        mul_q12_i32(cos, dx).saturating_sub(mul_q12_i32(sin, dy)),
        mul_q12_i32(sin, dx).saturating_add(mul_q12_i32(cos, dy)),
    )
}

fn movement_input(player: &Player, dx: i32, dy: i32, jump: bool) -> InputFrame {
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    // One route decision spans several 60 Hz ticks, so full throttle covers
    // about 21 units per decision. Taper the approach with distance or a
    // single step can cross a small waypoint's whole acceptance window and
    // orbit it forever.
    let limit = dx.abs().max(dy.abs()).saturating_mul(6).clamp(16, 127);
    InputFrame {
        movement: [
            (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
            (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        ],
        held: if jump { button::CROSS } else { 0 },
        ..InputFrame::default()
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
