//! Read-only state block for the host journey driver.
//!
//! The block is published once per frame at the top of the game loop, before the
//! frame's pad poll, and `seq` is written last. A host that watches `seq` can read
//! a complete snapshot and choose the pad sample the guest is about to poll. The
//! guest never reads the block back, so it cannot change play.

use core::ptr::{addr_of_mut, write_volatile};

pub const MAX_MONSTERS: usize = 48;
pub const MAX_PICKUPS: usize = 64;
pub const MAX_MOVERS: usize = 64;
pub const MAX_PROPS: usize = 24;
const MAGIC: u32 = 0x4e52_4a51; // "QJRN"
const VERSION: u32 = 1;

/// Mode bits.
pub const MODE_MENU: u32 = 1;
pub const MODE_INTERMISSION: u32 = 2;
pub const MODE_DEAD: u32 = 4;

/// One monster: `[source_index, kind << 16 | state << 8 | flags, health, x, y, z]`.
pub type MonsterRow = [i32; 6];
/// One pickup: `[source_index, class, x, y, z]`.
pub type PickupRow = [i32; 5];
/// One mover: `[source_index, state, class, x, y, z]`.
pub type MoverRow = [i32; 6];
/// One damageable non-monster (barrels): `[source_index, health, x, y, z]`.
pub type PropRow = [i32; 5];

#[repr(C)]
pub struct State {
    pub magic: u32,
    pub version: u32,
    pub seq: u32,
    pub frame: u32,
    pub map: u32,
    pub mode: u32,
    pub skill: u32,
    pub runes: u32,
    pub origin: [i32; 3],
    pub velocity: [i32; 3],
    pub angles: [i32; 3],
    pub grounded: u32,
    pub water: u32,
    pub health: i32,
    pub armor: u32,
    pub armor_tier: u32,
    pub ammo: [u32; 4],
    pub weapons: u32,
    pub weapon: u32,
    pub keys: u32,
    pub powerups: u32,
    pub kills: u32,
    pub kills_total: u32,
    pub secrets: u32,
    pub secrets_total: u32,
    pub intermission_elapsed: u32,
    pub monster_count: u32,
    pub pickup_count: u32,
    pub mover_count: u32,
    pub monsters: [MonsterRow; MAX_MONSTERS],
    pub pickups: [PickupRow; MAX_PICKUPS],
    pub movers: [MoverRow; MAX_MOVERS],
    pub prop_count: u32,
    pub props: [PropRow; MAX_PROPS],
}

#[no_mangle]
pub static mut JOURNEY_STATE: State = State {
    magic: MAGIC,
    version: VERSION,
    seq: 0,
    frame: 0,
    map: 0,
    mode: 0,
    skill: 0,
    runes: 0,
    origin: [0; 3],
    velocity: [0; 3],
    angles: [0; 3],
    grounded: 0,
    water: 0,
    health: 0,
    armor: 0,
    armor_tier: 0,
    ammo: [0; 4],
    weapons: 0,
    weapon: 0,
    keys: 0,
    powerups: 0,
    kills: 0,
    kills_total: 0,
    secrets: 0,
    secrets_total: 0,
    intermission_elapsed: 0,
    monster_count: 0,
    pickup_count: 0,
    mover_count: 0,
    monsters: [[0; 6]; MAX_MONSTERS],
    pickups: [[0; 5]; MAX_PICKUPS],
    movers: [[0; 6]; MAX_MOVERS],
    prop_count: 0,
    props: [[0; 5]; MAX_PROPS],
};

/// Everything the loop knows that is not in the scene.
pub struct Frame<'a> {
    pub frame: u32,
    pub map: u32,
    pub mode: u32,
    pub intermission_elapsed: u32,
    pub player: &'a crate::player::Player,
    pub weapon: &'a quake_core::combat::WeaponState,
    pub entities: &'a crate::entity::EntityScene,
}

#[inline(never)]
pub fn publish(frame: &Frame<'_>) {
    // SAFETY: single-threaded guest loop; the host only reads this block.
    unsafe {
        let s = addr_of_mut!(JOURNEY_STATE);
        let w = |p: *mut u32, v: u32| write_volatile(p, v);
        let wi = |p: *mut i32, v: i32| write_volatile(p, v);
        w(addr_of_mut!((*s).frame), frame.frame);
        w(addr_of_mut!((*s).map), frame.map);
        w(addr_of_mut!((*s).mode), frame.mode);
        w(
            addr_of_mut!((*s).intermission_elapsed),
            frame.intermission_elapsed,
        );
        let o = frame.player.origin();
        let v = frame.player.velocity();
        for (i, c) in [o.x, o.y, o.z].into_iter().enumerate() {
            wi(addr_of_mut!((*s).origin[i]), c);
        }
        for (i, c) in [v.x, v.y, v.z].into_iter().enumerate() {
            wi(addr_of_mut!((*s).velocity[i]), c);
        }
        for i in 0..3 {
            wi(
                addr_of_mut!((*s).angles[i]),
                i32::from(frame.player.view_angles[i]),
            );
        }
        w(addr_of_mut!((*s).grounded), u32::from(frame.player.journey_grounded()));
        w(addr_of_mut!((*s).water), u32::from(frame.player.water_level()));
        let inv = frame.weapon.inventory();
        wi(addr_of_mut!((*s).health), i32::from(inv.health()));
        w(addr_of_mut!((*s).armor), u32::from(inv.armor()));
        w(
            addr_of_mut!((*s).armor_tier),
            match inv.armor_tier() {
                None => 0,
                Some(t) => 1 + t as u32,
            },
        );
        use quake_core::combat::AmmoKind;
        for (i, k) in [
            AmmoKind::Shells,
            AmmoKind::Nails,
            AmmoKind::Rockets,
            AmmoKind::Cells,
        ]
        .into_iter()
        .enumerate()
        {
            w(addr_of_mut!((*s).ammo[i]), u32::from(inv.ammo(k)));
        }
        w(addr_of_mut!((*s).weapons), u32::from(inv.owned_weapons()));
        w(addr_of_mut!((*s).weapon), inv.active_weapon() as u32);
        w(addr_of_mut!((*s).keys), u32::from(inv.keys()));
        use quake_core::survival::PowerupKind;
        let p = inv.powerups();
        let mut bits = 0u32;
        for (i, k) in [
            PowerupKind::Pentagram,
            PowerupKind::Ring,
            PowerupKind::Quad,
            PowerupKind::Biosuit,
        ]
        .into_iter()
        .enumerate()
        {
            bits |= u32::from(p.active(k)) << i;
        }
        w(addr_of_mut!((*s).powerups), bits);
        let (k, kt) = frame.entities.kills();
        let (sc, st) = frame.entities.secrets();
        w(addr_of_mut!((*s).kills), u32::from(k));
        w(addr_of_mut!((*s).kills_total), u32::from(kt));
        w(addr_of_mut!((*s).secrets), u32::from(sc));
        w(addr_of_mut!((*s).secrets_total), u32::from(st));
        w(addr_of_mut!((*s).skill), u32::from(frame.entities.skill()));
        w(addr_of_mut!((*s).runes), u32::from(frame.entities.runes()));
        let (mc, pc, vc, pr) = frame.entities.journey_fill(
            &mut *addr_of_mut!((*s).monsters),
            &mut *addr_of_mut!((*s).pickups),
            &mut *addr_of_mut!((*s).movers),
            &mut *addr_of_mut!((*s).props),
        );
        w(addr_of_mut!((*s).prop_count), pr);
        w(addr_of_mut!((*s).monster_count), mc);
        w(addr_of_mut!((*s).pickup_count), pc);
        w(addr_of_mut!((*s).mover_count), vc);
        // Last: a host that sees a new `seq` sees a whole snapshot.
        let seq = core::ptr::read_volatile(addr_of_mut!((*s).seq));
        w(addr_of_mut!((*s).seq), seq.wrapping_add(1));
    }
}
