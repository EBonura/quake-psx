//! E1M7 Chthon and episode-completion regression.
//!
//! Ordinary pad input collects the sigil, wakes and kills Chthon through the
//! authored lightning mechanism, reaches the exit and returns to Start. The
//! probe checks rune carry-over and the alternate Start spawn. It begins in
//! E1M7, so it is not a complete Episode 1 playthrough.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::int32::mul_q12_i32;

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::{EntityScene, GameplayResult};
use crate::input::InputFrame;
use crate::player::Player;

const PROBE_MAGIC: u32 = 0x5150_5358;
/// Version of the shared gameplay probe used by this regression.
const PROBE_VERSION: u32 = 13;

const PHASE_SIGIL: u32 = 1;
const PHASE_BOSS: u32 = 2;
const PHASE_ARENA: u32 = 3;
/// Standing on the lift, waiting for it to carry the player to the ring.
const PHASE_LIFT: u32 = 6;
/// On the ring, walking onto the `event_lightning` button.
const PHASE_RING: u32 = 7;
/// Off the ring, across the bridge Chthon's death closed, down the shaft.
const PHASE_DESCENT: u32 = 8;
const PHASE_EXIT: u32 = 4;
const PHASE_START: u32 = 5;
const PHASE_COMPLETE: u32 = 0xe1;
const PHASE_ERROR: u32 = 0xff;

const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_TIMEOUT: u32 = 2;
const FAILURE_TARGET_GRAPH: u32 = 3;
const FAILURE_NO_SIGIL: u32 = 4;
const FAILURE_NO_BOSS: u32 = 5;
const FAILURE_NO_CHANGELEVEL: u32 = 6;
const FAILURE_EPISODE_STATE: u32 = 7;
const FAILURE_PLAYER_DEAD: u32 = 8;

const MAX_STAGE_FRAMES: u32 = 2_400;

/// `item_sigil` touched and the rune folded into `serverflags`.
const SIGIL_TAKEN: u32 = 1 << 0;
/// The sigil's authored `target` reached `monster_boss`.
const BOSS_AWAKE: u32 = 1 << 1;
/// Chthon is out of the lava: active, visible, alive, animating.
const BOSS_RISEN: u32 = 1 << 2;
/// A lava ball left him.
const BOSS_THREW: u32 = 1 << 3;
/// The arena's east corridor was walked to the map's own lift on foot.
const ARENA_WALKED: u32 = 1 << 4;
/// The authored `trigger_changelevel` fired.
const CHANGELEVEL: u32 = 1 << 5;
/// The end-of-level panel reported the episode finished.
const EPISODE_PANEL: u32 = 1 << 6;
/// The rune survived the map load.
const RUNE_PERSISTED: u32 = 1 << 7;
/// Start spawned the player on `info_player_start2`.
const START2_SPAWN: u32 = 1 << 8;
/// The rune-1 `func_episodegate` spawned and is solid.
const EPISODE_GATE: u32 = 1 << 9;
/// `func_bossgate` is still there: shareware can only ever hold one rune.
const BOSS_GATE: u32 = 1 << 10;
/// The lift carried the player from the arena floor up to the button ring.
const LIFT_RIDDEN: u32 = 1 << 11;
/// The `event_lightning` chain delivered at least one shock.
const BOSS_SHOCKED: u32 = 1 << 12;
/// Chthon died, and only the shock chain could have done it.
const BOSS_DEAD: u32 = 1 << 13;
/// The player left the ring and stood on the bridge his death closed.
const BRIDGE_WALKED: u32 = 1 << 14;
/// The shaft behind the opened gate carried the player down to the chamber.
const SHAFT_FALLEN: u32 = 1 << 15;
/// The route began at E1M7's authored player spawn, before any input ran.
const START_SPAWN: u32 = 1 << 16;
/// E1M7's exit ran `SUB_UseTargets` into its authored finale relay.
const CHANGELEVEL_TARGETS: u32 = 1 << 17;

const REQUIRED: u32 = SIGIL_TAKEN
    | BOSS_AWAKE
    | BOSS_RISEN
    | BOSS_THREW
    | ARENA_WALKED
    | CHANGELEVEL
    | EPISODE_PANEL
    | RUNE_PERSISTED
    | START2_SPAWN
    | EPISODE_GATE
    | BOSS_GATE
    | LIFT_RIDDEN
    | BOSS_SHOCKED
    | BOSS_DEAD
    | BRIDGE_WALKED
    | SHAFT_FALLEN
    | START_SPAWN
    | CHANGELEVEL_TARGETS;

const CLASS_INFO_PLAYER_START2: u8 = 0x18;
const CLASS_FUNC_BOSSGATE: u8 = 0x0a;
const CLASS_FUNC_EPISODEGATE: u8 = 0x0e;
/// `func_episodegate` spawnflag bits below `RUNE_MASK` name the episode.
const RUNE_ONE: u16 = 1;

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
            phase: PHASE_SIGIL,
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
    radius: i32,
    /// The route is only at this point once it is at or below this height.
    /// Every leg of the descent passes over its own destination in the air
    /// first, so a plan-view radius alone would accept the waypoint while the
    /// player is still a thousand units above the floor it names.
    /// [`ANY_HEIGHT`] leaves the point unconstrained.
    ceiling: i32,
    /// Mechanism bits that must be in hand before the route may leave this
    /// point. A pickup waypoint keeps steering onto the item until the touch
    /// actually lands rather than accepting an arrival radius.
    require: u32,
}

/// A waypoint the route may reach at any height.
const ANY_HEIGHT: i32 = i32::MAX;

const fn waypoint(x: i32, y: i32) -> Waypoint {
    Waypoint {
        x,
        y,
        radius: 24,
        ceiling: ANY_HEIGHT,
        require: 0,
    }
}

/// E1M7's sigil sits on the west walkway; the rest is the arena's south
/// corridor out to the far side. Every point came out of the cooked map:
/// `path --from` the cooked `info_player_start` `--to` the cooked `item_sigil`
/// `--to` the far side of the arena, then validated by walking it with the
/// same movement core the guest links.
const SIGIL_ROUTE: &[Waypoint] = &[Waypoint {
    // The cooked `item_sigil` origin, not an approach point: the route has to
    // stand on it for `sigil_touch` to run.
    x: 8,
    y: 64,
    radius: 8,
    ceiling: ANY_HEIGHT,
    require: SIGIL_TAKEN | BOSS_AWAKE,
}];
/// The arena floor out to the map's own lift, then the lift's own deck. Every
/// point is `tools/routesim` `path` output over the cooked E1M7 hull, from the
/// `item_sigil` origin to the centre of the `func_plat` brush.
const ARENA_ROUTE: &[Waypoint] = &[
    waypoint(-232, 112),
    waypoint(-208, 352),
    waypoint(-160, 400),
    waypoint(920, 400),
    waypoint(992, 328),
    waypoint(1064, 112),
    waypoint(1208, 88),
    Waypoint {
        // The lift deck's own centre. `func_plat`'s automatic touch fires from
        // the deck, so the route has to stand on it rather than beside it.
        x: 1216,
        y: 64,
        radius: 16,
        ceiling: ANY_HEIGHT,
        require: 0,
    },
];
/// The button ring, walked west from the lift to the `func_button` whose
/// authored target is `event_lightning`.
const RING_ROUTE: &[Waypoint] = &[Waypoint {
    x: 832,
    y: 64,
    radius: 16,
    ceiling: ANY_HEIGHT,
    require: BOSS_SHOCKED | BOSS_DEAD,
}];
/// The walk down to E1M7's own exit, every point off the cooked map.
///
/// Chthon's death fires the `trigger_relay` his `target` names, which fires
/// the name three doors carry: the two `DOOR_START_OPEN` halves of the lava
/// bridge (`*8` and `*9`, whose authored volume spans x 641..767 over the
/// lava and which therefore only EXIST once triggered) and the shaft gate
/// `*39` at x 777..791. So the descent is only walkable after the kill, which
/// is exactly what makes it the map's own exit gating.
const DESCENT_ROUTE: &[Waypoint] = &[
    Waypoint {
        // West along the ring and off its west edge. The bridge deck below is
        // 168 units down, under Quake's own fall-damage cutoff, so the drop
        // costs nothing. The ceiling is what forbids accepting this point
        // while still walking the ring directly above it.
        x: 696,
        y: 64,
        radius: 20,
        ceiling: 40,
        require: 0,
    },
    Waypoint {
        // East across the bridge, over its sill and through the opened gate
        // into the shaft mouth. The route steps in and gravity does the rest.
        x: 824,
        y: 64,
        radius: 12,
        ceiling: ANY_HEIGHT,
        require: 0,
    },
    Waypoint {
        // The shaft drops into an L-shaped chamber. Walking diagonally from
        // the landing to the exit catches the inside corner, so turn at the
        // cooked hull's real bend before entering the changelevel volume.
        x: 992,
        y: 24,
        radius: 12,
        ceiling: -900,
        require: 0,
    },
    Waypoint {
        // The middle of the authored `trigger_changelevel` volume, whose
        // cooked brush spans x 1001..1031, y 1..103, z -1055..-945. The
        // ceiling keeps the shaft mouth directly above from counting.
        x: 1016,
        y: 52,
        radius: 24,
        ceiling: -900,
        require: 0,
    },
];
/// The lift's own travel, from `func_plat`'s authored `height`. Reaching the
/// ring means gaining all of it.
const LIFT_TRAVEL_UNITS: i32 = 176;
/// The bridge deck Chthon's death closes, in whole units. Standing at or
/// below this after the ring means the route left the ring for real.
const BRIDGE_DECK_UNITS: i32 = 40;
/// The exit chamber floor, a thousand units under the arena.
const CHAMBER_FLOOR_UNITS: i32 = -900;

#[used]
static mut PROBE: Probe = Probe::new();
static mut ROUTE_INDEX: usize = 0;
static mut STALL_FRAMES: u32 = 0;
static mut DETOUR: usize = 0;
static mut BEST_DISTANCE: i32 = i32::MAX;
static mut BOSS_FRAMES: u32 = 0;
/// Player height in whole units when the lift ride began.
static mut LIFT_BASE_Z: i32 = i32::MIN;
/// Player height in whole units on the previous control frame.
static mut LAST_Z: i32 = i32::MAX;

pub const fn initial_map() -> EpisodeMap {
    EpisodeMap::E1M7
}

pub fn map_loaded(map: EpisodeMap, entities: &EntityScene, world: &ResidentMap, player: &Player) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let bit = 1u32 << map_index(map);
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | bit,
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_index(map));
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).wrapping_add(1),
        );
        write_volatile(addr_of_mut!((*probe).stage_frames), 0);
        ROUTE_INDEX = 0;
        BEST_DISTANCE = i32::MAX;
        STALL_FRAMES = 0;
        DETOUR = 0;
        LIFT_BASE_Z = i32::MIN;
        LAST_Z = i32::MAX;
        match map {
            EpisodeMap::E1M7 => {
                // `load_level` built `player` through the shipping
                // `Player::from_start` path. Pin it against the cooked player
                // record so a future setup placement cannot silently turn
                // this back into a direct-start encounter proof.
                let authored_start = world
                    .entities()
                    .get(1)
                    .filter(|entity| entity.class_name == 0x01);
                let at_authored_start = authored_start.is_some_and(|entity| {
                    let origin = player.origin();
                    (origin.x - entity.origin.x).abs() < (1 << 12)
                        && (origin.y - entity.origin.y).abs() < (1 << 12)
                        && (origin.z - entity.origin.z).abs() < (1 << 12)
                });
                if !at_authored_start {
                    fail(FAILURE_BAD_MAP, map_index(map), 0x5350_4157);
                    return;
                }
                write_volatile(
                    addr_of_mut!((*probe).player_state),
                    read_volatile(addr_of_mut!((*probe).player_state)) | START_SPAWN,
                );
                if entities.regression_class_present(world, 0x27) == 0 {
                    fail(FAILURE_NO_SIGIL, map_index(map), 0);
                }
                // The descent's last waypoint names the exit volume, so keep
                // it tied to the cooked brush rather than to a number typed
                // into this file: a recook that moved the volume has to fail
                // the gate instead of walking the route into a wall.
                let exit = DESCENT_ROUTE[DESCENT_ROUTE.len() - 1];
                match entities.regression_change_level_origin(EpisodeMap::Start) {
                    Some(origin) => {
                        let authored_x = origin.x >> 12;
                        let authored_y = origin.y >> 12;
                        let authored_z = origin.z >> 12;
                        if (authored_x - exit.x).abs() > exit.radius
                            || (authored_y - exit.y).abs() > exit.radius
                            || authored_z > exit.ceiling
                        {
                            fail(
                                FAILURE_NO_CHANGELEVEL,
                                map_index(map),
                                (authored_x as u32) << 16 | (authored_y as u32 & 0xffff),
                            );
                        }
                    }
                    None => fail(FAILURE_NO_CHANGELEVEL, map_index(map), 0),
                }
            }
            EpisodeMap::Start => {
                write_volatile(addr_of_mut!((*probe).phase), PHASE_START);
                validate_episode_state(entities, world, player);
            }
            _ => fail(FAILURE_BAD_MAP, map_index(map), 0),
        }
    }
}

/// The Start-side half of the contract, checked the frame Start finishes
/// loading: the rune came across, `SelectSpawnPoint` used
/// `info_player_start2`, the rune-1 `func_episodegate` now exists and is
/// solid, and `func_bossgate` is still shut because shareware can only ever
/// hold one of the four runes.
fn validate_episode_state(entities: &EntityScene, world: &ResidentMap, player: &Player) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let mut state = read_volatile(addr_of_mut!((*probe).player_state));
        if entities.runes() & 1 != 0 {
            state |= RUNE_PERSISTED;
        }
        let start2 = world
            .entities()
            .iter()
            .find(|entity| entity.class_name == CLASS_INFO_PLAYER_START2)
            .map(|entity| entity.origin);
        if let Some(origin) = start2 {
            let here = player.origin();
            if (here.x - origin.x).abs() < (1 << 12)
                && (here.y - origin.y).abs() < (1 << 12)
                && (here.z - origin.z).abs() < (1 << 12)
            {
                state |= START2_SPAWN;
            }
        }
        if entities.regression_solid_gate(world, CLASS_FUNC_EPISODEGATE, RUNE_ONE) {
            state |= EPISODE_GATE;
        }
        if entities.regression_class_present(world, CLASS_FUNC_BOSSGATE) != 0 {
            state |= BOSS_GATE;
        }
        write_volatile(addr_of_mut!((*probe).player_state), state);
        write_volatile(addr_of_mut!((*probe).valid_state_ranges), state);
        if state & REQUIRED != REQUIRED {
            fail(FAILURE_EPISODE_STATE, 0, state);
            return;
        }
        write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
        write_volatile(addr_of_mut!((*probe).complete), 1);
    }
}

pub fn controls(world: &ResidentMap, entities: &EntityScene, player: &Player) -> InputFrame {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0
            || read_volatile(addr_of_mut!((*probe).complete)) != 0
        {
            return InputFrame::default();
        }
        let total = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), total);
        let stage_frames = read_volatile(addr_of_mut!((*probe).stage_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).stage_frames), stage_frames);
        if stage_frames > MAX_STAGE_FRAMES {
            fail(
                FAILURE_TIMEOUT,
                map_index(world.map()),
                read_volatile(addr_of_mut!((*probe).phase)),
            );
            return InputFrame::default();
        }
        if world.map() != EpisodeMap::E1M7 {
            return InputFrame::default();
        }

        let phase = read_volatile(addr_of_mut!((*probe).phase));
        let mut state = read_volatile(addr_of_mut!((*probe).player_state));
        let camera = player.camera();
        let x = camera.origin.x >> 12;
        let y = camera.origin.y >> 12;
        write_volatile(addr_of_mut!((*probe).state_ranges), x as u32);
        write_volatile(addr_of_mut!((*probe).monster_state_bounds), y as u32);
        write_volatile(
            addr_of_mut!((*probe).weapon_selected),
            (camera.origin.z >> 12) as u32,
        );

        let z = camera.origin.z >> 12;
        // The lowest the player has been since the arena walk started. The
        // arena floor is the only place this route can be low, and the lift is
        // the only way up, so the height gained over it is the ride.
        if matches!(phase, PHASE_ARENA | PHASE_LIFT) && (LIFT_BASE_Z == i32::MIN || z < LIFT_BASE_Z)
        {
            LIFT_BASE_Z = z;
        }
        // The descent's two heights are its own proof: the bridge deck only
        // exists once Chthon's death closed it, and the chamber floor is only
        // reachable through the gate his death opened.
        if phase == PHASE_DESCENT {
            let descended = state
                | if z <= BRIDGE_DECK_UNITS {
                    BRIDGE_WALKED
                } else {
                    0
                }
                | if z <= CHAMBER_FLOOR_UNITS {
                    SHAFT_FALLEN
                } else {
                    0
                };
            if descended != state {
                state = descended;
                write_volatile(addr_of_mut!((*probe).player_state), state);
            }
        }
        let route: &[Waypoint] = match phase {
            PHASE_SIGIL => SIGIL_ROUTE,
            PHASE_ARENA => ARENA_ROUTE,
            PHASE_RING => RING_ROUTE,
            PHASE_DESCENT => {
                // The bridge is built by movers, and a mover takes time. The
                // route waits on the runtime reporting them settled rather
                // than on a frame count, because the alternative is stepping
                // off the ring into the lava the bridge has not covered yet.
                match entities.regression_movers_settled(world, boss_death_target(world)) {
                    Some(true) => DESCENT_ROUTE,
                    Some(false) => return InputFrame::default(),
                    None => {
                        fail(FAILURE_NO_CHANGELEVEL, map_index(world.map()), 1);
                        return InputFrame::default();
                    }
                }
            }
            PHASE_LIFT => {
                // Standing still on the deck: `func_plat` is an automatic
                // mover, so the touch that started it is the only input the
                // ride needs, and the carry is what the runtime has to do.
                if z - LIFT_BASE_Z >= LIFT_TRAVEL_UNITS {
                    write_volatile(addr_of_mut!((*probe).player_state), state | LIFT_RIDDEN);
                    write_volatile(addr_of_mut!((*probe).phase), PHASE_RING);
                    write_volatile(addr_of_mut!((*probe).stage_frames), 0);
                    ROUTE_INDEX = 0;
                    BEST_DISTANCE = i32::MAX;
                    STALL_FRAMES = 0;
                    DETOUR = 0;
                }
                return InputFrame::default();
            }
            PHASE_BOSS => {
                // Stand still and let the fight start: he has to be out of the
                // lava, animating and throwing before the arena walk begins.
                BOSS_FRAMES = BOSS_FRAMES.wrapping_add(1);
                if state & (BOSS_RISEN | BOSS_THREW) == (BOSS_RISEN | BOSS_THREW) {
                    write_volatile(addr_of_mut!((*probe).phase), PHASE_ARENA);
                    write_volatile(addr_of_mut!((*probe).stage_frames), 0);
                    BEST_DISTANCE = i32::MAX;
                    STALL_FRAMES = 0;
                    DETOUR = 0;
                }
                return InputFrame::default();
            }
            _ => return InputFrame::default(),
        };

        // Losing height is progress on this route, so a fall must not look
        // like a stall. Without this the detour would rotate mid-shaft and
        // spend Quake's air control steering the drop into a wall.
        let descending = z < LAST_Z;
        LAST_Z = z;
        while let Some(target) = route.get(ROUTE_INDEX).copied() {
            let dx = target.x.saturating_sub(x);
            let dy = target.y.saturating_sub(y);
            let in_plan = dx.abs() <= target.radius && dy.abs() <= target.radius;
            if !in_plan || z > target.ceiling {
                let distance = dx.abs().saturating_add(dy.abs());
                if distance < BEST_DISTANCE || descending {
                    BEST_DISTANCE = BEST_DISTANCE.min(distance);
                    STALL_FRAMES = 0;
                    DETOUR = 0;
                } else {
                    STALL_FRAMES += 1;
                    if STALL_FRAMES > 45 {
                        STALL_FRAMES = 0;
                        DETOUR = (DETOUR + 1) & 7;
                    }
                }
                // Inside the plan radius but still above the floor this point
                // names: the route is on a ledge over its own destination and
                // has to walk off it. The arrival taper would park it on the
                // edge instead, so steer at the radius rather than the
                // vanishing delta.
                let (dx, dy) = if in_plan {
                    (extend(dx, target.radius), extend(dy, target.radius))
                } else {
                    (dx, dy)
                };
                let (dx, dy) = detoured(dx, dy, DETOUR);
                return movement_input(player, dx, dy);
            }
            if state & target.require != target.require {
                // Standing on the item without the touch having landed yet:
                // keep nudging across it instead of accepting the arrival.
                let (dx, dy) = detoured(
                    if dx == 0 { target.radius } else { dx },
                    dy,
                    (total as usize >> 4) & 7,
                );
                return movement_input(player, dx, dy);
            }
            ROUTE_INDEX += 1;
            write_volatile(addr_of_mut!((*probe).route_index), ROUTE_INDEX as u32);
            BEST_DISTANCE = i32::MAX;
            STALL_FRAMES = 0;
            DETOUR = 0;
        }

        // Route finished.
        match phase {
            PHASE_SIGIL => {
                if state & (SIGIL_TAKEN | BOSS_AWAKE) != (SIGIL_TAKEN | BOSS_AWAKE) {
                    fail(FAILURE_NO_SIGIL, map_index(world.map()), state);
                    return InputFrame::default();
                }
                if entities.regression_boss(world).is_none() {
                    fail(FAILURE_NO_BOSS, map_index(world.map()), 0);
                    return InputFrame::default();
                }
                write_volatile(addr_of_mut!((*probe).phase), PHASE_BOSS);
                write_volatile(addr_of_mut!((*probe).stage_frames), 0);
                ROUTE_INDEX = 0;
            }
            PHASE_ARENA => {
                write_volatile(addr_of_mut!((*probe).player_state), state | ARENA_WALKED);
                write_volatile(addr_of_mut!((*probe).phase), PHASE_LIFT);
                write_volatile(addr_of_mut!((*probe).stage_frames), 0);
            }
            PHASE_RING => {
                write_volatile(addr_of_mut!((*probe).phase), PHASE_DESCENT);
                write_volatile(addr_of_mut!((*probe).stage_frames), 0);
                ROUTE_INDEX = 0;
                BEST_DISTANCE = i32::MAX;
                STALL_FRAMES = 0;
                DETOUR = 0;
                LAST_Z = i32::MAX;
            }
            PHASE_DESCENT => {
                write_volatile(addr_of_mut!((*probe).phase), PHASE_EXIT);
                write_volatile(addr_of_mut!((*probe).stage_frames), 0);
            }
            _ => {}
        }
        InputFrame::default()
    }
}

pub fn observe(
    world: &ResidentMap,
    entities: &EntityScene,
    gameplay: GameplayResult,
    pickup: crate::entity::PickupResult,
    health: i16,
) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).last_health), health.max(0) as u32);
        if let Some(error) = gameplay.target_error.or(pickup.target_error) {
            fail(FAILURE_TARGET_GRAPH, map_index(world.map()), error as u32);
            return;
        }
        if health <= 0 {
            fail(
                FAILURE_PLAYER_DEAD,
                map_index(world.map()),
                read_volatile(addr_of_mut!((*probe).phase)),
            );
            return;
        }
        write_volatile(
            addr_of_mut!((*probe).target_edges),
            read_volatile(addr_of_mut!((*probe).target_edges))
                .saturating_add(u32::from(gameplay.fired_target_edges)),
        );
        if world.map() != EpisodeMap::E1M7 {
            return;
        }
        let mut state = read_volatile(addr_of_mut!((*probe).player_state));
        if entities.runes() & 1 != 0 {
            state |= SIGIL_TAKEN;
        }
        if gameplay.boss_awakened || pickup.boss_awakened {
            state |= BOSS_AWAKE;
        }
        if let Some(boss) = entities.regression_boss(world) {
            write_volatile(addr_of_mut!((*probe).boss), u32::from(boss.frame));
            write_volatile(
                addr_of_mut!((*probe).monster_present),
                u32::from(boss.active),
            );
            if boss.active && boss.visible && !boss.dead {
                state |= BOSS_RISEN;
            }
            if boss.active && boss.throwing {
                let seen = read_volatile(addr_of_mut!((*probe).shock_count)).saturating_add(1);
                write_volatile(addr_of_mut!((*probe).shock_count), seen);
                state |= BOSS_THREW;
            }
            if boss.dead {
                state |= BOSS_DEAD;
            }
        }
        let activations = gameplay.player_activated_movers.iter().flatten().count() as u32;
        if activations != 0 {
            let seen =
                read_volatile(addr_of_mut!((*probe).weapon_fired)).saturating_add(activations);
            write_volatile(addr_of_mut!((*probe).weapon_fired), seen);
        }
        if gameplay.boss_shocks != 0 {
            state |= BOSS_SHOCKED;
        }
        write_volatile(addr_of_mut!((*probe).player_state), state);
    }
}

/// The end-of-level panel the Chthon map raises on its way back to Start.
pub fn observe_intermission(view: &quake_core::level::IntermissionView) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let mut state = read_volatile(addr_of_mut!((*probe).player_state)) | CHANGELEVEL;
        if view.episode != quake_core::level::IntermissionView::EPISODE_NONE {
            state |= EPISODE_PANEL;
        }
        write_volatile(addr_of_mut!((*probe).player_state), state);
        write_volatile(
            addr_of_mut!((*probe).intermission_state),
            (u32::from(view.kills) << 24)
                | (u32::from(view.total_kills) << 16)
                | (u32::from(view.secrets) << 8)
                | u32::from(view.total_secrets),
        );
        write_volatile(
            addr_of_mut!((*probe).transitions),
            read_volatile(addr_of_mut!((*probe).transitions)).wrapping_add(1),
        );
    }
}

/// Pin the otherwise invisible `changelevel_touch -> SUB_UseTargets` edge.
/// The source and its target are read from the cooked E1M7 entity table, so a
/// route cannot earn this bit merely by entering an arbitrary exit volume.
pub fn observe_changelevel_targets(world: &ResidentMap, source_index: u16, edges: u16) {
    if world.map() != EpisodeMap::E1M7 || edges == 0 {
        return;
    }
    let Some(source) = world.entities().get(source_index as usize) else {
        return;
    };
    if source.class_name != 0x47 || source.target == 0 {
        return;
    }
    unsafe {
        let probe = addr_of_mut!(PROBE);
        let state = read_volatile(addr_of_mut!((*probe).player_state));
        write_volatile(
            addr_of_mut!((*probe).player_state),
            state | CHANGELEVEL_TARGETS,
        );
    }
}

/// The name the doors Chthon's death drives carry, walked out of the cooked
/// entity table rather than typed in: `monster_boss`'s own `target` names a
/// `trigger_relay`, and that relay's `target` is the doors' `targetname`.
fn boss_death_target(world: &ResidentMap) -> u16 {
    const CLASS_MONSTER_BOSS: u8 = 0x37;
    let sources = world.entities();
    let Some(relay_name) = sources
        .iter()
        .find(|entity| entity.class_name == CLASS_MONSTER_BOSS)
        .map(|entity| entity.target)
        .filter(|target| *target != 0)
    else {
        return 0;
    };
    sources
        .iter()
        .find(|entity| entity.target_name == relay_name && entity.target != 0)
        .map(|entity| entity.target)
        .unwrap_or(0)
}

/// Grow a component that has shrunk inside the arrival radius back out to it,
/// keeping its sign and leaving a dead-centre axis alone.
fn extend(delta: i32, radius: i32) -> i32 {
    if delta > 0 {
        delta.max(radius)
    } else if delta < 0 {
        delta.min(-radius)
    } else {
        0
    }
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

fn movement_input(player: &Player, dx: i32, dy: i32) -> InputFrame {
    let yaw = player.view_angles[1] as u16 & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    let limit = dx.abs().max(dy.abs()).saturating_mul(6).clamp(16, 127);
    InputFrame {
        movement: [
            (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
            (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        ],
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
