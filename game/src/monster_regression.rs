//! Image-free runtime regression for authored Easy-mode E1M1 monsters.

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use psx_math::atan2_q12;
use psx_math::int32::{isqrt_i32, square_i32_saturating};
use quake_core::combat::WeaponState;
use quake_core::monster::{MonsterState, CLASS_ARMY, CLASS_DOG, SOLDIER_HEAD_MODEL_ID};
use quake_formats::Vec3I32;

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::{EntityScene, MonsterFrameResult};
use crate::player::Player;

const PROBE_MAGIC: u32 = 0x5150_5358;
const PROBE_VERSION: u32 = 6;
const PHASE_DOG: u32 = 0x71;
const PHASE_SOLDIER: u32 = 0x72;
const PHASE_DAMAGE_STATES: u32 = 0x73;
const PHASE_COMPLETE: u32 = 0x80;
const PHASE_ERROR: u32 = 0xff;
const FAILURE_BAD_MAP: u32 = 1;
const FAILURE_BAD_SOURCE: u32 = 2;
const FAILURE_NO_CLEAR_POSITION: u32 = 3;
const FAILURE_DOG_TIMEOUT: u32 = 4;
const FAILURE_SOLDIER_TIMEOUT: u32 = 5;
const FAILURE_STATE_TIMEOUT: u32 = 6;

const SOLDIER_SOURCE: u16 = 21;
const DOG_SOURCE: u16 = 82;
const PAIN_SOURCE: u16 = 115;
const DEATH_SOURCE: u16 = 122;
const GIB_SOURCE: u16 = 124;

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
    phase: u32,
    phase_frames: u32,
    dog_eye: Vec3I32,
    soldier_eye: Vec3I32,
    dog_origin: Vec3I32,
    soldier_origin: Vec3I32,
    damage_applied: bool,
    pain_seen: bool,
    death_seen: bool,
    gib_seen: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            initialized: false,
            phase: 0,
            phase_frames: 0,
            dog_eye: Vec3I32 { x: 0, y: 0, z: 0 },
            soldier_eye: Vec3I32 { x: 0, y: 0, z: 0 },
            dog_origin: Vec3I32 { x: 0, y: 0, z: 0 },
            soldier_origin: Vec3I32 { x: 0, y: 0, z: 0 },
            damage_applied: false,
            pain_seen: false,
            death_seen: false,
            gib_seen: false,
        }
    }
}

static mut STATE: State = State::new();

pub fn setup(world: &ResidentMap, entities: &EntityScene, player: &mut Player) -> bool {
    unsafe {
        if world.map() != EpisodeMap::E1M1 {
            fail(FAILURE_BAD_MAP, 0, 0);
            return false;
        }
        for (source, class_name, origin) in [
            (SOLDIER_SOURCE, CLASS_ARMY, [248, 2_392, 40]),
            (DOG_SOURCE, CLASS_DOG, [88, 1_520, -200]),
        ] {
            let Some(entity) = world.entities().get(source as usize) else {
                fail(FAILURE_BAD_SOURCE, source, 0);
                return false;
            };
            let actual = [
                entity.origin.x >> 12,
                entity.origin.y >> 12,
                entity.origin.z >> 12,
            ];
            if entity.class_name != class_name
                || entity.spawn_flags & 0x0100 != 0
                || actual != origin
            {
                fail(FAILURE_BAD_SOURCE, source, u32::from(entity.class_name));
                return false;
            }
        }
        let Some(dog_eye) = entities.regression_monster_player_eye(world, DOG_SOURCE, 80) else {
            fail(FAILURE_NO_CLEAR_POSITION, DOG_SOURCE, 80);
            return false;
        };
        let Some(soldier_eye) = entities.regression_monster_player_eye(world, SOLDIER_SOURCE, 80)
        else {
            fail(FAILURE_NO_CLEAR_POSITION, SOLDIER_SOURCE, 80);
            return false;
        };
        let Some(dog) = entities.regression_monster_snapshot(DOG_SOURCE) else {
            fail(FAILURE_BAD_SOURCE, DOG_SOURCE, 1);
            return false;
        };
        let Some(soldier) = entities.regression_monster_snapshot(SOLDIER_SOURCE) else {
            fail(FAILURE_BAD_SOURCE, SOLDIER_SOURCE, 1);
            return false;
        };
        player.place_camera(dog_eye, aim_angles(dog_eye, dog.origin));
        let state = &mut *addr_of_mut!(STATE);
        state.initialized = true;
        state.phase = PHASE_DOG;
        state.dog_eye = dog_eye;
        state.soldier_eye = soldier_eye;
        state.dog_origin = dog.origin;
        state.soldier_origin = soldier.origin;

        let probe = addr_of_mut!(PROBE);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_DOG);
        write_volatile(addr_of_mut!((*probe).maps_loaded), 1 << 1);
        write_volatile(addr_of_mut!((*probe).maps_validated), 1 << 1);
        write_volatile(addr_of_mut!((*probe).monster_present), 0x03);
        write_volatile(addr_of_mut!((*probe).monster_state_bounds), 0x03);
        write_volatile(addr_of_mut!((*probe).current_map), 1);
        write_volatile(addr_of_mut!((*probe).map_loads), 1);
        write_volatile(addr_of_mut!((*probe).failure_entity), u32::from(DOG_SOURCE));
        true
    }
}

pub fn prepare(
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut Player,
    _weapon: &mut WeaponState,
) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized {
            return;
        }
        match state.phase {
            PHASE_DOG => {
                if let Some(dog) = entities.regression_monster_snapshot(DOG_SOURCE) {
                    player.place_camera(state.dog_eye, aim_angles(state.dog_eye, dog.origin));
                }
            }
            PHASE_SOLDIER => {
                if let Some(soldier) = entities.regression_monster_snapshot(SOLDIER_SOURCE) {
                    player.place_camera(
                        state.soldier_eye,
                        aim_angles(state.soldier_eye, soldier.origin),
                    );
                }
            }
            PHASE_DAMAGE_STATES if !state.damage_applied => {
                let pain = entities.regression_damage_monster(world, PAIN_SOURCE, 4);
                let death = entities.regression_damage_monster(world, DEATH_SOURCE, 30);
                let gib = entities.regression_damage_monster(world, GIB_SOURCE, 70);
                if pain.is_none() || death.is_none() || gib.is_none() {
                    fail(FAILURE_BAD_SOURCE, PAIN_SOURCE, 2);
                    return;
                }
                state.damage_applied = true;
            }
            _ => {}
        }
    }
}

pub fn observe(entities: &EntityScene, weapon: &WeaponState, frame: MonsterFrameResult) {
    unsafe {
        let state = &mut *addr_of_mut!(STATE);
        if !state.initialized || read_volatile(addr_of_mut!(PROBE.complete)) != 0 {
            return;
        }
        state.phase_frames = state.phase_frames.wrapping_add(1);
        let probe = addr_of_mut!(PROBE);
        write_volatile(
            addr_of_mut!((*probe).total_frames),
            read_volatile(addr_of_mut!((*probe).total_frames)).wrapping_add(1),
        );
        write_volatile(addr_of_mut!((*probe).stage_frames), state.phase_frames);
        write_volatile(
            addr_of_mut!((*probe).last_health),
            u32::from(weapon.inventory().health().max(0) as u16),
        );

        match state.phase {
            PHASE_DOG => {
                let Some(dog) = entities.regression_monster_snapshot(DOG_SOURCE) else {
                    fail(FAILURE_BAD_SOURCE, DOG_SOURCE, 3);
                    return;
                };
                if dog.active {
                    write_volatile(
                        addr_of_mut!((*probe).target_edges),
                        read_volatile(addr_of_mut!((*probe).target_edges)) | 1,
                    );
                }
                if dog.origin != state.dog_origin {
                    write_volatile(
                        addr_of_mut!((*probe).monster_animated),
                        read_volatile(addr_of_mut!((*probe).monster_animated)) | 1,
                    );
                }
                if frame.attacks != 0 && frame.player_damage != 0 {
                    write_volatile(
                        addr_of_mut!((*probe).monster_attack),
                        read_volatile(addr_of_mut!((*probe).monster_attack)) | 1,
                    );
                    transition(state, probe, PHASE_SOLDIER, SOLDIER_SOURCE);
                } else if state.phase_frames > 360 {
                    fail(
                        FAILURE_DOG_TIMEOUT,
                        DOG_SOURCE,
                        weapon.inventory().health() as u32,
                    );
                }
            }
            PHASE_SOLDIER => {
                let Some(soldier) = entities.regression_monster_snapshot(SOLDIER_SOURCE) else {
                    fail(FAILURE_BAD_SOURCE, SOLDIER_SOURCE, 3);
                    return;
                };
                if soldier.active {
                    write_volatile(
                        addr_of_mut!((*probe).target_edges),
                        read_volatile(addr_of_mut!((*probe).target_edges)) | 2,
                    );
                }
                if soldier.origin != state.soldier_origin {
                    write_volatile(
                        addr_of_mut!((*probe).monster_animated),
                        read_volatile(addr_of_mut!((*probe).monster_animated)) | 2,
                    );
                }
                if frame.attacks != 0 && frame.player_damage != 0 {
                    write_volatile(
                        addr_of_mut!((*probe).monster_attack),
                        read_volatile(addr_of_mut!((*probe).monster_attack)) | 2,
                    );
                }
                if frame.player_killed && weapon.inventory().health() <= 0 {
                    write_volatile(addr_of_mut!((*probe).player_state), 1);
                    transition(state, probe, PHASE_DAMAGE_STATES, PAIN_SOURCE);
                } else if state.phase_frames > 1_800 {
                    fail(
                        FAILURE_SOLDIER_TIMEOUT,
                        SOLDIER_SOURCE,
                        weapon.inventory().health().max(0) as u32,
                    );
                }
            }
            PHASE_DAMAGE_STATES => {
                let Some(pain) = entities.regression_monster_snapshot(PAIN_SOURCE) else {
                    fail(FAILURE_BAD_SOURCE, PAIN_SOURCE, 4);
                    return;
                };
                let Some(death) = entities.regression_monster_snapshot(DEATH_SOURCE) else {
                    fail(FAILURE_BAD_SOURCE, DEATH_SOURCE, 4);
                    return;
                };
                let Some(gib) = entities.regression_monster_snapshot(GIB_SOURCE) else {
                    fail(FAILURE_BAD_SOURCE, GIB_SOURCE, 4);
                    return;
                };
                state.pain_seen |= matches!(
                    pain.state,
                    MonsterState::PainA | MonsterState::PainB | MonsterState::PainC
                );
                state.death_seen |=
                    matches!(death.state, MonsterState::DeathA | MonsterState::DeathB);
                state.gib_seen |=
                    gib.state == MonsterState::Gib && gib.model_id == SOLDIER_HEAD_MODEL_ID;
                if state.pain_seen {
                    write_volatile(addr_of_mut!((*probe).monster_pain), 1);
                }
                if state.death_seen {
                    write_volatile(addr_of_mut!((*probe).monster_death), 1);
                }
                if state.gib_seen {
                    write_volatile(addr_of_mut!((*probe).boss), 1);
                }
                let pain_progressed = state.pain_seen
                    && !matches!(
                        pain.state,
                        MonsterState::PainA | MonsterState::PainB | MonsterState::PainC
                    );
                if pain_progressed && death.corpse_finished && state.gib_seen {
                    write_volatile(addr_of_mut!((*probe).state_ranges), 3);
                    write_volatile(addr_of_mut!((*probe).valid_state_ranges), 3);
                    write_volatile(addr_of_mut!((*probe).phase), PHASE_COMPLETE);
                    write_volatile(addr_of_mut!((*probe).complete), 1);
                } else if state.phase_frames > 240 {
                    fail(FAILURE_STATE_TIMEOUT, DEATH_SOURCE, u32::from(death.frame));
                }
            }
            _ => {}
        }
    }
}

unsafe fn transition(state: &mut State, probe: *mut Probe, phase: u32, entity: u16) {
    state.phase = phase;
    state.phase_frames = 0;
    unsafe {
        write_volatile(addr_of_mut!((*probe).phase), phase);
        write_volatile(addr_of_mut!((*probe).failure_entity), u32::from(entity));
        write_volatile(
            addr_of_mut!((*probe).route_index),
            read_volatile(addr_of_mut!((*probe).route_index)).wrapping_add(1),
        );
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

unsafe fn fail(code: u32, entity: u16, detail: u32) {
    unsafe {
        let probe = addr_of_mut!(PROBE);
        if read_volatile(addr_of_mut!((*probe).failure_code)) != 0 {
            return;
        }
        write_volatile(addr_of_mut!((*probe).failure_code), code);
        write_volatile(addr_of_mut!((*probe).failure_map), 1);
        write_volatile(addr_of_mut!((*probe).failure_entity), u32::from(entity));
        write_volatile(addr_of_mut!((*probe).failure_detail), detail);
        write_volatile(addr_of_mut!((*probe).phase), PHASE_ERROR);
    }
}
