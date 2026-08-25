//! Ordinary-input E1M2 and E1M3 spawn-to-exit proof, ending after E1M4 loads.
//!
//! This probe starts at E1M2's cooked `info_player_start`, earns every mover
//! state through the shipping guest, and crosses both maps' own changelevels.
//! `tools/routesim` supplied static walkable legs and the original demo1 route
//! supplied E1M3's authored corridor choices; the guest remains the authority
//! for lifts, buttons, keys, target graphs, weapons, teleports and map loads.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::{atan2_q12, int32::isqrt_i32, int32::mul_q12_i32};
use psx_pad::button;

use crate::asset::EpisodeMap;
use crate::entity::{EntityScene, GameplayResult, PickupResult};
use crate::input::InputFrame;
use crate::player::Player;
use quake_core::combat::WeaponState;
use quake_core::mover::QuakeMoverState;
use quake_formats::Vec3I32;

const PROBE_MAGIC: u32 = 0x5150_5358;
// Version 14 extends the proof through E1M3 and into E1M4.
const PROBE_VERSION: u32 = 14;
const PHASE_E1M2: u32 = 2;
const PHASE_E1M3: u32 = 3;
const PHASE_COMPLETE: u32 = 0x23;
const PHASE_ERROR: u32 = 0xff;

const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_TARGET_GRAPH: u32 = 3;
const FAILURE_MECHANISM: u32 = 4;
const FAILURE_USE_OPENED_SHOOTABLE: u32 = 5;
const FAILURE_PLAYER_DEAD: u32 = 6;
const FAILURE_BUTTON_STATE: u32 = 7;

const MAX_TOTAL_FRAMES: u32 = 14_000;
const MAX_STAGE_FRAMES: u32 = 2_000;

const E1M2_LIFT: u32 = 1 << 0;
const E1M2_USE_REJECTED: u32 = 1 << 1;
const E1M2_BUTTON_SHOT: u32 = 1 << 2;
const E1M2_TARGET_77: u32 = 1 << 3;
const E1M2_BUTTON_HOLD_OPEN: u32 = 1 << 4;
const E1M2_U_BEND_GATE: u32 = 1 << 5;
const E1M2_KEY_BRIDGE: u32 = 1 << 6;
const E1M2_KEY_42: u32 = 1 << 7;
const E1M2_KEY_TARGET_68: u32 = 1 << 8;
const E1M2_SILVER_DOOR: u32 = 1 << 9;
const E1M2_FLOORPLATE: u32 = 1 << 10;
const E1M2_EXIT_DOOR: u32 = 1 << 11;
const E1M2_CHANGELEVEL: u32 = 1 << 12;
const E1M3_GOLD_KEY: u32 = 1 << 13;
const E1M3_KEY_LIFT_TRIGGERED: u32 = 1 << 14;
const E1M3_KEY_LIFT_RIDDEN: u32 = 1 << 15;
const E1M3_STAIR_BUTTON: u32 = 1 << 16;
const E1M3_STAIRCASE: u32 = 1 << 17;
const E1M3_GOLD_DOOR: u32 = 1 << 18;
const E1M3_SECRET_DOORS: u32 = 1 << 19;
const E1M3_TRAIN: u32 = 1 << 20;
const E1M3_TRAPDOOR: u32 = 1 << 21;
const E1M3_UNDERWATER_GATE: u32 = 1 << 22;
const E1M3_CORRIDOR_BUTTON: u32 = 1 << 23;
const E1M3_END_LIFT_TRIGGERED: u32 = 1 << 24;
const E1M3_END_BUTTON: u32 = 1 << 25;
const E1M3_END_LIFT_RIDDEN: u32 = 1 << 26;
const E1M3_CHANGELEVEL: u32 = 1 << 27;
const E1M3_GOLD_LIFT_DOWN: u32 = 1 << 28;

const E1M2_REQUIRED: u32 = E1M2_LIFT
    | E1M2_USE_REJECTED
    | E1M2_BUTTON_SHOT
    | E1M2_TARGET_77
    | E1M2_BUTTON_HOLD_OPEN
    | E1M2_U_BEND_GATE
    | E1M2_KEY_BRIDGE
    | E1M2_KEY_42
    | E1M2_KEY_TARGET_68
    | E1M2_SILVER_DOOR
    | E1M2_FLOORPLATE
    | E1M2_EXIT_DOOR
    | E1M2_CHANGELEVEL;

const E1M3_REQUIRED: u32 = E1M3_GOLD_KEY
    | E1M3_KEY_LIFT_TRIGGERED
    | E1M3_KEY_LIFT_RIDDEN
    | E1M3_STAIR_BUTTON
    | E1M3_STAIRCASE
    | E1M3_GOLD_DOOR
    | E1M3_SECRET_DOORS
    | E1M3_TRAIN
    | E1M3_TRAPDOOR
    | E1M3_UNDERWATER_GATE
    | E1M3_CORRIDOR_BUTTON
    | E1M3_END_LIFT_TRIGGERED
    | E1M3_END_BUTTON
    | E1M3_END_LIFT_RIDDEN
    | E1M3_CHANGELEVEL
    | E1M3_GOLD_LIFT_DOWN;

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
            phase: PHASE_E1M2,
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
    E1M2ToLift,
    E1M2RideLift,
    E1M2ToShootable,
    E1M2RejectUse,
    E1M2SelectShotgun,
    E1M2ShootButton,
    E1M2HoldOpen,
    E1M2ToUBendGate,
    E1M2ToBridgeButton,
    E1M2PressBridgeButton,
    E1M2ToKey,
    E1M2ToSilverDoor,
    E1M2ThroughSilverDoor,
    E1M2ToFloorplate,
    E1M2ToExitTrigger,
    E1M2ToChangelevel,
    AwaitE1M3,
    E1M3ToGoldLift,
    E1M3RideGoldLiftDown,
    E1M3ToGoldKey,
    E1M3ToKeyLiftTrigger,
    E1M3AwaitKeyLift,
    E1M3BoardKeyLift,
    E1M3RideKeyLift,
    E1M3ReturnToIntersection,
    E1M3SelectShotgun,
    E1M3ShootStairButton,
    E1M3AwaitStaircase,
    E1M3ToGoldDoor,
    E1M3ThroughGoldDoor,
    E1M3AfterGoldDoor,
    E1M3AwaitSecretDoors,
    E1M3ThroughSecretDoors,
    E1M3AwaitTrapdoor,
    E1M3Dive,
    E1M3Underwater,
    E1M3AwaitCorridorDoors,
    E1M3ToEndButton,
    E1M3RideEndLift,
    E1M3ToChangelevel,
    AwaitE1M4,
    Complete,
}

struct RouteState {
    stage: Stage,
    route_index: usize,
    stage_frames: u32,
    best_distance: i32,
    stalled_frames: u16,
    detour: usize,
    u_bend_activations: u8,
    jump_committed: bool,
}

impl RouteState {
    const fn new() -> Self {
        Self {
            stage: Stage::E1M2ToLift,
            route_index: 0,
            stage_frames: 0,
            best_distance: i32::MAX,
            stalled_frames: 0,
            detour: 0,
            u_bend_activations: 0,
            jump_committed: false,
        }
    }
}

#[derive(Copy, Clone)]
struct Waypoint {
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    jump: bool,
}

const ANY_Z: i32 = i32::MIN;

const fn waypoint(x: i32, y: i32) -> Waypoint {
    Waypoint {
        x,
        y,
        z: ANY_Z,
        radius: 24,
        jump: false,
    }
}

const E1M2_TO_LIFT: &[Waypoint] = &[
    // The regression build leaves monsters inert but solid. The Ogre in the
    // spawn-room arch stands exactly on the old center-line waypoint, so pass
    // it on the west side of the bridge instead of letting the detour logic
    // walk off into the water.
    waypoint(1416, 1480),
    waypoint(1416, 1120),
    waypoint(1416, 1000),
    waypoint(1496, 960),
    waypoint(1432, 720),
    waypoint(1240, 608),
    waypoint(1144, 496),
    waypoint(1144, 240),
    waypoint(1176, 208),
    waypoint(1192, 192),
    // Enter the automatic lift on its centre line. Its trigger reaches past
    // the deck, and boarding near the west edge can pin the 32-unit player
    // hull against the shaft as the platform starts to rise.
    waypoint(1272, 144),
    Waypoint {
        x: 1272,
        y: 36,
        // The untargeted platform starts rising as soon as its expanded
        // trigger is touched. Retire the boarding point by its tight XY
        // centre instead of demanding the low-deck height after the deck has
        // already carried the player upward.
        z: ANY_Z,
        radius: 8,
        jump: false,
    },
];

const E1M2_TO_SHOOTABLE: &[Waypoint] = &[
    Waypoint {
        x: 1360,
        y: 96,
        z: ANY_Z,
        radius: 4,
        jump: false,
    },
    Waypoint {
        x: 1360,
        y: 240,
        z: ANY_Z,
        radius: 4,
        jump: false,
    },
    Waypoint {
        x: 1440,
        y: 352,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    waypoint(1672, 352),
    waypoint(1712, 312),
    waypoint(1904, -120),
    waypoint(1984, -200),
    waypoint(1984, -312),
    Waypoint {
        x: 1504,
        y: -456,
        z: 313,
        radius: 24,
        jump: false,
    },
];

const E1M2_TO_U_BEND_GATE: &[Waypoint] = &[
    waypoint(1488, -456),
    waypoint(1344, -424),
    waypoint(1248, -440),
    Waypoint {
        x: 1152,
        y: -536,
        z: ANY_Z,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 1120,
        y: -560,
        z: ANY_Z,
        radius: 12,
        jump: false,
    },
    Waypoint {
        x: 1120,
        y: -656,
        z: ANY_Z,
        radius: 12,
        jump: false,
    },
    waypoint(1240, -688),
    waypoint(1320, -688),
    waypoint(1640, -752),
    waypoint(1640, -872),
    Waypoint {
        x: 1488,
        y: -904,
        z: 441,
        radius: 40,
        jump: false,
    },
];

const E1M2_TO_BRIDGE_BUTTON: &[Waypoint] = &[Waypoint {
    x: 1248,
    y: -1160,
    z: 441,
    radius: 8,
    jump: false,
}];

const E1M2_TO_KEY: &[Waypoint] = &[
    waypoint(960, -802),
    waypoint(928, -450),
    Waypoint {
        x: 928,
        y: -338,
        z: 433,
        radius: 24,
        jump: false,
    },
    // The bridge ends just short of the pickup island. The original route
    // jumps the authored gap; the destination is the key's actual origin.
    Waypoint {
        x: 880,
        y: -300,
        z: ANY_Z,
        radius: 12,
        jump: true,
    },
];

const E1M2_TO_SILVER_DOOR: &[Waypoint] = &[
    // Do not cut diagonally off the key island: that falls into the lower
    // water corridor.  Step off its south edge onto the authored bridge that
    // target 93 restored, then follow the high gallery to the silver door.
    Waypoint {
        x: 880,
        y: -337,
        z: 433,
        radius: 10,
        jump: true,
    },
    waypoint(880, -370),
    // Stay over the centre of bridge #70 until its south end. Steering
    // diagonally toward the gallery stepped off the narrow bridge edge even
    // though the authored target had raised it correctly.
    waypoint(880, -670),
    waypoint(848, -720),
    waypoint(832, -1164),
    waypoint(640, -1164),
    waypoint(624, -1036),
    waypoint(608, -572),
    Waypoint {
        x: 336,
        y: -274,
        z: 321,
        radius: 32,
        jump: false,
    },
];

const E1M2_THROUGH_SILVER_DOOR: &[Waypoint] = &[
    waypoint(288, -220),
    Waypoint {
        x: 288,
        y: -192,
        z: 321,
        radius: 20,
        jump: false,
    },
];

const E1M2_TO_FLOORPLATE: &[Waypoint] = &[
    waypoint(112, 94),
    waypoint(-64, 270),
    Waypoint {
        x: -96,
        y: 288,
        z: 323,
        radius: 10,
        jump: false,
    },
];

const E1M2_TO_EXIT_TRIGGER: &[Waypoint] = &[
    waypoint(-96, -320),
    waypoint(-192, -480),
    waypoint(-352, -480),
    waypoint(-592, -480),
    Waypoint {
        x: -636,
        y: -480,
        z: 481,
        radius: 12,
        jump: false,
    },
];

const E1M2_TO_CHANGELEVEL: &[Waypoint] = &[
    waypoint(-624, -472),
    waypoint(-684, -472),
    Waypoint {
        x: -684,
        y: -484,
        z: ANY_Z,
        radius: 10,
        jump: false,
    },
];

const E1M3_TO_GOLD_LIFT: &[Waypoint] = &[
    waypoint(-560, -1760),
    waypoint(-288, -1736),
    waypoint(-272, -1720),
    waypoint(-144, -1248),
    waypoint(48, -1192),
    waypoint(56, -1184),
    waypoint(160, -1184),
    waypoint(272, -1352),
    waypoint(464, -1352),
    waypoint(504, -1120),
    waypoint(504, -952),
    Waypoint {
        x: 344,
        y: -896,
        z: -88,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 384,
        y: -874,
        z: -87,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: 416,
        y: -842,
        z: -103,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: 448,
        y: -810,
        z: -119,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: 480,
        y: -778,
        z: -119,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: 512,
        y: -746,
        z: -119,
        radius: 12,
        jump: false,
    },
];

const E1M3_TO_GOLD_KEY: &[Waypoint] = &[
    Waypoint {
        x: 352,
        y: -890,
        z: -327,
        radius: 20,
        jump: false,
    },
    // Keep the bend through the flooded hall explicit. The old long diagonal
    // crossed a solid corner and stalled against it forever.
    Waypoint {
        x: -320,
        y: -698,
        z: -327,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -368,
        y: -666,
        z: -327,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -464,
        y: -634,
        z: -327,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -512,
        y: -554,
        z: -330,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -528,
        y: -442,
        z: -367,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -560,
        y: -378,
        z: -367,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -896,
        y: -378,
        z: -346,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -960,
        y: -378,
        z: -335,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: -976,
        y: -360,
        z: -336,
        radius: 24,
        jump: false,
    },
];

const E1M3_KEY_TO_LIFT_TRIGGER: &[Waypoint] = &[
    waypoint(-560, -376),
    waypoint(-552, -392),
    waypoint(-552, -528),
    waypoint(-456, -632),
    waypoint(8, -792),
    waypoint(320, -880),
];

const E1M3_BOARD_KEY_LIFT: &[Waypoint] = &[waypoint(408, -864), waypoint(496, -776)];

const E1M3_RETURN_TO_INTERSECTION: &[Waypoint] = &[
    // Spiral around the dry rim of the flooded lift hall. The old diagonal
    // from the east ramp toward its centre crossed the open shaft; because
    // those points ignored height, the lower water floor could falsely retire
    // them and strand the route below the gallery.
    Waypoint {
        x: 436,
        y: -880,
        z: -87,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 504,
        y: -1056,
        z: -39,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 504,
        y: -1304,
        z: -39,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 480,
        y: -1328,
        z: -39,
        radius: 8,
        jump: false,
    },
    waypoint(400, -1352),
    waypoint(264, -1344),
    waypoint(160, -1184),
    waypoint(-56, -1184),
    waypoint(-152, -1184),
    waypoint(-272, -1712),
    waypoint(-520, -1760),
    waypoint(-576, -1752),
    waypoint(-712, -1616),
    // Rejoin the ordinary E1M3 route through automatic doors #20/#21. The
    // button's thin west-facing brush is directly visible from this corridor;
    // the tempting diagonal shot from the spawn hall hits world geometry.
    waypoint(-568, -1760),
    waypoint(-296, -1744),
    waypoint(-264, -1712),
    waypoint(-264, -1408),
    waypoint(-280, -1392),
    waypoint(-424, -1250),
    waypoint(-616, -1250),
    Waypoint {
        x: -760,
        y: -1240,
        z: 73,
        radius: 24,
        jump: false,
    },
];

const E1M3_TO_GOLD_DOOR: &[Waypoint] = &[
    // Button #303 was shot from the eastern face of this same corridor, so
    // continue west across the now-complete staircase instead of backtracking
    // through the automatic doors while their close cycle can crush us.
    Waypoint {
        x: -920,
        y: -1184,
        z: -7,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: -984,
        y: -992,
        z: -7,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: -1192,
        y: -912,
        z: -7,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: -1192,
        y: -784,
        z: -56,
        radius: 32,
        jump: false,
    },
    // Demo1 takes the northern gallery around the open lake. The short
    // southeast diagonal is only a visual ledge and drops into the trap below.
    waypoint(-1246, -763),
    waypoint(-1298, -667),
    waypoint(-1387, -627),
    waypoint(-1275, -708),
    waypoint(-1133, -686),
    waypoint(-1190, -591),
    waypoint(-1216, -471),
    waypoint(-1216, -243),
    waypoint(-1259, -155),
    waypoint(-1121, -20),
    waypoint(-1245, 40),
    waypoint(-1028, -129),
    waypoint(-896, -92),
    waypoint(-819, -141),
    waypoint(-662, -174),
    Waypoint {
        x: -548,
        y: -136,
        z: -71,
        radius: 20,
        jump: false,
    },
];

const E1M3_THROUGH_GOLD_DOOR: &[Waypoint] = &[Waypoint {
    x: -452,
    y: -136,
    z: -71,
    radius: 20,
    jump: false,
}];

const E1M3_TO_SECRET_DOORS: &[Waypoint] = &[
    Waypoint {
        x: -344,
        y: -252,
        z: -71,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: -168,
        y: -444,
        z: -87,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: 8,
        y: -444,
        z: -167,
        radius: 32,
        jump: false,
    },
    Waypoint {
        x: 72,
        y: -380,
        z: -183,
        radius: 24,
        jump: false,
    },
    // Trigger #56 spans y=-335..=-325 and opens both OPEN_ONCE secret-door
    // leaves through target 72.
    Waypoint {
        x: 72,
        y: -336,
        z: -183,
        radius: 4,
        jump: false,
    },
];

const E1M3_THROUGH_SECRET_DOORS: &[Waypoint] = &[
    Waypoint {
        x: 72,
        y: -160,
        z: -183,
        radius: 20,
        jump: false,
    },
    // Opposite edges of the narrow passage touch trigger #57 (train #53)
    // and trigger #62 (trapdoor #60) with ordinary swimming input.
    Waypoint {
        x: 104,
        y: -114,
        z: -288,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 24,
        y: -114,
        z: -288,
        radius: 24,
        jump: false,
    },
];

const E1M3_DIVE: &[Waypoint] = &[
    Waypoint {
        x: 96,
        y: -166,
        z: -288,
        radius: 20,
        jump: false,
    },
    Waypoint {
        x: 72,
        y: -600,
        z: -288,
        radius: 24,
        jump: false,
    },
];

const E1M3_UNDERWATER_ROUTE: &[Waypoint] = &[
    Waypoint {
        x: 351,
        y: -557,
        z: -287,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 431,
        y: -493,
        z: -287,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 783,
        y: -493,
        z: -287,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 807,
        y: -405,
        z: -287,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 819,
        y: -397,
        z: -287,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 903,
        y: -397,
        z: -287,
        radius: 8,
        jump: false,
    },
    Waypoint {
        x: 999,
        y: -381,
        z: -225,
        radius: 12,
        jump: false,
    },
    Waypoint {
        x: 1015,
        y: -365,
        z: -213,
        radius: 10,
        jump: false,
    },
    Waypoint {
        x: 1064,
        y: -400,
        z: -184,
        radius: 12,
        jump: false,
    },
    // The original route climbs the eastern ramp, then uses the thin stone
    // steps by the entrance. The speed demo's grenade-assisted corkscrew can
    // clear the same wall, but ordinary shotgun movement cannot.
    Waypoint {
        x: 1064,
        y: -480,
        z: -184,
        radius: 12,
        jump: false,
    },
    Waypoint {
        x: 920,
        y: -480,
        z: -184,
        radius: 12,
        jump: false,
    },
    Waypoint {
        x: 888,
        y: -488,
        z: -184,
        radius: 4,
        jump: false,
    },
    Waypoint {
        x: 868,
        y: -488,
        z: -160,
        radius: 1,
        jump: true,
    },
    Waypoint {
        x: 868,
        y: -480,
        z: -160,
        radius: 1,
        jump: false,
    },
    Waypoint {
        x: 868,
        y: -464,
        z: -128,
        radius: 1,
        jump: true,
    },
    Waypoint {
        x: 868,
        y: -448,
        z: -128,
        radius: 1,
        jump: false,
    },
    Waypoint {
        x: 868,
        y: -416,
        z: -104,
        radius: 1,
        jump: true,
    },
    Waypoint {
        x: 868,
        y: -398,
        z: -104,
        radius: 1,
        jump: false,
    },
    Waypoint {
        x: 868,
        y: -352,
        z: -72,
        radius: 1,
        jump: true,
    },
    Waypoint {
        x: 868,
        y: -336,
        z: -72,
        radius: 1,
        jump: false,
    },
    Waypoint {
        x: 840,
        y: -336,
        z: -40,
        radius: 1,
        jump: true,
    },
    Waypoint {
        x: 824,
        y: -352,
        z: -40,
        radius: 8,
        jump: false,
    },
    // Button #115 opens the four target-100 corridor doors.
    Waypoint {
        x: 744,
        y: -360,
        z: -40,
        radius: 12,
        jump: false,
    },
];

// The original single-player route leaves the four-door room, follows the
// Ogre corridor, and presses the button inside the end elevator. The optional
// south alcove is the Yellow Armor secret teleporter, not the map exit.
const E1M3_TO_END_BUTTON: &[Waypoint] = &[
    Waypoint {
        x: 796,
        y: -341,
        z: -40,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 882,
        y: -297,
        z: -41,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 972,
        y: -275,
        z: -86,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1063,
        y: -253,
        z: -184,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1160,
        y: -309,
        z: -184,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1226,
        y: -408,
        z: -184,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1320,
        y: -434,
        z: -132,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1416,
        y: -415,
        z: -136,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1518,
        y: -303,
        z: -136,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1607,
        y: -258,
        z: -136,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1672,
        y: -200,
        z: -136,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1644,
        y: -184,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1712,
        y: -158,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1699,
        y: -93,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1600,
        y: 97,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1489,
        y: 139,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1372,
        y: 124,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1315,
        y: 212,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1314,
        y: 310,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1303,
        y: 400,
        z: ANY_Z,
        radius: 24,
        jump: false,
    },
    Waypoint {
        x: 1364,
        y: 548,
        z: ANY_Z,
        radius: 12,
        jump: false,
    },
];

const E1M3_TO_CHANGELEVEL: &[Waypoint] = &[
    waypoint(1360, 760),
    waypoint(1360, 880),
    waypoint(1400, 960),
    Waypoint {
        x: 1340,
        y: 1030,
        z: 560,
        radius: 16,
        jump: false,
    },
    Waypoint {
        x: 1340,
        y: 1060,
        z: 560,
        radius: 12,
        jump: false,
    },
];

#[used]
static mut PROBE: Probe = Probe::new();
static mut STATE: RouteState = RouteState::new();

pub fn map_loaded(map: EpisodeMap) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        match map {
            EpisodeMap::E1M2 if state.stage == Stage::E1M2ToLift => {
                record_map(probe, map);
            }
            EpisodeMap::E1M3 if state.stage == Stage::AwaitE1M3 => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & E1M2_REQUIRED != E1M2_REQUIRED {
                    fail(FAILURE_MECHANISM, 2, mechanisms);
                    return;
                }
                record_map(probe, map);
                write_volatile(addr_of_mut!((*probe).transitions), 1);
                write_volatile(
                    addr_of_mut!((*probe).player_state),
                    mechanisms | E1M2_CHANGELEVEL,
                );
                set_stage(state, probe, Stage::E1M3ToGoldLift, PHASE_E1M3);
            }
            EpisodeMap::E1M4 if state.stage == Stage::AwaitE1M4 => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & E1M3_REQUIRED != E1M3_REQUIRED {
                    fail(FAILURE_MECHANISM, 3, mechanisms);
                    return;
                }
                record_map(probe, map);
                state.stage = Stage::Complete;
                write_volatile(addr_of_mut!((*probe).transitions), 2);
                write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
                write_volatile(addr_of_mut!((*probe).complete), 1);
            }
            _ => fail(FAILURE_BAD_MAP, map_index(map), state.stage as u32),
        }
    }
}

pub fn controls(
    map: EpisodeMap,
    entities: &EntityScene,
    player: &Player,
    weapon: &WeaponState,
) -> InputFrame {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 || state.stage == Stage::Complete
        {
            return InputFrame::default();
        }
        if weapon.inventory().health() <= 0 {
            fail(FAILURE_PLAYER_DEAD, map_index(map), state.stage as u32);
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).saturating_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        state.stage_frames = state.stage_frames.saturating_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), state.stage_frames);
        write_volatile(
            addr_of_mut!((*probe).intermission_state),
            state.stage as u32,
        );
        if total > MAX_TOTAL_FRAMES || state.stage_frames > MAX_STAGE_FRAMES {
            fail(FAILURE_TIMEOUT, map_index(map), state.stage as u32);
            return InputFrame::default();
        }

        let origin = player.origin();
        let x = origin.x >> 12;
        let y = origin.y >> 12;
        let z = origin.z >> 12;
        write_volatile(addr_of_mut!((*probe).last_health), x as u32);
        write_volatile(addr_of_mut!((*probe).state_ranges), y as u32);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), z as u32);
        write_volatile(addr_of_mut!((*probe).weapon_selected), weapon.shots_fired());
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));

        if map == EpisodeMap::E1M2
            && read_volatile(addr_of_mut!((*probe).player_state)) & E1M2_BUTTON_HOLD_OPEN != 0
            && entities.regression_route_mover_state(243) != Some(QuakeMoverState::Top)
        {
            fail(FAILURE_BUTTON_STATE, 2, 243);
            return InputFrame::default();
        }
        if state.stage == Stage::E1M2ToShootable && z < 250 {
            fail(
                FAILURE_MECHANISM,
                map_index(map),
                0x2000_0000 | state.route_index as u32,
            );
            return InputFrame::default();
        }
        if state.stage == Stage::E1M2ToUBendGate && z < 250 {
            fail(
                FAILURE_MECHANISM,
                map_index(map),
                0x6000_0000 | state.route_index as u32,
            );
            return InputFrame::default();
        }

        match state.stage {
            Stage::E1M2ToLift => {
                walk_stage(state, probe, player, E1M2_TO_LIFT, Stage::E1M2RideLift)
            }
            Stage::E1M2RideLift => {
                // The deck's authored top puts the player at z=312. Leaving
                // at z=300 makes the adjoining z=320 floor a 20-unit step,
                // two units above Quake's limit, so wait for the actual Top
                // state instead of using a timing-sensitive near-top sample.
                if z >= 310
                    && entities.regression_route_mover_state(192) == Some(QuakeMoverState::Top)
                {
                    add_mechanism(probe, E1M2_LIFT);
                    set_stage(state, probe, Stage::E1M2ToShootable, PHASE_E1M2);
                }
                InputFrame::default()
            }
            Stage::E1M2ToShootable => walk_stage(
                state,
                probe,
                player,
                E1M2_TO_SHOOTABLE,
                Stage::E1M2RejectUse,
            ),
            Stage::E1M2RejectUse => {
                let target = Vec3I32 {
                    x: 1546 << 12,
                    y: -552 << 12,
                    z: 328 << 12,
                };
                let mut input = look_toward(player, target);
                // Several ordinary input polls, not one synthetic call. Any
                // activation or target edge is rejected by `observe`.
                input.pressed = button::SQUARE;
                if state.stage_frames >= 8 {
                    add_mechanism(probe, E1M2_USE_REJECTED);
                    set_stage(state, probe, Stage::E1M2SelectShotgun, PHASE_E1M2);
                }
                input
            }
            Stage::E1M2SelectShotgun => {
                let chord = button::TRIANGLE | button::UP | button::RIGHT;
                let input = InputFrame {
                    held: chord,
                    pressed: chord,
                    ..InputFrame::default()
                };
                set_stage(state, probe, Stage::E1M2ShootButton, PHASE_E1M2);
                input
            }
            Stage::E1M2ShootButton => {
                let target = Vec3I32 {
                    x: 1546 << 12,
                    // Button #243 is only two units deep. Aim through the
                    // middle of its face so every pellet reaches the brush;
                    // the button itself occludes Ogre #214 from auto-aim.
                    y: -552 << 12,
                    z: 328 << 12,
                };
                let mut input = look_toward(player, target);
                input.held = button::R2;
                input
            }
            Stage::E1M2HoldOpen => {
                if entities.regression_route_mover_state(243) == Some(QuakeMoverState::Top) {
                    add_mechanism(probe, E1M2_BUTTON_HOLD_OPEN);
                    set_stage(state, probe, Stage::E1M2ToUBendGate, PHASE_E1M2);
                }
                InputFrame::default()
            }
            Stage::E1M2ToUBendGate => walk_stage(
                state,
                probe,
                player,
                E1M2_TO_U_BEND_GATE,
                Stage::E1M2ToBridgeButton,
            ),
            Stage::E1M2ToBridgeButton => walk_stage(
                state,
                probe,
                player,
                E1M2_TO_BRIDGE_BUTTON,
                Stage::E1M2PressBridgeButton,
            ),
            Stage::E1M2PressBridgeButton => {
                // Walking into the face is the original `button_touch` path;
                // `func_button` has no USE function.
                InputFrame {
                    movement: movement_input(player, 1248 - x, -1211 - y, false).movement,
                    ..InputFrame::default()
                }
            }
            Stage::E1M2ToKey => {
                if weapon.inventory().keys() & 1 != 0 {
                    add_mechanism(probe, E1M2_KEY_42);
                    set_stage(state, probe, Stage::E1M2ToSilverDoor, PHASE_E1M2);
                    InputFrame::default()
                } else if state.route_index == 0
                    && entities.regression_route_mover_state(70) != Some(QuakeMoverState::Top)
                {
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M2_TO_KEY)
                }
            }
            Stage::E1M2ToSilverDoor => walk_stage(
                state,
                probe,
                player,
                E1M2_TO_SILVER_DOOR,
                Stage::E1M2ThroughSilverDoor,
            ),
            Stage::E1M2ThroughSilverDoor => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M2_SILVER_DOOR != 0 {
                    walk_stage(
                        state,
                        probe,
                        player,
                        E1M2_THROUGH_SILVER_DOOR,
                        Stage::E1M2ToFloorplate,
                    )
                } else {
                    // Keep touching the authored key-door body until the
                    // shipping `door_touch_key` consumes the carried key.
                    movement_input(player, 288 - x, -208 - y, false)
                }
            }
            Stage::E1M2ToFloorplate => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M2_FLOORPLATE != 0 {
                    set_stage(state, probe, Stage::E1M2ToExitTrigger, PHASE_E1M2);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M2_TO_FLOORPLATE)
                }
            }
            Stage::E1M2ToExitTrigger => walk_stage(
                state,
                probe,
                player,
                E1M2_TO_EXIT_TRIGGER,
                Stage::E1M2ToChangelevel,
            ),
            Stage::E1M2ToChangelevel => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M2_EXIT_DOOR == 0 {
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M2_TO_CHANGELEVEL)
                }
            }
            Stage::E1M3ToGoldLift => walk_stage(
                state,
                probe,
                player,
                E1M3_TO_GOLD_LIFT,
                Stage::E1M3RideGoldLiftDown,
            ),
            Stage::E1M3RideGoldLiftDown => {
                if z <= -300
                    && entities.regression_route_mover_state(87) == Some(QuakeMoverState::Top)
                {
                    add_mechanism(probe, E1M3_GOLD_LIFT_DOWN);
                    set_stage(state, probe, Stage::E1M3ToGoldKey, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3ToGoldKey => {
                if weapon.inventory().keys() & 2 != 0 {
                    add_mechanism(probe, E1M3_GOLD_KEY);
                    set_stage(state, probe, Stage::E1M3ToKeyLiftTrigger, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M3_TO_GOLD_KEY)
                }
            }
            Stage::E1M3ToKeyLiftTrigger => walk_stage(
                state,
                probe,
                player,
                E1M3_KEY_TO_LIFT_TRIGGER,
                Stage::E1M3AwaitKeyLift,
            ),
            Stage::E1M3AwaitKeyLift => {
                if entities.regression_route_mover_state(87) == Some(QuakeMoverState::Top) {
                    add_mechanism(probe, E1M3_KEY_LIFT_TRIGGERED);
                    set_stage(state, probe, Stage::E1M3BoardKeyLift, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3BoardKeyLift => walk_stage(
                state,
                probe,
                player,
                E1M3_BOARD_KEY_LIFT,
                Stage::E1M3RideKeyLift,
            ),
            Stage::E1M3RideKeyLift => {
                if z >= -130
                    && entities.regression_route_mover_state(87) == Some(QuakeMoverState::Bottom)
                {
                    add_mechanism(probe, E1M3_KEY_LIFT_RIDDEN);
                    set_stage(state, probe, Stage::E1M3ReturnToIntersection, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3ReturnToIntersection => walk_stage(
                state,
                probe,
                player,
                E1M3_RETURN_TO_INTERSECTION,
                Stage::E1M3SelectShotgun,
            ),
            Stage::E1M3SelectShotgun => {
                let chord = button::TRIANGLE | button::UP | button::RIGHT;
                let input = InputFrame {
                    held: chord,
                    pressed: chord,
                    ..InputFrame::default()
                };
                set_stage(state, probe, Stage::E1M3ShootStairButton, PHASE_E1M3);
                input
            }
            Stage::E1M3ShootStairButton => {
                let mut input = look_toward(
                    player,
                    Vec3I32 {
                        x: -828 << 12,
                        y: -1240 << 12,
                        z: 104 << 12,
                    },
                );
                input.held = button::R2;
                input
            }
            Stage::E1M3AwaitStaircase => {
                if (24..=28).all(|source_index| {
                    entities.regression_route_mover_state(source_index)
                        == Some(QuakeMoverState::Top)
                }) {
                    add_mechanism(probe, E1M3_STAIRCASE);
                    set_stage(state, probe, Stage::E1M3ToGoldDoor, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3ToGoldDoor => walk_stage(
                state,
                probe,
                player,
                E1M3_TO_GOLD_DOOR,
                Stage::E1M3ThroughGoldDoor,
            ),
            Stage::E1M3ThroughGoldDoor => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M3_GOLD_DOOR != 0 {
                    walk_stage(
                        state,
                        probe,
                        player,
                        E1M3_THROUGH_GOLD_DOOR,
                        Stage::E1M3AfterGoldDoor,
                    )
                } else {
                    movement_input(player, -500 - x, -203 - y, false)
                }
            }
            Stage::E1M3AfterGoldDoor => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & E1M3_SECRET_DOORS != 0 {
                    set_stage(state, probe, Stage::E1M3AwaitSecretDoors, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M3_TO_SECRET_DOORS)
                }
            }
            Stage::E1M3AwaitSecretDoors => {
                if entities.regression_route_mover_state(54) == Some(QuakeMoverState::Top)
                    && entities.regression_route_mover_state(55) == Some(QuakeMoverState::Top)
                {
                    set_stage(state, probe, Stage::E1M3ThroughSecretDoors, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3ThroughSecretDoors => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                let passage = E1M3_TRAIN | E1M3_TRAPDOOR | E1M3_UNDERWATER_GATE;
                if mechanisms & passage == passage {
                    // The narrow twin triggers can both fire while the opened
                    // trapdoor is already dropping the player. Once their
                    // authored fan-out is observed, do not try to swim back
                    // through the now-overhead doorway.
                    set_stage(state, probe, Stage::E1M3AwaitTrapdoor, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M3_THROUGH_SECRET_DOORS)
                }
            }
            Stage::E1M3AwaitTrapdoor => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & E1M3_TRAPDOOR != 0
                    && entities.regression_route_mover_state(60) == Some(QuakeMoverState::Top)
                {
                    set_stage(state, probe, Stage::E1M3Dive, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3Dive => walk_stage(state, probe, player, E1M3_DIVE, Stage::E1M3Underwater),
            Stage::E1M3Underwater => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M3_CORRIDOR_BUTTON != 0 {
                    set_stage(state, probe, Stage::E1M3AwaitCorridorDoors, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M3_UNDERWATER_ROUTE)
                }
            }
            Stage::E1M3AwaitCorridorDoors => {
                if (110..=111).chain(113..=114).all(|source_index| {
                    entities.regression_route_mover_state(source_index)
                        == Some(QuakeMoverState::Top)
                }) {
                    set_stage(state, probe, Stage::E1M3ToEndButton, PHASE_E1M3);
                }
                InputFrame::default()
            }
            Stage::E1M3ToEndButton => {
                if read_volatile(addr_of_mut!((*probe).player_state)) & E1M3_END_BUTTON != 0 {
                    set_stage(state, probe, Stage::E1M3RideEndLift, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    walk_stage_stay(state, probe, player, E1M3_TO_END_BUTTON)
                }
            }
            Stage::E1M3RideEndLift => {
                if z > 500 {
                    add_mechanism(probe, E1M3_END_LIFT_RIDDEN);
                    set_stage(state, probe, Stage::E1M3ToChangelevel, PHASE_E1M3);
                    InputFrame::default()
                } else {
                    movement_input(player, 1360 - x, 432 - y, false)
                }
            }
            Stage::E1M3ToChangelevel => walk_stage_stay(state, probe, player, E1M3_TO_CHANGELEVEL),
            Stage::AwaitE1M3 | Stage::AwaitE1M4 | Stage::Complete => InputFrame::default(),
        }
    }
}

pub fn observe(
    map: EpisodeMap,
    entities: &EntityScene,
    gameplay: GameplayResult,
    pickup: PickupResult,
    weapon: &WeaponState,
) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        if let Some(error) = gameplay.target_error.or(pickup.target_error) {
            fail(FAILURE_TARGET_GRAPH, map_index(map), error as u32);
            return;
        }
        let frame_edges = u32::from(gameplay.fired_target_edges)
            .saturating_add(u32::from(pickup.fired_target_edges));
        write_volatile(
            addr_of_mut!((*probe).target_edges),
            read_volatile(addr_of_mut!((*probe).target_edges)).saturating_add(frame_edges),
        );

        let activations = gameplay.player_activated_movers;
        if state.stage == Stage::E1M2RejectUse {
            if activations.iter().flatten().any(|&index| index == 243) || frame_edges != 0 {
                fail(FAILURE_USE_OPENED_SHOOTABLE, 2, 243);
                return;
            }
        }
        if state.stage == Stage::E1M2ShootButton
            && activations.iter().flatten().any(|&index| index == 243)
        {
            if gameplay.fired_target_edges == 0 {
                fail(FAILURE_MECHANISM, 2, 77);
                return;
            }
            add_mechanism(probe, E1M2_BUTTON_SHOT | E1M2_TARGET_77);
            write_volatile(
                addr_of_mut!((*probe).weapon_fired),
                read_volatile(addr_of_mut!((*probe).weapon_fired)).saturating_add(1),
            );
            set_stage(state, probe, Stage::E1M2HoldOpen, PHASE_E1M2);
        }
        if state.stage == Stage::E1M3ShootStairButton
            && activations.iter().flatten().any(|&index| index == 303)
        {
            if gameplay.fired_target_edges == 0 {
                fail(FAILURE_MECHANISM, 3, 32);
                return;
            }
            add_mechanism(probe, E1M3_STAIR_BUTTON);
            write_volatile(
                addr_of_mut!((*probe).weapon_fired),
                read_volatile(addr_of_mut!((*probe).weapon_fired)).saturating_add(1),
            );
            set_stage(state, probe, Stage::E1M3AwaitStaircase, PHASE_E1M3);
        }
        let activated = |source_index| {
            activations
                .iter()
                .flatten()
                .any(|&index| index == source_index)
        };
        if map == EpisodeMap::E1M3 {
            if activated(54) && activated(55) {
                add_mechanism(probe, E1M3_SECRET_DOORS);
            }
            if activated(53) {
                add_mechanism(probe, E1M3_TRAIN);
            }
            if activated(60) {
                add_mechanism(probe, E1M3_TRAPDOOR);
            }
            if activated(66) {
                add_mechanism(probe, E1M3_UNDERWATER_GATE);
            }
            if activated(115) {
                if gameplay.fired_target_edges == 0 {
                    fail(FAILURE_MECHANISM, 3, 100);
                    return;
                }
                add_mechanism(probe, E1M3_CORRIDOR_BUTTON);
            }
            if activated(14) {
                // `button_fire` runs only when the moving button reaches Top;
                // the initial touch merely starts that travel.
                add_mechanism(probe, E1M3_END_BUTTON);
            }
            if [4, 5, 15, 16, 19].into_iter().all(activated) {
                if gameplay.fired_target_edges == 0 {
                    fail(FAILURE_MECHANISM, 3, 0x4000_0004);
                    return;
                }
                add_mechanism(probe, E1M3_END_LIFT_TRIGGERED);
            }
        }
        // Trigger #52 can be crossed on the final approach to #243, before
        // the route advances to the named U-bend leg.  Prove its target 73
        // fan-out by observing both authored door activations, independent of
        // that harmless route-stage timing.
        for source_index in activations.iter().copied().flatten() {
            match source_index {
                50 => state.u_bend_activations |= 1,
                51 => state.u_bend_activations |= 2,
                _ => {}
            }
        }
        if state.u_bend_activations == 3 {
            add_mechanism(probe, E1M2_U_BEND_GATE);
        }
        if state.stage == Stage::E1M2PressBridgeButton
            && activations.iter().flatten().any(|&index| index == 71)
        {
            add_mechanism(probe, E1M2_KEY_BRIDGE);
            set_stage(state, probe, Stage::E1M2ToKey, PHASE_E1M2);
        }
        if pickup.last_source_index == Some(42) {
            if weapon.inventory().keys() & 1 == 0 || pickup.fired_target_edges == 0 {
                fail(FAILURE_MECHANISM, 2, 42);
                return;
            }
            if !matches!(
                entities.regression_route_mover_state(204),
                Some(QuakeMoverState::Up | QuakeMoverState::Top)
            ) || !matches!(
                entities.regression_route_mover_state(205),
                Some(QuakeMoverState::Up | QuakeMoverState::Top)
            ) {
                fail(FAILURE_MECHANISM, 2, 205);
                return;
            }
            add_mechanism(probe, E1M2_KEY_42 | E1M2_KEY_TARGET_68);
            write_volatile(
                addr_of_mut!((*probe).weapon_pickups),
                read_volatile(addr_of_mut!((*probe).weapon_pickups)).saturating_add(1),
            );
        }
        if map == EpisodeMap::E1M3 && pickup.last_source_index == Some(104) {
            if weapon.inventory().keys() & 2 == 0 {
                fail(FAILURE_MECHANISM, 3, 104);
                return;
            }
            add_mechanism(probe, E1M3_GOLD_KEY);
            write_volatile(
                addr_of_mut!((*probe).weapon_pickups),
                read_volatile(addr_of_mut!((*probe).weapon_pickups)).saturating_add(1),
            );
        }
        if gameplay.consumed_key == Some(1) {
            add_mechanism(probe, E1M2_SILVER_DOOR);
        }
        if map == EpisodeMap::E1M3 && gameplay.consumed_key == Some(2) {
            if !matches!(
                entities.regression_route_mover_state(37),
                Some(QuakeMoverState::Up | QuakeMoverState::Top)
            ) || !matches!(
                entities.regression_route_mover_state(38),
                Some(QuakeMoverState::Up | QuakeMoverState::Top)
            ) {
                fail(FAILURE_MECHANISM, 3, 38);
                return;
            }
            add_mechanism(probe, E1M3_GOLD_DOOR);
        }
        if state.stage == Stage::E1M2ToFloorplate
            && activations.iter().flatten().any(|&index| index == 80)
        {
            add_mechanism(probe, E1M2_FLOORPLATE);
        }
        if state.stage == Stage::E1M2ToExitTrigger && gameplay.fired_target_edges != 0 {
            add_mechanism(probe, E1M2_EXIT_DOOR);
        }
    }
}

pub fn transition_requested(map: EpisodeMap, destination: EpisodeMap) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        match (map, destination) {
            (EpisodeMap::E1M2, EpisodeMap::E1M3) => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & (E1M2_REQUIRED & !E1M2_CHANGELEVEL)
                    != E1M2_REQUIRED & !E1M2_CHANGELEVEL
                {
                    fail(FAILURE_MECHANISM, 2, mechanisms);
                    return;
                }
                add_mechanism(probe, E1M2_CHANGELEVEL);
                set_stage(state, probe, Stage::AwaitE1M3, PHASE_E1M2);
            }
            (EpisodeMap::E1M3, EpisodeMap::E1M4) => {
                let mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
                if mechanisms & (E1M3_REQUIRED & !E1M3_CHANGELEVEL)
                    != E1M3_REQUIRED & !E1M3_CHANGELEVEL
                {
                    fail(FAILURE_MECHANISM, 3, mechanisms);
                    return;
                }
                add_mechanism(probe, E1M3_CHANGELEVEL);
                set_stage(state, probe, Stage::AwaitE1M4, PHASE_E1M3);
            }
            _ => fail(FAILURE_BAD_MAP, map_index(map), map_index(destination)),
        }
    }
}

fn walk_stage(
    state: &mut RouteState,
    probe: *mut Probe,
    player: &Player,
    route: &[Waypoint],
    next: Stage,
) -> InputFrame {
    let input = walk_stage_stay(state, probe, player, route);
    if state.route_index >= route.len() {
        set_stage(state, probe, next, unsafe {
            read_volatile(addr_of_mut!((*probe).phase))
        });
        InputFrame::default()
    } else {
        input
    }
}

fn walk_stage_stay(
    state: &mut RouteState,
    probe: *mut Probe,
    player: &Player,
    route: &[Waypoint],
) -> InputFrame {
    let origin = player.origin();
    let x = origin.x >> 12;
    let y = origin.y >> 12;
    let z = origin.z >> 12;
    while let Some(target) = route.get(state.route_index).copied() {
        let dx = target.x.saturating_sub(x);
        let dy = target.y.saturating_sub(y);
        // A loose height band is useful on ordinary slopes and moving
        // platforms, but it must not retire the launch point while the player
        // is already falling past it. Require a real landing immediately
        // before each authored jump edge.
        let next_is_jump = route
            .get(state.route_index.saturating_add(1))
            .is_some_and(|waypoint| waypoint.jump);
        let height_tolerance = if player.water_level() != 0 || next_is_jump {
            8
        } else {
            64
        };
        let height_ok = target.z == ANY_Z || (target.z - z).abs() <= height_tolerance;
        // A jump target is a landing, not merely a coordinate crossed near
        // the apex. Retiring it in mid-air made the next step steer the player
        // off E1M3's eight-unit-wide ledges before their feet touched down.
        let landed = !target.jump || player.rider().grounded;
        if dx.abs() <= target.radius && dy.abs() <= target.radius && height_ok && landed {
            state.route_index += 1;
            unsafe { write_volatile(addr_of_mut!((*probe).route_index), state.route_index as u32) };
            state.best_distance = i32::MAX;
            state.stalled_frames = 0;
            state.detour = 0;
            state.jump_committed = false;
            continue;
        }
        let distance = dx.abs().saturating_add(dy.abs());
        if distance < state.best_distance {
            state.best_distance = distance;
            state.stalled_frames = 0;
            state.detour = 0;
        } else {
            state.stalled_frames = state.stalled_frames.saturating_add(1);
            if state.stalled_frames > 60 {
                state.stalled_frames = 0;
                state.detour = (state.detour + 1) & 7;
            }
        }
        // Dunked (the E1M2 moat, when the bridge ogre shoves the walker
        // off): do what a human does, face the waypoint, swim toward it
        // holding jump to rise, and let `CheckWaterJump` hop the ledge.
        let in_liquid = player.water_level() != 0;
        if in_liquid {
            let rise = target.z == ANY_Z || target.z > z + 4;
            let mut input = look_toward(
                player,
                Vec3I32 {
                    x: target.x << 12,
                    y: target.y << 12,
                    z: if target.z == ANY_Z {
                        player.origin().z
                    } else if rise {
                        target.z.saturating_add(64) << 12
                    } else {
                        target.z << 12
                    },
                },
            );
            input.movement = movement_input(player, dx, dy, false).movement;
            input.held = if rise { button::CROSS } else { 0 };
            return input;
        }
        let (dx, dy) = detoured(dx, dy, state.detour);
        // A short hop can put the player back on the launch stone without
        // reaching the higher target. Re-arm only in that grounded, still-low
        // state. Landing at the target height remains protected from the
        // repeated-jump bug above.
        if target.jump
            && state.jump_committed
            && player.rider().grounded
            && target.z != ANY_Z
            && z + 8 < target.z
        {
            state.jump_committed = false;
        }
        let jump_pulse = target.jump && state.stage_frames & 7 < 2;
        // At a fatal gap, do not walk off while waiting for the next clean
        // jump edge. Once the pulse starts, keep steering in the air while
        // releasing CROSS so a landing can re-arm the normal jump latch.
        let jump = if target.jump && !state.jump_committed {
            if !jump_pulse {
                return InputFrame::default();
            }
            state.jump_committed = true;
            true
        } else {
            false
        };
        return movement_input(player, dx, dy, jump);
    }
    InputFrame::default()
}

fn movement_input(player: &Player, dx: i32, dy: i32, jump: bool) -> InputFrame {
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    // Most routes retire their points well outside the taper. E1M3's little
    // platforms are only eight units wide, so the zero-radius centering points
    // need a genuinely fine analog correction instead of the old 16/127
    // minimum that could carry the player straight over the opposite edge.
    let limit = dx.abs().max(dy.abs()).saturating_mul(6).clamp(4, 127);
    InputFrame {
        movement: [
            (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
            (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        ],
        held: if jump { button::CROSS } else { 0 },
        ..InputFrame::default()
    }
}

fn look_toward(player: &Player, target: Vec3I32) -> InputFrame {
    let from = player.camera().origin;
    let dx = (target.x.saturating_sub(from.x)) >> 12;
    let dy = (target.y.saturating_sub(from.y)) >> 12;
    let dz = (target.z.saturating_sub(from.z)) >> 12;
    let horizontal = isqrt_i32(dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)));
    let target_pitch = atan2_q12(-dz, horizontal) as i16;
    let target_yaw = atan2_q12(dy, dx) as i16;
    InputFrame {
        look: [
            angle_delta(target_yaw, player.view_angles[1]).clamp(-127, 127),
            angle_delta(target_pitch, player.view_angles[0]).clamp(-127, 127),
        ],
        ..InputFrame::default()
    }
}

fn angle_delta(target: i16, current: i16) -> i16 {
    let mut delta = (i32::from(target as u16) - i32::from(current as u16)) & 0x0fff;
    if delta > 2_048 {
        delta -= 4_096;
    }
    delta as i16
}

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

fn set_stage(state: &mut RouteState, probe: *mut Probe, stage: Stage, phase: u32) {
    state.stage = stage;
    state.route_index = 0;
    state.stage_frames = 0;
    state.best_distance = i32::MAX;
    state.stalled_frames = 0;
    state.detour = 0;
    state.jump_committed = false;
    unsafe {
        write_volatile(addr_of_mut!((*probe).phase), phase);
        write_volatile(addr_of_mut!((*probe).route_index), 0);
        write_volatile(addr_of_mut!((*probe).stage_frames), 0);
    }
}

fn add_mechanism(probe: *mut Probe, bits: u32) {
    unsafe {
        write_volatile(
            addr_of_mut!((*probe).player_state),
            read_volatile(addr_of_mut!((*probe).player_state)) | bits,
        );
    }
}

fn record_map(probe: *mut Probe, map: EpisodeMap) {
    unsafe {
        let bit = 1 << map_index(map);
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | bit,
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | bit,
        );
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).saturating_add(1),
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
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
