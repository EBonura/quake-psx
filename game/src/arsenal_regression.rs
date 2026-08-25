//! Emulator regression for authored pickups and the Episode 1 arsenal.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::atan2_q12;
use psx_math::int32::{isqrt_i32, square_i32_saturating};
use quake_core::combat::{AmmoKind, Weapon, WeaponState, WeaponView, NAIL_POOL_CAPACITY};
use quake_formats::Vec3I32;

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::{EntityScene, GrenadeResult, NailResult, PickupResult, RocketResult};
use crate::player::Player;
use crate::renderer::RenderStats;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 5;
const ROUTE_MAP_BITS: u32 = 0x3e;
const ROUTE_TRANSITION_BITS: u32 = 0x3c;
const SHOTGUN_BIT: u32 = 1 << 1;
const SUPER_SHOTGUN_BIT: u32 = 1 << 2;
const NAILGUN_BIT: u32 = 1 << 3;
const SUPER_NAILGUN_BIT: u32 = 1 << 4;
const GRENADE_BIT: u32 = 1 << 5;
const ROCKET_BIT: u32 = 1 << 6;
const ALL_WEAPON_BITS: u32 =
    SHOTGUN_BIT | SUPER_SHOTGUN_BIT | NAILGUN_BIT | SUPER_NAILGUN_BIT | GRENADE_BIT | ROCKET_BIT;
const ALL_PICKUP_BITS: u32 = ALL_WEAPON_BITS & !SHOTGUN_BIT;
const ALL_PROJECTILE_BITS: u32 = NAILGUN_BIT | SUPER_NAILGUN_BIT | GRENADE_BIT | ROCKET_BIT;
const EXPLOSION_PRESENTATION_BIT: u32 = 1 << 31;
const PHASE_PICKUP: u32 = 0x61;
const PHASE_TRANSITION: u32 = 0x5e;
const PHASE_WAIT_MAP: u32 = 0x5f;
const PHASE_PICKUP_DONE: u32 = 0x62;
const PHASE_NAIL_POOL: u32 = 0x6a;
const PHASE_NAIL_POOL_DONE: u32 = 0x6c;
const PHASE_SHOTGUN: u32 = 0x63;
const PHASE_SUPER_SHOTGUN: u32 = 0x64;
const PHASE_NAILGUN: u32 = 0x65;
const PHASE_SUPER_NAILGUN: u32 = 0x66;
const PHASE_GRENADE: u32 = 0x67;
const PHASE_ROCKET: u32 = 0x68;
const PHASE_ROCKET_RENDER: u32 = 0x69;
const PHASE_LIGHTNING_RENDER: u32 = 0x6b;
const PHASE_COMPLETE: u32 = 0x70;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_NO_PICKUP: u32 = 2;
const FAILURE_NO_TARGET: u32 = 3;
const FAILURE_PICKUP: u32 = 4;
const FAILURE_TIMEOUT: u32 = 5;
const FAILURE_NAIL_POOL: u32 = 6;
const FAILURE_LIGHTNING_TRACE: u32 = 7;
const MAX_FRAMES: u32 = 210;

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
    pickup_sources: [u16; 2],
    pickup_origins: [Vec3I32; 2],
    pickup_classes: [u8; 2],
    pickup_weapons: [Weapon; 2],
    pickup_bits: [u32; 2],
    pickup_count: u8,
    pickup_index: u8,
    target_source: u16,
    target_eye: Vec3I32,
    render_eye: Vec3I32,
    target_point: Vec3I32,
    initial_health: i16,
    last_health: i16,
    rocket_health: i16,
    nail_pool_ammo: u16,
}

impl State {
    const fn new() -> Self {
        Self {
            initialized: false,
            pickup_sources: [0; 2],
            pickup_origins: [Vec3I32 { x: 0, y: 0, z: 0 }; 2],
            pickup_classes: [0; 2],
            pickup_weapons: [Weapon::Axe; 2],
            pickup_bits: [0; 2],
            pickup_count: 0,
            pickup_index: 0,
            target_source: 0,
            target_eye: Vec3I32 { x: 0, y: 0, z: 0 },
            render_eye: Vec3I32 { x: 0, y: 0, z: 0 },
            target_point: Vec3I32 { x: 0, y: 0, z: 0 },
            initial_health: 0,
            last_health: 0,
            rocket_health: 0,
            nail_pool_ammo: 0,
        }
    }
}

static mut STATE: State = State::new();

pub fn setup(world: &ResidentMap, entities: &EntityScene, player: &mut Player) -> bool {
    unsafe {
        if world.map() != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, map_number(world.map()), 0);
            return false;
        }
        let state = &mut *addr_of_mut!(STATE);
        state.initialized = true;
        map_loaded(world, entities, player)
    }
}

pub fn map_loaded(world: &ResidentMap, entities: &EntityScene, player: &mut Player) -> bool {
    unsafe {
        let map_number = map_number(world.map());
        if !(1..=5).contains(&map_number) {
            fail(FAILURE_BAD_MAP, map_number, 0);
            return false;
        }
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized {
            return false;
        }
        let (pickups, pickup_count) = match world.map() {
            EpisodeMap::E1M1 => (
                [
                    (0x55, Weapon::Nailgun, NAILGUN_BIT),
                    (0x58, Weapon::SuperShotgun, SUPER_SHOTGUN_BIT),
                ],
                2,
            ),
            EpisodeMap::E1M3 => (
                [
                    (0x53, Weapon::GrenadeLauncher, GRENADE_BIT),
                    (0, Weapon::Axe, 0),
                ],
                1,
            ),
            EpisodeMap::E1M4 => (
                [
                    (0x57, Weapon::SuperNailgun, SUPER_NAILGUN_BIT),
                    (0, Weapon::Axe, 0),
                ],
                1,
            ),
            EpisodeMap::E1M5 => (
                [
                    (0x56, Weapon::RocketLauncher, ROCKET_BIT),
                    (0, Weapon::Axe, 0),
                ],
                1,
            ),
            EpisodeMap::E1M2 => ([(0, Weapon::Axe, 0); 2], 0),
            _ => return false,
        };
        state.pickup_count = pickup_count;
        state.pickup_index = 0;
        let mut pickup_index = 0usize;
        while pickup_index < pickup_count as usize {
            let (class_name, weapon, bit) = pickups[pickup_index];
            let Some((source, origin)) = entities.regression_pickup_origin(world, class_name)
            else {
                fail(FAILURE_NO_PICKUP, map_number, u32::from(class_name));
                return false;
            };
            state.pickup_sources[pickup_index] = source;
            state.pickup_origins[pickup_index] = origin;
            state.pickup_classes[pickup_index] = class_name;
            state.pickup_weapons[pickup_index] = weapon;
            state.pickup_bits[pickup_index] = bit;
            pickup_index += 1;
        }
        if world.map() == EpisodeMap::E1M5 {
            let Some((target_source, eye, target)) = entities.regression_arsenal_shot_setup(world)
            else {
                fail(FAILURE_NO_TARGET, 5, 0);
                return false;
            };
            let Some(initial_health) = entities.regression_monster_health(target_source) else {
                fail(FAILURE_NO_TARGET, 5, u32::from(target_source));
                return false;
            };
            let Some(render_eye) = entities.regression_offset_eye(world, eye, target) else {
                fail(FAILURE_NO_TARGET, 5, 0x4000_0000 | u32::from(target_source));
                return false;
            };
            state.target_source = target_source;
            state.target_eye = eye;
            state.render_eye = render_eye;
            state.target_point = target;
            state.initial_health = initial_health;
            state.last_health = initial_health;
            state.rocket_health = initial_health;
            let probe = addr_of_mut!(PROBE);
            write_volatile(addr_of_mut!((*probe).monster_present), 1 << 5);
            write_volatile(addr_of_mut!((*probe).monster_state_bounds), 1 << 5);
            write_volatile(
                addr_of_mut!((*probe).failure_entity),
                u32::from(target_source),
            );
            write_volatile(addr_of_mut!((*probe).last_health), initial_health as u32);
        }

        let probe = addr_of_mut!(PROBE);
        let map_bit = 1 << map_number;
        write_volatile(
            addr_of_mut!((*probe).maps_loaded),
            read_volatile(addr_of_mut!((*probe).maps_loaded)) | map_bit,
        );
        write_volatile(
            addr_of_mut!((*probe).maps_validated),
            read_volatile(addr_of_mut!((*probe).maps_validated)) | map_bit,
        );
        write_volatile(
            addr_of_mut!((*probe).map_loads),
            read_volatile(addr_of_mut!((*probe).map_loads)).saturating_add(1),
        );
        write_volatile(addr_of_mut!((*probe).current_map), map_number);
        if pickup_count == 0 {
            write_volatile(addr_of_mut!((*probe).phase), PHASE_TRANSITION);
        } else {
            player.teleport(state.pickup_origins[0]);
            write_volatile(addr_of_mut!((*probe).phase), PHASE_PICKUP);
        }
        true
    }
}

pub fn fire_held(weapon: &WeaponState) -> bool {
    unsafe {
        let phase = read_volatile(addr_of_mut!(PROBE.phase));
        let expected = match phase {
            PHASE_NAIL_POOL => 0,
            PHASE_SHOTGUN => 0,
            PHASE_SUPER_SHOTGUN => 1,
            PHASE_NAILGUN => 2,
            PHASE_SUPER_NAILGUN => 3,
            PHASE_GRENADE => 4,
            PHASE_ROCKET => 5,
            _ => return false,
        };
        weapon.shots_fired() == expected
    }
}

pub fn prepare(
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut Player,
    weapon: &mut WeaponState,
) {
    unsafe {
        let phase = read_volatile(addr_of_mut!(PROBE.phase));
        let probe = addr_of_mut!(PROBE);
        let state = &mut *addr_of_mut!(STATE);
        if phase == PHASE_TRANSITION {
            let destination = match world.map() {
                EpisodeMap::E1M1 => EpisodeMap::E1M2,
                EpisodeMap::E1M2 => EpisodeMap::E1M3,
                EpisodeMap::E1M3 => EpisodeMap::E1M4,
                EpisodeMap::E1M4 => EpisodeMap::E1M5,
                _ => {
                    fail(FAILURE_BAD_MAP, map_number(world.map()), phase);
                    return;
                }
            };
            let Some(origin) = entities.regression_change_level_origin(destination) else {
                fail(
                    FAILURE_BAD_MAP,
                    map_number(world.map()),
                    0x7000_0000 | map_number(destination),
                );
                return;
            };
            player.teleport(origin);
            write_volatile(
                addr_of_mut!((*probe).transitions),
                read_volatile(addr_of_mut!((*probe).transitions)) | (1 << map_number(destination)),
            );
            write_volatile(addr_of_mut!((*probe).phase), PHASE_WAIT_MAP);
            return;
        }
        let animated = read_volatile(addr_of_mut!((*probe).weapon_animated));
        let fired = read_volatile(addr_of_mut!((*probe).weapon_fired));
        let projectiles = read_volatile(addr_of_mut!((*probe).shock_count));
        if phase == PHASE_PICKUP_DONE {
            if !weapon.select(Weapon::Nailgun)
                || !entities.regression_fill_nail_pool(world, state.render_eye)
            {
                fail(
                    FAILURE_NAIL_POOL,
                    5,
                    entities.regression_nail_count() as u32,
                );
                return;
            }
            // This labeled diagnostic launch uses the real standard fuse and
            // physics path from a clear point above the authored pickup. It
            // is not represented as an authored grenade-launcher shot.
            let grenade_origin = Vec3I32 {
                x: state.pickup_origins[0].x,
                y: state.pickup_origins[0].y,
                z: state.pickup_origins[0].z.saturating_add(64 << 12),
            };
            if !entities.spawn_grenade(
                world,
                quake_core::combat::GrenadeSpawn {
                    origin: grenade_origin,
                    velocity: Vec3I32::default(),
                    angles: quake_formats::Vec3I16::default(),
                    lifetime_ticks: quake_core::combat::GRENADE_LIFETIME_TICKS,
                    damage: quake_core::combat::GRENADE_DAMAGE,
                },
            ) {
                entities.regression_clear_nails();
                fail(FAILURE_NAIL_POOL, 5, 0x8000_0000);
                return;
            }
            state.nail_pool_ammo = weapon.inventory().ammo(AmmoKind::Nails);
            write_volatile(addr_of_mut!((*probe).phase), PHASE_NAIL_POOL);
            return;
        }
        let next = match phase {
            PHASE_NAIL_POOL_DONE => Some((Weapon::Shotgun, SHOTGUN_BIT, PHASE_SHOTGUN)),
            PHASE_SHOTGUN if animated & SHOTGUN_BIT != 0 && fired & SHOTGUN_BIT != 0 => {
                Some((Weapon::SuperShotgun, SUPER_SHOTGUN_BIT, PHASE_SUPER_SHOTGUN))
            }
            PHASE_SUPER_SHOTGUN
                if animated & SUPER_SHOTGUN_BIT != 0 && fired & SUPER_SHOTGUN_BIT != 0 =>
            {
                Some((Weapon::Nailgun, NAILGUN_BIT, PHASE_NAILGUN))
            }
            PHASE_NAILGUN
                if animated & NAILGUN_BIT != 0
                    && fired & NAILGUN_BIT != 0
                    && projectiles & NAILGUN_BIT != 0 =>
            {
                Some((Weapon::SuperNailgun, SUPER_NAILGUN_BIT, PHASE_SUPER_NAILGUN))
            }
            PHASE_SUPER_NAILGUN
                if animated & SUPER_NAILGUN_BIT != 0
                    && fired & SUPER_NAILGUN_BIT != 0
                    && projectiles & SUPER_NAILGUN_BIT != 0 =>
            {
                Some((Weapon::GrenadeLauncher, GRENADE_BIT, PHASE_GRENADE))
            }
            PHASE_GRENADE
                if animated & GRENADE_BIT != 0
                    && fired & GRENADE_BIT != 0
                    && projectiles & GRENADE_BIT != 0 =>
            {
                state.rocket_health = state.last_health;
                Some((Weapon::RocketLauncher, ROCKET_BIT, PHASE_ROCKET))
            }
            _ => None,
        };
        if let Some((next_weapon, bit, next_phase)) = next {
            if !weapon.select(next_weapon) {
                fail(FAILURE_PICKUP, 5, bit);
                return;
            }
            player.place_camera(
                state.target_eye,
                aim_angles(state.target_eye, state.target_point),
            );
            write_volatile(
                addr_of_mut!((*probe).weapon_selected),
                read_volatile(addr_of_mut!((*probe).weapon_selected)) | bit,
            );
            write_volatile(addr_of_mut!((*probe).phase), next_phase);
        } else if phase == PHASE_ROCKET && weapon.shots_fired() > 5 {
            player.place_camera(
                state.render_eye,
                aim_angles(state.render_eye, state.target_point),
            );
        }
    }
}

pub fn observe_pickup(pickup: PickupResult, player: &mut Player, weapon: &mut WeaponState) {
    unsafe {
        if read_volatile(addr_of_mut!(PROBE.phase)) != PHASE_PICKUP {
            return;
        }
        let state = &mut *addr_of_mut!(STATE);
        let index = state.pickup_index as usize;
        let expected_source = state.pickup_sources[index];
        // Items are touched with Quake's 15-unit `FL_ITEM` slack, so standing
        // on a weapon can consume neighbouring ammo in the same tick and the
        // last consumed source need not be the weapon; the inventory check
        // below proves the weapon itself was taken.
        if pickup.consumed == 0 {
            fail(FAILURE_PICKUP, 5, u32::from(expected_source));
            return;
        }
        let expected_weapon = state.pickup_weapons[index];
        let inventory = weapon.inventory();
        if !inventory.owns(expected_weapon) || inventory.active_weapon() != expected_weapon {
            fail(
                FAILURE_PICKUP,
                read_volatile(addr_of_mut!(PROBE.current_map)),
                0x5000_0000 | u32::from(state.pickup_classes[index]),
            );
            return;
        }
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).weapon_pickups),
            read_volatile(addr_of_mut!((*probe).weapon_pickups)) | state.pickup_bits[index],
        );
        state.pickup_index += 1;
        write_volatile(
            addr_of_mut!((*probe).route_index),
            u32::from(state.pickup_index),
        );
        if state.pickup_index == state.pickup_count {
            let next_phase = if state.pickup_bits[index] == ROCKET_BIT {
                PHASE_PICKUP_DONE
            } else {
                PHASE_TRANSITION
            };
            write_volatile(addr_of_mut!((*probe).phase), next_phase);
        } else {
            player.teleport(state.pickup_origins[state.pickup_index as usize]);
        }
    }
}

pub fn observe_combat(
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut Player,
    weapon: &mut WeaponState,
    rocket: RocketResult,
    nail: NailResult,
    grenade: GrenadeResult,
) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized {
            return;
        }
        let probe = addr_of_mut!(PROBE);
        let frames = read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1);
        write_volatile(addr_of_mut!((*probe).total_frames), frames);
        write_volatile(addr_of_mut!((*probe).stage_frames), frames);
        if read_volatile(addr_of_mut!((*probe).current_map)) != 5 {
            return;
        }
        let Some(health) = entities.regression_monster_health(state.target_source) else {
            fail(FAILURE_NO_TARGET, 5, u32::from(state.target_source));
            return;
        };
        state.last_health = health;
        write_volatile(
            addr_of_mut!((*probe).last_health),
            u32::from(health.max(0) as u16),
        );
        if health < state.initial_health {
            write_volatile(addr_of_mut!((*probe).monster_pain), 1 << 5);
        }
        let mut nail_flags = read_volatile(addr_of_mut!((*probe).monster_death));
        if nail.impacts != 0 {
            nail_flags |= 1;
        }
        if nail.damage.total_damage != 0 {
            nail_flags |= 1 << 1;
        }
        write_volatile(addr_of_mut!((*probe).monster_death), nail_flags);
        let mut grenade_flags = read_volatile(addr_of_mut!((*probe).monster_animated));
        if grenade.bounces != 0 {
            grenade_flags |= 1;
        }
        if grenade.rests != 0 {
            grenade_flags |= 1 << 1;
        }
        if grenade.explosions != 0 {
            grenade_flags |= 1 << 2;
        }
        if grenade.damage.total_damage != 0 {
            grenade_flags |= 1 << 3;
        }
        write_volatile(addr_of_mut!((*probe).monster_animated), grenade_flags);
        let phase = read_volatile(addr_of_mut!((*probe).phase));
        if phase == PHASE_NAIL_POOL {
            let nail_count = entities.regression_nail_count();
            let ammo = weapon.inventory().ammo(AmmoKind::Nails);
            if nail_count != NAIL_POOL_CAPACITY
                || weapon.shots_fired() != 0
                || ammo != state.nail_pool_ammo
                || weapon.view().frame != 0
            {
                fail(
                    FAILURE_NAIL_POOL,
                    5,
                    ((nail_count as u32) << 16) | u32::from(ammo),
                );
                return;
            }
            write_volatile(
                addr_of_mut!((*probe).boss),
                (1 << 16) | NAIL_POOL_CAPACITY as u32,
            );
            entities.regression_clear_nails();
            write_volatile(addr_of_mut!((*probe).phase), PHASE_NAIL_POOL_DONE);
            return;
        }
        let fired_bit = match (phase, weapon.shots_fired()) {
            (PHASE_SHOTGUN, count) if count >= 1 => SHOTGUN_BIT,
            (PHASE_SUPER_SHOTGUN, count) if count >= 2 => SUPER_SHOTGUN_BIT,
            (PHASE_NAILGUN, count) if count >= 3 => NAILGUN_BIT,
            (PHASE_SUPER_NAILGUN, count) if count >= 4 => SUPER_NAILGUN_BIT,
            (PHASE_GRENADE, count) if count >= 5 => GRENADE_BIT,
            (PHASE_ROCKET, count) if count >= 6 => ROCKET_BIT,
            _ => 0,
        };
        if fired_bit != 0 {
            write_volatile(
                addr_of_mut!((*probe).weapon_fired),
                read_volatile(addr_of_mut!((*probe).weapon_fired)) | fired_bit,
            );
        }
        if phase == PHASE_ROCKET
            && rocket.impacts != 0
            && health < state.rocket_health
            && rocket.self_damage != 0
        {
            write_volatile(
                addr_of_mut!((*probe).target_edges),
                u32::from(rocket.impacts),
            );
            write_volatile(
                addr_of_mut!((*probe).player_state),
                (u32::from(rocket.self_damage) << 16)
                    | u32::from(weapon.inventory().health().max(0) as u16),
            );
            write_volatile(addr_of_mut!((*probe).phase), PHASE_ROCKET_RENDER);
        }
        if read_volatile(addr_of_mut!((*probe).phase)) == PHASE_ROCKET_RENDER
            && read_volatile(addr_of_mut!((*probe).weapon_pickups)) == ALL_PICKUP_BITS
            && read_volatile(addr_of_mut!((*probe).weapon_selected)) == ALL_WEAPON_BITS
            && read_volatile(addr_of_mut!((*probe).weapon_fired)) == ALL_WEAPON_BITS
            && read_volatile(addr_of_mut!((*probe).weapon_animated)) == ALL_WEAPON_BITS
            && read_volatile(addr_of_mut!((*probe).shock_count)) == ALL_PROJECTILE_BITS
            && read_volatile(addr_of_mut!((*probe).maps_loaded)) == ROUTE_MAP_BITS
            && read_volatile(addr_of_mut!((*probe).maps_validated)) == ROUTE_MAP_BITS
            && read_volatile(addr_of_mut!((*probe).transitions)) == ROUTE_TRANSITION_BITS
            && read_volatile(addr_of_mut!((*probe).state_ranges))
                == quake_core::combat::ROCKET_MODEL_ID as u32
            && read_volatile(addr_of_mut!((*probe).valid_state_ranges)) != 0
            && read_volatile(addr_of_mut!((*probe).monster_death)) == 0x03
            && read_volatile(addr_of_mut!((*probe).monster_animated)) == 0x0f
            && read_volatile(addr_of_mut!((*probe).boss)) == ((1 << 16) | NAIL_POOL_CAPACITY as u32)
        {
            let Some(lightning_pickup) = quake_core::combat::pickup_for_entity(0x54, 0) else {
                fail(FAILURE_LIGHTNING_TRACE, 5, 0x5400_0000);
                return;
            };
            weapon.apply_pickup(lightning_pickup);
            if !weapon.select(Weapon::Lightning) {
                fail(FAILURE_LIGHTNING_TRACE, 5, 0x5400_0001);
                return;
            }
            // `W_FireLightning` starts the visible bolt at the player's
            // origin, 22 units below the eye. Keep the proof camera in the
            // real first-person position so it validates the silhouette the
            // player sees instead of looking straight down the beam axis.
            let beam_origin = Vec3I32 {
                x: state.render_eye.x,
                y: state.render_eye.y,
                z: state.render_eye.z.saturating_sub(22 << 12),
            };
            let Some((flags, forward)) =
                entities.regression_wall_lightning_probe(world, beam_origin)
            else {
                fail(FAILURE_LIGHTNING_TRACE, 5, 0);
                return;
            };
            let camera_eye = state.render_eye;
            let aim = Vec3I32 {
                x: state
                    .render_eye
                    .x
                    .saturating_add(forward.x.saturating_mul(600)),
                y: state
                    .render_eye
                    .y
                    .saturating_add(forward.y.saturating_mul(600)),
                z: state
                    .render_eye
                    .z
                    .saturating_add(forward.z.saturating_mul(600)),
            };
            player.place_camera(camera_eye, aim_angles(camera_eye, aim));
            write_volatile(addr_of_mut!((*probe).monster_attack), flags);
            write_volatile(addr_of_mut!((*probe).phase), PHASE_LIGHTNING_RENDER);
        }
        if read_volatile(addr_of_mut!((*probe).phase)) == PHASE_LIGHTNING_RENDER
            && read_volatile(addr_of_mut!((*probe).monster_attack)) == 0x0f
        {
            write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
            write_volatile(addr_of_mut!((*probe).complete), 1);
        }
        if frames > MAX_FRAMES && read_volatile(addr_of_mut!((*probe).complete)) == 0 {
            fail(FAILURE_TIMEOUT, 5, phase);
        }
    }
}

pub fn observe_render(stats: RenderStats, view: WeaponView) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if stats.explosion_effect_packets != 0 {
            write_volatile(
                addr_of_mut!((*probe).target_edges),
                read_volatile(addr_of_mut!((*probe).target_edges)) | EXPLOSION_PRESENTATION_BIT,
            );
        }
        let phase = read_volatile(addr_of_mut!((*probe).phase));
        if phase == PHASE_LIGHTNING_RENDER && stats.lightning_beam_packets != 0 {
            write_volatile(
                addr_of_mut!((*probe).monster_attack),
                read_volatile(addr_of_mut!((*probe).monster_attack)) | (1 << 3),
            );
        }
        let mut projectile_bits = 0u32;
        if stats.nail_projectile_packets != 0 {
            projectile_bits |= match phase {
                PHASE_NAILGUN => NAILGUN_BIT,
                PHASE_SUPER_NAILGUN => SUPER_NAILGUN_BIT,
                _ => 0,
            };
        }
        if stats.grenade_projectile_packets != 0 {
            projectile_bits |= GRENADE_BIT;
        }
        if stats.rocket_projectile_packets != 0 {
            projectile_bits |= ROCKET_BIT;
            write_volatile(
                addr_of_mut!((*probe).state_ranges),
                quake_core::combat::ROCKET_MODEL_ID as u32,
            );
            write_volatile(
                addr_of_mut!((*probe).valid_state_ranges),
                read_volatile(addr_of_mut!((*probe).valid_state_ranges))
                    .saturating_add(stats.rocket_projectile_packets),
            );
        }
        if projectile_bits != 0 {
            write_volatile(
                addr_of_mut!((*probe).shock_count),
                read_volatile(addr_of_mut!((*probe).shock_count)) | projectile_bits,
            );
        }
        write_volatile(
            addr_of_mut!((*probe).intermission_state),
            read_volatile(addr_of_mut!((*probe).intermission_state))
                .saturating_add(stats.projectile_packets),
        );
    }
    if view.frame == 0 || stats.view_model_packets == 0 {
        return;
    }
    let bit = match view.model_id {
        quake_core::combat::SHOTGUN_MODEL_ID => SHOTGUN_BIT,
        quake_core::combat::SUPER_SHOTGUN_MODEL_ID => SUPER_SHOTGUN_BIT,
        quake_core::combat::NAILGUN_MODEL_ID => NAILGUN_BIT,
        quake_core::combat::SUPER_NAILGUN_MODEL_ID => SUPER_NAILGUN_BIT,
        quake_core::combat::GRENADE_LAUNCHER_MODEL_ID => GRENADE_BIT,
        quake_core::combat::ROCKET_LAUNCHER_MODEL_ID => ROCKET_BIT,
        _ => return,
    };
    unsafe {
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).weapon_animated),
            read_volatile(addr_of_mut!((*probe).weapon_animated)) | bit,
        );
    }
}

const fn map_number(map: EpisodeMap) -> u32 {
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

fn aim_angles(from: Vec3I32, to: Vec3I32) -> [i16; 3] {
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
