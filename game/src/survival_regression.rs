//! Normal-input E1M1 route through the authored survival hazards.
//!
//! The route walks the shipping movement motor from `info_player_start` into
//! E1M1's authored slime channel and its authored water pool, dies there,
//! restarts the level with a fire press, and takes the authored
//! `item_artifact_super_damage`. Nothing is teleported and no state is
//! injected: every waypoint is either a corridor point on the proven E1M1
//! chain route or a station derived at runtime from the cooked map, and the
//! only inputs are ordinary pad presses.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::int32::mul_q12_i32;
use psx_pad::button;
use quake_core::collision::{CONTENTS_LAVA, CONTENTS_SLIME, CONTENTS_WATER};
use quake_core::combat::{AmmoKind, Weapon, WeaponState};
use quake_core::survival::PowerupKind;

use crate::asset::{EpisodeMap, ResidentMap};
use crate::input::InputFrame;
use crate::player::{Player, PlayerFrame};

const PROBE_MAGIC: u32 = 0x5150_5358;
// Version 9: the convergence probes took 5 through 8.
const PROBE_VERSION: u32 = 9;
const PHASE_ROUTE: u32 = 1;
const PHASE_COMPLETE: u32 = 0x59;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_NO_HAZARD_LEAF: u32 = 3;
const FAILURE_NO_ARTIFACT: u32 = 4;
const FAILURE_RESPAWN_LOADOUT: u32 = 5;
const FAILURE_ROUTE_ORDER: u32 = 6;
const FAILURE_MOTOR_STALL: u32 = 7;
/// A live player whose motor refuses to run for this many consecutive frames
/// is frozen rather than blocked. Two seconds is far longer than any single
/// legitimate hiccup and far shorter than the route timeout.
const MOTOR_STALL_FRAMES_BEFORE_FAILURE: u32 = 120;
const MAX_ROUTE_FRAMES: u32 = 9_000;

const HAZARD_DAMAGE: u32 = 1 << 0;
const FALL_DAMAGE: u32 = 1 << 1;
const DROWN_DAMAGE: u32 = 1 << 2;
const HAZARD_DEATH: u32 = 1 << 3;
const RESPAWN_LOADOUT: u32 = 1 << 4;
const POWERUP_TAKEN: u32 = 1 << 5;
const POWERUP_HALF_SPENT: u32 = 1 << 6;
const POWERUP_EXPIRED: u32 = 1 << 7;
const DESCENDED: u32 = 1 << 8;
/// Every outcome the gate is here to observe, all of them on real E1M1 with
/// ordinary pad input.
const REQUIRED_MECHANISMS: u32 = HAZARD_DAMAGE
    | FALL_DAMAGE
    | DROWN_DAMAGE
    | HAZARD_DEATH
    | RESPAWN_LOADOUT
    | POWERUP_TAKEN
    | POWERUP_HALF_SPENT
    | POWERUP_EXPIRED;

/// The player origin drops below this once the authored lift has carried it
/// into E1M1's lower level.
const DESCENDED_Z: i32 = -150;

/// The 136-byte gameplay probe record shared by every headless gate. Field
/// names stay the shared ones so the host reader keeps one layout; the
/// comment on each line is what this gate stores there.
#[repr(C)]
#[derive(Copy, Clone)]
struct Probe {
    magic: u32,
    version: u32,
    complete: u32,
    phase: u32,
    failure_code: u32,
    failure_map: u32,
    failure_entity: u32, // failing stage
    failure_detail: u32,
    total_frames: u32,
    maps_loaded: u32,
    maps_validated: u32,
    transitions: u32,          // authored quad entity index
    weapon_selected: u32,      // hazard damage taken
    weapon_fired: u32,         // fall damage taken
    weapon_animated: u32,      // drowning damage taken
    monster_present: u32,      // quad seconds at pickup
    monster_animated: u32,     // quad seconds when half spent
    monster_state_bounds: u32, // authored hazard leaf index
    monster_attack: u32,       // deaths
    monster_pain: u32,         // respawns
    monster_death: u32,        // respawn health
    boss: u32,                 // respawn shells
    current_map: u32,
    route_index: u32,
    last_health: u32,        // player x
    state_ranges: u32,       // player y
    valid_state_ranges: u32, // player z
    map_loads: u32,
    stage_frames: u32,       // current stage
    shock_count: u32,        // water levels seen, one bit per level
    intermission_state: u32, // water types seen: water 1, slime 2, lava 4
    player_state: u32,       // mechanism bits
    weapon_pickups: u32,     // lowest player z reached
    // Motor health: `stall_frames << 8 | MovementStalls bits`. A shipping map
    // must never stall the player motor nor leave a collision query
    // unresolved, so any non-zero value here fails the gate.
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
            transitions: u32::MAX,
            weapon_selected: 0,
            weapon_fired: 0,
            weapon_animated: 0,
            monster_present: 0,
            monster_animated: 0,
            monster_state_bounds: u32::MAX,
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
    use_press: bool,
    /// Frames to stand on the waypoint before advancing. A player who has just
    /// set an authored mover going waits for it exactly like a human does; the
    /// alternative is walking into the shaft the mover has not filled yet.
    dwell: u32,
    require: u32,
}

const fn waypoint(x: i32, y: i32) -> Waypoint {
    Waypoint {
        x,
        y,
        z: i32::MIN,
        radius: 20,
        jump: false,
        use_press: false,
        dwell: 0,
        require: 0,
    }
}

/// Stand on a mover's control until the mover has finished travelling.
const fn wait_for_mover(x: i32, y: i32, radius: i32, dwell: u32, use_press: bool) -> Waypoint {
    Waypoint {
        x,
        y,
        z: i32::MIN,
        radius,
        jump: false,
        use_press,
        dwell,
        require: 0,
    }
}

const fn station(x: i32, y: i32, radius: i32, require: u32) -> Waypoint {
    Waypoint {
        x,
        y,
        z: i32::MIN,
        radius,
        jump: false,
        use_press: false,
        dwell: 0,
        require,
    }
}

/// Corridor points 0 to 11 are the proven E1M1 chain route's approach to the
/// lower level; only the stations after them are new.
const LOWER_LEVEL_APPROACH: &[Waypoint] = &[
    waypoint(432, -352),
    waypoint(432, 16),
    waypoint(368, 16),
    waypoint(368, 64),
    waypoint(336, 64),
    waypoint(336, 528),
    // Cross the hall on one axis at a time. The diagonal the E1M1 chain
    // route takes here relies on its own approach speed; this route arrives
    // with a different one and wedges on the shaft's door frame.
    waypoint(96, 592),
    waypoint(0, 592),
    waypoint(0, 576),
    Waypoint {
        x: -40,
        y: 576,
        z: 24,
        radius: 4,
        jump: false,
        use_press: false,
        dwell: 0,
        require: 0,
    },
    Waypoint {
        x: 96,
        y: 576,
        z: 24,
        radius: 8,
        jump: true,
        use_press: true,
        dwell: 50,
        require: 0,
    },
    Waypoint {
        x: 64,
        y: 576,
        z: 24,
        radius: 12,
        jump: false,
        use_press: false,
        dwell: 0,
        require: 0,
    },
    Waypoint {
        x: 0,
        y: 576,
        z: i32::MIN,
        radius: 16,
        jump: true,
        use_press: false,
        dwell: 0,
        require: DESCENDED | FALL_DAMAGE,
    },
    waypoint(0, 672),
    waypoint(80, 720),
    waypoint(80, 1040),
];

#[used]
static mut PROBE: Probe = Probe::new();
static mut ROUTE_INDEX: usize = 0;
static mut STAGE: usize = 0;
static mut PREVIOUS_HEALTH: i16 = 100;
static mut LAST_X: i32 = i32::MIN;
static mut LAST_Y: i32 = i32::MIN;
static mut STUCK_FRAMES: u32 = 0;
static mut DWELL_FRAMES: u32 = 0;

/// Frames of no progress before the route stops pushing straight at a
/// waypoint and slides along the obstacle instead. Quake corridors are full
/// of door frames and 18 unit steps that a straight-line approach can hang
/// on; a human plays around them the same way.
const STUCK_FRAMES_BEFORE_SLIDE: u32 = 24;
const SLIDE_FRAMES: u32 = 40;

/// The corridor from the lower level through E1M1's authored double doors to
/// the room that opens on the long west ramp. Two stages share it.
///
/// The dogleg east is not decoration. Route regressions keep the monster think
/// loop out of the probe, so E1M1's authored Soldier at (8, 1520) and Dog at
/// (88, 1520) never move, but they are still solid bodies in the player's
/// broad phase. Together they wall x -8..120 across the corridor, and a
/// straight run north wedges the player at y 1471 against the Dog for the rest
/// of the frame budget. Passing at x 240 clears the Dog's +32 half-width and
/// the player's own 16 by a wide margin.
const NORTH_CORRIDOR: [Waypoint; 7] = [
    waypoint(80, 1360),
    waypoint(240, 1440),
    waypoint(240, 1650),
    // The authored double doors at y 1777..1823 open on touch.
    waypoint(128, 1740),
    waypoint(128, 1856),
    // Wide of the authored Soldier at (80, 2024) on the way to the ramp.
    waypoint(20, 1930),
    waypoint(-120, 1990),
];

/// Stage 0 walks the west ramp into the authored slime channel and dies there.
/// Stage 1 re-walks the approach and drowns in the authored water pool. Stage 2
/// takes the authored quad and waits out its timer.
///
/// Every coordinate below was measured against the cooked `e1m1.psb` with
/// `tools/routesim`, which links the same movement and collision the guest
/// links, and each leg was walked there with ordinary movement input before it
/// was authored here.
fn stage_route(stage: usize) -> &'static [Waypoint] {
    // The slime channel is x 0..320, y 2660..2990, surface -136, floor -176, so
    // the player stands in it at origin -152. Its west walkway is world brush at
    // -56 and the authored `func_door` bridge that covers the channel spawns
    // retracted (`START_OPEN`), so the channel is open from the first frame.
    const SLIME: [Waypoint; 14] = [
        NORTH_CORRIDOR[0],
        NORTH_CORRIDOR[1],
        NORTH_CORRIDOR[2],
        NORTH_CORRIDOR[3],
        NORTH_CORRIDOR[4],
        NORTH_CORRIDOR[5],
        NORTH_CORRIDOR[6],
        // The west ramp: -184 at the room, then -168, -152, -136, -104, -88 to
        // the slime room's -56 walkway.
        waypoint(-250, 2060),
        waypoint(-300, 2200),
        waypoint(-300, 2400),
        waypoint(-300, 2600),
        waypoint(-200, 2700),
        waypoint(-30, 2760),
        station(160, 2900, 48, HAZARD_DAMAGE | HAZARD_DEATH),
    ];
    // Walking south out of the lower level drops into E1M1's water at y 1150.
    // Only the deep pool at x 550..700, y 850..1000 submerges the player: its
    // floor stands the player at -344 with the surface at -296, so the eye
    // sample at -322 is under water and the water level reaches three. The
    // motor has no vertical swim, so the player walks the pool floor east.
    //
    // The first two points are the only way west out of the lower level. The
    // room's west wall there is a real 24 unit ledge (world floor -208 east of
    // it, -184 on top) and Quake steps 18, so a leg that turns west at y 1040
    // walks straight into it and can never climb out. The flat -224 floor south
    // of y 1030 crosses the same span with no step at all, so the route drops
    // back into the room before it turns.
    const WATER: [Waypoint; 7] = [
        waypoint(80, 1000),
        waypoint(0, 1010),
        waypoint(0, 1100),
        waypoint(0, 1250),
        waypoint(300, 1250),
        waypoint(550, 1200),
        station(620, 950, 48, DROWN_DAMAGE),
    ];
    // The authored `item_artifact_super_damage` sits at (544, 2480, -88) in
    // E1M1's secret chamber. The chamber has two authored entrances and this
    // route takes the walkable one: the east passage at y 2480, behind
    // `func_door` #32 (opened by `trigger_once` #33, a plate the corridor leg
    // crosses) and `func_door_secret` #28 (no targetname, so the shipping use
    // press opens it).
    //
    // The other entrance is the one the plate at (449..455, 2001..2031) serves:
    // `trigger_multiple` #31 raises `func_door` #30, a 64 unit bridge whose
    // raised top is 65 above the walkway beside it. Nothing but riding it up
    // reaches that top, and this port has no `SV_PushMove` rider, so that
    // entrance stays shut whether or not the plate can be shot.
    const QUAD: [Waypoint; 23] = [
        NORTH_CORRIDOR[0],
        NORTH_CORRIDOR[1],
        NORTH_CORRIDOR[2],
        NORTH_CORRIDOR[3],
        NORTH_CORRIDOR[4],
        // The proven E1M1 chain route's climb to the upper level.
        waypoint(-144, 1856),
        waypoint(-152, 2000),
        waypoint(-152, 2080),
        waypoint(-152, 2112),
        waypoint(-152, 2480),
        waypoint(-152, 2592),
        waypoint(-152, 2720),
        waypoint(-48, 2720),
        // The authored `func_button` at (-63..-33, 2657..2663) opens on touch
        // and raises the `t2` bridge over the slime channel. The bridge travels
        // 238 units at speed 600, so half a second covers it many times over;
        // crossing before it lands drops the player in the slime.
        wait_for_mover(-48, 2680, 9, 90, false),
        waypoint(-16, 2720),
        waypoint(0, 2720),
        waypoint(352, 2720),
        waypoint(640, 2784),
        // This leg crosses `trigger_once` #33 at (769..895, 2601..2615), which
        // opens `func_door` #32. That door carries `wait -1`, so it stays open.
        waypoint(800, 2736),
        waypoint(848, 2544),
        // `func_door_secret` #28 at (753..767, 2433..2527) travels 86 units at
        // speed 50, so the first press waits out the travel. The second press
        // is on the door's own face: the secret holds open for five seconds and
        // then shuts, and a player who arrives after that just opens it again.
        wait_for_mover(800, 2480, 16, 120, true),
        wait_for_mover(786, 2480, 14, 40, true),
        station(
            544,
            2480,
            32,
            POWERUP_TAKEN | POWERUP_HALF_SPENT | POWERUP_EXPIRED,
        ),
    ];
    match stage {
        0 => &SLIME,
        1 => &WATER,
        _ => &QUAD,
    }
}

const STAGE_COUNT: usize = 3;

pub fn map_loaded(map: EpisodeMap) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if map != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, map_index(map), 0);
            return;
        }
        write_volatile(addr_of_mut!((*probe).maps_loaded), 0x002);
        write_volatile(addr_of_mut!((*probe).maps_validated), 0x002);
        write_volatile(addr_of_mut!((*probe).current_map), 1);
        let loads = read_volatile(addr_of_mut!((*probe).map_loads)).saturating_add(1);
        write_volatile(addr_of_mut!((*probe).map_loads), loads);
        // Every load restarts the approach from `info_player_start`. The only
        // reload this route performs is the respawn after its own death, so a
        // reload is exactly the boundary between survival stages.
        ROUTE_INDEX = 0;
        // `PREVIOUS_HEALTH` is deliberately not reset. It holds the zero the
        // death frame left, and `observe` reads the zero-to-hundred edge on the
        // first frame after the reload to assert the `SetNewParms` loadout.
        // Resetting it here put a hundred on both sides of that edge, so the
        // respawn was never observed and `RESPAWN_LOADOUT` could not be set.
        LAST_X = i32::MIN;
        LAST_Y = i32::MIN;
        STUCK_FRAMES = 0;
        DWELL_FRAMES = 0;
        // `DESCENDED` is this route's proof that the authored lift carried
        // the player into the lower level on THIS load, not an accumulated
        // survival outcome, so it re-arms here. Left sticky it let stage 1
        // inherit stage 0's descent: waypoint 9's requirement was already
        // satisfied, the USE press that calls the lift was skipped, and the
        // route then stood on the shaft lip at z 24 waiting to be at -207
        // until the frame budget ran out. Every other bit is a real
        // observation and accumulates across stages, so only this one is
        // cleared.
        let armed = read_volatile(addr_of_mut!((*probe).player_state)) & !DESCENDED;
        write_volatile(addr_of_mut!((*probe).player_state), armed);
        if loads > 1 {
            STAGE += 1;
            if STAGE >= STAGE_COUNT {
                fail(FAILURE_ROUTE_ORDER, loads, STAGE as u32);
            }
        }
    }
}

/// Assert the authored source data this route depends on exists before the
/// first frame runs, exactly like the other authored-map probes.
pub fn setup(map: &ResidentMap) -> bool {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let Some(leaf) = hazard_leaf_index(map) else {
            fail(FAILURE_NO_HAZARD_LEAF, 1, 0);
            return false;
        };
        write_volatile(addr_of_mut!((*probe).monster_state_bounds), leaf as u32);
        let Some(entity) = artifact_entity_index(map) else {
            fail(FAILURE_NO_ARTIFACT, 1, 0);
            return false;
        };
        write_volatile(addr_of_mut!((*probe).transitions), entity as u32);
        true
    }
}

/// The authored slime volume this route falls into, found by scanning the
/// cooked leaf table rather than naming a coordinate.
fn hazard_leaf_index(map: &ResidentMap) -> Option<usize> {
    let leaves = map.leaves();
    (0..leaves.len()).find(|index| {
        leaves
            .get(*index)
            .is_some_and(|leaf| leaf.contents == CONTENTS_SLIME || leaf.contents == CONTENTS_LAVA)
    })
}

/// The authored `item_artifact_super_damage` this route collects.
fn artifact_entity_index(map: &ResidentMap) -> Option<usize> {
    let entities = map.entities();
    (0..entities.len()).find(|index| {
        entities.get(*index).is_some_and(|entity| {
            PowerupKind::from_class_name(entity.class_name) == Some(PowerupKind::Quad)
        })
    })
}

pub fn controls(map: EpisodeMap, player: &Player, weapon: &WeaponState) -> InputFrame {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if map != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, map_index(map), 0);
            return InputFrame::default();
        }
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        write_volatile(addr_of_mut!((*probe).stage_frames), STAGE as u32);
        if total > MAX_ROUTE_FRAMES {
            fail(FAILURE_TIMEOUT, ROUTE_INDEX as u32, STAGE as u32);
            return InputFrame::default();
        }

        let origin = player.origin();
        let x = origin.x >> 12;
        let y = origin.y >> 12;
        let z = origin.z >> 12;
        write_volatile(addr_of_mut!((*probe).last_health), x as u32);
        write_volatile(addr_of_mut!((*probe).state_ranges), y as u32);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), z as u32);
        let lowest = read_volatile(addr_of_mut!((*probe).weapon_pickups)) as i32;
        if lowest == 0 || z < lowest {
            write_volatile(addr_of_mut!((*probe).weapon_pickups), z as u32);
        }
        if (x - LAST_X).abs() + (y - LAST_Y).abs() > 2 {
            STUCK_FRAMES = 0;
        } else {
            STUCK_FRAMES = STUCK_FRAMES.saturating_add(1);
        }
        LAST_X = x;
        LAST_Y = y;
        let mut mechanisms = read_volatile(addr_of_mut!((*probe).player_state));
        if z < DESCENDED_Z {
            mechanisms |= DESCENDED;
            write_volatile(addr_of_mut!((*probe).player_state), mechanisms);
        }

        if weapon.inventory().health() <= 0 {
            // A corpse has one ordinary input left. PlayerDeathThink wants a
            // release before the press, so pulse rather than hold.
            return if total & 3 == 0 {
                InputFrame {
                    pressed: button::R2,
                    ..InputFrame::default()
                }
            } else {
                InputFrame::default()
            };
        }

        while let Some((route, index)) = route_cursor() {
            let Some(target) = route.get(index).copied() else {
                break;
            };
            let dx = target.x.saturating_sub(x);
            let dy = target.y.saturating_sub(y);
            let wrong_height = target.z != i32::MIN && (target.z - z).abs() > 8;
            if dx.abs() > target.radius || dy.abs() > target.radius || wrong_height {
                let sliding = STUCK_FRAMES > STUCK_FRAMES_BEFORE_SLIDE;
                let (dx, dy) = if !sliding {
                    (dx, dy)
                } else if (STUCK_FRAMES / SLIDE_FRAMES) & 1 == 0 {
                    (-dy, dx)
                } else {
                    (dy, -dx)
                };
                // Build eastward ground speed while the lift is still parked,
                // then atomically jump/use while the post-move origin remains
                // within the button's expanded USE box.  Starting the jump
                // at the setup station would cap acceleration to 30 u/s and
                // let the descending lift catch the player.
                let launch = target.jump && target.use_press && x >= -24;
                // The fall station begins only one hull-width from the shaft.
                // Waiting for the generic two-in-eight jump pulse can run off
                // the lip before CROSS is asserted, losing the vertical boost
                // this marginal 230-unit drop needs to cross the hard-land
                // cutoff.
                let fall_launch = target.require & FALL_DAMAGE != 0;
                let jump = sliding
                    || launch
                    || fall_launch
                    || (!target.use_press && target.jump && total & 7 < 2);
                let mut input = movement_input(player, dx, dy, jump);
                if launch {
                    input.pressed |= button::SQUARE;
                }
                return input;
            }
            let waiting =
                DWELL_FRAMES < target.dwell || mechanisms & target.require != target.require;
            if waiting {
                DWELL_FRAMES = DWELL_FRAMES.saturating_add(1);
                return if target.use_press {
                    InputFrame {
                        pressed: button::SQUARE,
                        ..InputFrame::default()
                    }
                } else {
                    InputFrame::default()
                };
            }
            advance();
        }

        // The stage is finished. Every stage but the last ends in the
        // player's own death, so the respawn is what starts the next one.
        if STAGE + 1 < STAGE_COUNT {
            return InputFrame::default();
        }
        if mechanisms & REQUIRED_MECHANISMS != REQUIRED_MECHANISMS {
            fail(FAILURE_ROUTE_ORDER, mechanisms, STAGE as u32);
            return InputFrame::default();
        }
        write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
        write_volatile(addr_of_mut!((*probe).complete), 1);
        InputFrame::default()
    }
}

/// The approach runs before every stage's own waypoints, so a respawn walks
/// the same authored corridor again instead of being placed.
fn route_cursor() -> Option<(&'static [Waypoint], usize)> {
    unsafe {
        if ROUTE_INDEX < LOWER_LEVEL_APPROACH.len() {
            return Some((LOWER_LEVEL_APPROACH, ROUTE_INDEX));
        }
        let stage = stage_route(STAGE);
        let index = ROUTE_INDEX - LOWER_LEVEL_APPROACH.len();
        (index < stage.len()).then_some((stage, index))
    }
}

unsafe fn advance() {
    ROUTE_INDEX += 1;
    DWELL_FRAMES = 0;
    // Reaching a waypoint is progress, and standing on a mover's control on
    // purpose is not being stuck. Carrying the count across the boundary made
    // the route slide sideways off a dwell instead of walking the leg it had
    // just waited for.
    STUCK_FRAMES = 0;
    let probe = addr_of_mut!(PROBE);
    write_volatile(addr_of_mut!((*probe).route_index), ROUTE_INDEX as u32);
}

/// Fold one frame of survival outcomes into the probe.
pub fn observe(
    frame: PlayerFrame,
    survival: quake_core::survival::SurvivalFrame,
    weapon: &WeaponState,
) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        let mut mechanisms = read_volatile(addr_of_mut!((*probe).player_state));

        // Motor health. A corpse is deliberately not advanced by the shipping
        // loop, so only a live player's motor is expected to run. Anything the
        // motor had to assume is sticky, and a motor that will not run at all
        // fails the gate outright.
        if weapon.inventory().health() > 0 {
            let motor = read_volatile(addr_of_mut!((*probe).target_edges));
            let bits = (motor | u32::from(frame.stalls.bits())) & 0xff;
            let stalled = if frame.motor_ran {
                0
            } else {
                ((motor >> 8).saturating_add(1)).min(0x00ff_ffff)
            };
            write_volatile(addr_of_mut!((*probe).target_edges), (stalled << 8) | bits);
            if stalled >= MOTOR_STALL_FRAMES_BEFORE_FAILURE {
                fail(FAILURE_MOTOR_STALL, bits, STAGE as u32);
                return;
            }
        }

        if frame.water_level != 0 {
            let levels = read_volatile(addr_of_mut!((*probe).shock_count));
            write_volatile(
                addr_of_mut!((*probe).shock_count),
                levels | (1 << frame.water_level),
            );
            let types = read_volatile(addr_of_mut!((*probe).intermission_state));
            let bit = match frame.water_type {
                CONTENTS_WATER => 1,
                CONTENTS_SLIME => 2,
                CONTENTS_LAVA => 4,
                _ => 0,
            };
            write_volatile(addr_of_mut!((*probe).intermission_state), types | bit);
        }

        let damage = survival.damage_taken.max(0) as u32;
        if damage != 0 {
            if frame
                .events
                .contains(quake_core::movement::MovementEvents::HARD_LAND)
            {
                let total =
                    read_volatile(addr_of_mut!((*probe).weapon_fired)).saturating_add(damage);
                write_volatile(addr_of_mut!((*probe).weapon_fired), total);
                mechanisms |= FALL_DAMAGE;
            } else if frame.water_level == 3 && frame.water_type == CONTENTS_WATER {
                let total =
                    read_volatile(addr_of_mut!((*probe).weapon_animated)).saturating_add(damage);
                write_volatile(addr_of_mut!((*probe).weapon_animated), total);
                mechanisms |= DROWN_DAMAGE;
            } else if matches!(frame.water_type, CONTENTS_SLIME | CONTENTS_LAVA) {
                let total =
                    read_volatile(addr_of_mut!((*probe).weapon_selected)).saturating_add(damage);
                write_volatile(addr_of_mut!((*probe).weapon_selected), total);
                mechanisms |= HAZARD_DAMAGE;
            }
        }

        if survival.died {
            let deaths = read_volatile(addr_of_mut!((*probe).monster_attack)).saturating_add(1);
            write_volatile(addr_of_mut!((*probe).monster_attack), deaths);
            if matches!(frame.water_type, CONTENTS_SLIME | CONTENTS_LAVA)
                || mechanisms & HAZARD_DAMAGE != 0
            {
                mechanisms |= HAZARD_DEATH;
            }
        }

        let inventory = weapon.inventory();
        let seconds = inventory.powerups().remaining_seconds(PowerupKind::Quad);
        if seconds != 0 {
            if mechanisms & POWERUP_TAKEN == 0 {
                mechanisms |= POWERUP_TAKEN;
                write_volatile(addr_of_mut!((*probe).monster_present), seconds as u32);
            }
            if seconds <= 15 && mechanisms & POWERUP_HALF_SPENT == 0 {
                mechanisms |= POWERUP_HALF_SPENT;
                write_volatile(addr_of_mut!((*probe).monster_animated), seconds as u32);
            }
        } else if mechanisms & POWERUP_HALF_SPENT != 0 {
            mechanisms |= POWERUP_EXPIRED;
        }

        // The respawn loadout is asserted the first frame after a restart.
        let health = inventory.health();
        if PREVIOUS_HEALTH <= 0 && health > 0 {
            let respawns = read_volatile(addr_of_mut!((*probe).monster_pain)).saturating_add(1);
            write_volatile(addr_of_mut!((*probe).monster_pain), respawns);
            write_volatile(addr_of_mut!((*probe).monster_death), health as u32);
            write_volatile(
                addr_of_mut!((*probe).boss),
                u32::from(inventory.ammo(AmmoKind::Shells)),
            );
            let clean = health == 100
                && inventory.armor() == 0
                && inventory.keys() == 0
                && inventory.ammo(AmmoKind::Shells) == 25
                && inventory.ammo(AmmoKind::Nails) == 0
                && inventory.ammo(AmmoKind::Rockets) == 0
                && inventory.ammo(AmmoKind::Cells) == 0
                && inventory.active_weapon() == Weapon::Shotgun
                && !inventory.owns(Weapon::SuperShotgun)
                && PowerupKind::ALL
                    .iter()
                    .all(|kind| !inventory.powerups().active(*kind));
            if clean {
                mechanisms |= RESPAWN_LOADOUT;
            } else {
                fail(FAILURE_RESPAWN_LOADOUT, health as u32, respawns);
                return;
            }
        }
        PREVIOUS_HEALTH = health;
        write_volatile(addr_of_mut!((*probe).player_state), mechanisms);
    }
}

fn movement_input(player: &Player, dx: i32, dy: i32, jump: bool) -> InputFrame {
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    // One route decision spans several 60 Hz ticks, so taper hard with
    // distance: full throttle across a small waypoint overshoots it and the
    // recovery diagonal wedges the player on door frames.
    let limit = dx.abs().max(dy.abs()).saturating_mul(2).clamp(16, 127);
    InputFrame {
        movement: [
            (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
            (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        ],
        held: if jump { button::CROSS } else { 0 },
        ..InputFrame::default()
    }
}

fn fail(code: u32, detail: u32, stage: u32) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).failure_code), code);
        write_volatile(addr_of_mut!((*probe).failure_map), 1);
        write_volatile(addr_of_mut!((*probe).failure_detail), detail);
        write_volatile(addr_of_mut!((*probe).failure_entity), stage);
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
