//! Platform-neutral Quake inventory, weapon, and hitbox arithmetic.
//!
//! The guest owns world/entity tracing, but the weapon clock and every ray are
//! deterministic fixed-point policy.  This keeps host tests useful while the
//! PlayStation runtime remains allocation-free.

use psx_engine::div_q12_i32;
use psx_math::int32::mul_q12_i32;
use psx_math::{cos_q12, sin_q12};
use quake_formats::{Vec3I16, Vec3I32};

use crate::survival::{
    PowerupKind, Powerups, Survival, SurvivalFrame, SurvivalInput, MEGAHEALTH_ROT_DELAY_TICKS,
    MEGAHEALTH_ROT_INTERVAL_TICKS, PLAYER_MAX_HEALTH, QUAD_DAMAGE_MULTIPLIER,
};

pub const SHOTGUN_MODEL_ID: i16 = 0x54;
pub const AXE_MODEL_ID: i16 = 0x4e;
pub const SUPER_SHOTGUN_MODEL_ID: i16 = 0x55;
pub const NAILGUN_MODEL_ID: i16 = 0x50;
pub const SUPER_NAILGUN_MODEL_ID: i16 = 0x51;
pub const GRENADE_LAUNCHER_MODEL_ID: i16 = 0x52;
pub const ROCKET_LAUNCHER_MODEL_ID: i16 = 0x53;
pub const LIGHTNING_MODEL_ID: i16 = 0x4f;
pub const GRENADE_MODEL_ID: i16 = 0x2a;
pub const ROCKET_MODEL_ID: i16 = 0x40;
pub const NAIL_MODEL_ID: i16 = 0x4a;
pub const LIGHTNING_BOLT_MODEL_ID: i16 = 0x13;
pub const SHOTGUN_PELLETS: usize = 6;
pub const SUPER_SHOTGUN_PELLETS: usize = 14;
pub const MAX_SHOTGUN_PELLETS: usize = SUPER_SHOTGUN_PELLETS;
pub const SHOTGUN_DAMAGE_PER_PELLET: i16 = 4;
pub const SHOTGUN_RANGE_UNITS: i32 = 2_048;
pub const SHOTGUN_REFIRE_TICKS: u16 = 30;
pub const SUPER_SHOTGUN_REFIRE_TICKS: u16 = 42;
pub const SHOTGUN_STARTING_SHELLS: u16 = 25;
/// `SetChangeParms` floors a survivor's health at half before the next level.
pub const CHANGE_LEVEL_MIN_HEALTH: i16 = 50;
pub const AXE_DAMAGE: i16 = 20;
pub const AXE_RANGE_UNITS: i32 = 64;
pub const AXE_REFIRE_TICKS: u16 = 30;
pub const ROCKET_DIRECT_DAMAGE: i16 = 100;
pub const ROCKET_SPLASH_DAMAGE: i16 = 120;
pub const ROCKET_REFIRE_TICKS: u16 = 48;
pub const ROCKET_LIFETIME_TICKS: u16 = 300;
pub const ROCKET_STEP_Q12: i32 = 68_267; // 1000 units/second at 60 Hz.
pub const NAIL_REFIRE_TICKS: u16 = 6;
pub const NAIL_LIFETIME_TICKS: u16 = 360;
pub const NAIL_POOL_CAPACITY: usize =
    (NAIL_LIFETIME_TICKS as usize + NAIL_REFIRE_TICKS as usize - 1) / NAIL_REFIRE_TICKS as usize;
pub const NAIL_DAMAGE: i16 = 9;
pub const SUPER_NAIL_DAMAGE: i16 = 18;
pub const GRENADE_REFIRE_TICKS: u16 = 36;
pub const GRENADE_LIFETIME_TICKS: u16 = 150;
pub const GRENADE_DAMAGE: i16 = 120;
pub const GRENADE_FLOOR_NORMAL_Z_Q12: i16 = 2_867;
pub const GRENADE_REST_Z_STEP_Q12: i32 = 60 * Q12_ONE / 60;
pub const LIGHTNING_REFIRE_TICKS: u16 = 6;
pub const LIGHTNING_RANGE_UNITS: i32 = 600;
pub const LIGHTNING_DAMAGE: i16 = 30;
const SHOTGUN_SPREAD_Q12: i32 = 163; // Quake FTOX(0.04).
const SUPER_SHOTGUN_HORIZONTAL_SPREAD_Q12: i32 = 573; // Quake FTOX(0.14).
const SUPER_SHOTGUN_VERTICAL_SPREAD_Q12: i32 = 327; // Quake FTOX(0.08).
const VIEW_MODEL_FRAME_TICKS: u8 = 6; // Quake's 10 Hz game-code cadence.
const Q12_ONE: i32 = 1 << 12;
const INITIAL_RANDOM_STATE: u32 = 0x51f1_5e1d;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProjectileKind {
    Nail,
    Grenade,
    Rocket,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SkyImpact {
    RemoveSilently,
    Impact,
    Bounce,
}

pub const fn projectile_sky_impact(kind: ProjectileKind) -> SkyImpact {
    match kind {
        // The preserved PSX port leaves the original spike sky TODO in place.
        ProjectileKind::Nail => SkyImpact::Impact,
        ProjectileKind::Grenade => SkyImpact::Bounce,
        ProjectileKind::Rocket => SkyImpact::RemoveSilently,
    }
}

pub const fn projectile_expires_this_tick(remaining_ticks: &mut u16) -> bool {
    if *remaining_ticks == 0 {
        return true;
    }
    *remaining_ticks -= 1;
    *remaining_ticks == 0
}

pub const fn grenade_should_rest(velocity: Vec3I32, normal_z: i16) -> bool {
    normal_z > GRENADE_FLOOR_NORMAL_Z_Q12 && velocity.z < GRENADE_REST_Z_STEP_Q12
}

pub fn settle_grenade_motion(
    velocity: &mut Vec3I32,
    angular_velocity: &mut Vec3I16,
    normal_z: i16,
) -> bool {
    if !grenade_should_rest(*velocity, normal_z) {
        return false;
    }
    *velocity = Vec3I32::default();
    *angular_velocity = Vec3I16::default();
    true
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GrenadeTick {
    Explode,
    Rest,
    Move,
}

pub const fn grenade_tick(remaining_ticks: &mut u16, resting: bool) -> GrenadeTick {
    if projectile_expires_this_tick(remaining_ticks) {
        GrenadeTick::Explode
    } else if resting {
        GrenadeTick::Rest
    } else {
        GrenadeTick::Move
    }
}

pub fn rocket_splash_points(distance_units: i32, self_damage: bool, visible: bool) -> i16 {
    explosion_splash_points(
        ROCKET_SPLASH_DAMAGE,
        distance_units,
        self_damage,
        visible,
        false,
    )
}

pub fn explosion_splash_points(
    damage: i16,
    distance_units: i32,
    self_damage: bool,
    visible: bool,
    target_is_shambler: bool,
) -> i16 {
    if !visible {
        return 0;
    }
    // `T_RadiusDamage`: `points = damage - 0.5 * vlen(inflictor - center)`.
    let half_distance = (distance_units.max(0) / 2).min(i16::MAX as i32) as i16;
    let mut points = damage.saturating_sub(half_distance).max(0);
    if self_damage {
        points /= 2;
    }
    if target_is_shambler {
        points /= 2;
    }
    points
}

pub const fn rocket_direct_points(damage: i16, target_is_shambler: bool) -> i16 {
    if target_is_shambler {
        damage / 2
    } else {
        damage
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExplosionKind {
    Rocket,
    Grenade,
}

impl ExplosionKind {
    pub const fn radius_ignores_direct_target(self) -> bool {
        matches!(self, Self::Rocket)
    }
}

pub const fn rocket_elapsed_ticks(elapsed_ticks: u16) -> u16 {
    if elapsed_ticks > 4 {
        4
    } else {
        elapsed_ticks
    }
}

const WEAPON_AXE_BIT: u8 = 1 << 0;
const WEAPON_SHOTGUN_BIT: u8 = 1 << 1;
const WEAPON_SUPER_SHOTGUN_BIT: u8 = 1 << 2;
const WEAPON_NAILGUN_BIT: u8 = 1 << 3;
const WEAPON_SUPER_NAILGUN_BIT: u8 = 1 << 4;
const WEAPON_GRENADE_LAUNCHER_BIT: u8 = 1 << 5;
const WEAPON_ROCKET_LAUNCHER_BIT: u8 = 1 << 6;
const WEAPON_LIGHTNING_BIT: u8 = 1 << 7;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AmmoKind {
    Shells,
    Nails,
    Rockets,
    Cells,
}

impl AmmoKind {
    pub const fn index(self) -> usize {
        match self {
            Self::Shells => 0,
            Self::Nails => 1,
            Self::Rockets => 2,
            Self::Cells => 3,
        }
    }

    const fn maximum(self) -> u16 {
        match self {
            Self::Nails => 200,
            Self::Shells | Self::Rockets | Self::Cells => 100,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Weapon {
    Axe,
    Shotgun,
    SuperShotgun,
    Nailgun,
    SuperNailgun,
    GrenadeLauncher,
    RocketLauncher,
    Lightning,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AttackAdmission {
    pub nail: bool,
    pub grenade: bool,
    pub rocket: bool,
}

impl AttackAdmission {
    pub const ALL: Self = Self {
        nail: true,
        grenade: true,
        rocket: true,
    };

    pub const fn allows(self, weapon: Weapon) -> bool {
        match weapon {
            Weapon::Nailgun | Weapon::SuperNailgun => self.nail,
            Weapon::GrenadeLauncher => self.grenade,
            Weapon::RocketLauncher => self.rocket,
            _ => true,
        }
    }
}

impl Weapon {
    /// Original Quake `impulse 1` through `impulse 8` weapon order.
    #[optimize(size)]
    pub const fn from_impulse(impulse: u8) -> Option<Self> {
        Some(match impulse {
            1 => Self::Axe,
            2 => Self::Shotgun,
            3 => Self::SuperShotgun,
            4 => Self::Nailgun,
            5 => Self::SuperNailgun,
            6 => Self::GrenadeLauncher,
            7 => Self::RocketLauncher,
            8 => Self::Lightning,
            _ => return None,
        })
    }

    /// Whether the original fires this weapon along `aim(self, ...)`:
    /// the shotguns, nailguns and rocket launcher always, the grenade
    /// launcher only at a level pitch (`W_FireGrenade`). The axe and the
    /// lightning gun shoot straight down `v_forward`.
    pub const fn auto_aims(self, pitch: i16) -> bool {
        match self {
            Self::Shotgun
            | Self::SuperShotgun
            | Self::Nailgun
            | Self::SuperNailgun
            | Self::RocketLauncher => true,
            Self::GrenadeLauncher => pitch == 0,
            Self::Axe | Self::Lightning => false,
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Axe => WEAPON_AXE_BIT,
            Self::Shotgun => WEAPON_SHOTGUN_BIT,
            Self::SuperShotgun => WEAPON_SUPER_SHOTGUN_BIT,
            Self::Nailgun => WEAPON_NAILGUN_BIT,
            Self::SuperNailgun => WEAPON_SUPER_NAILGUN_BIT,
            Self::GrenadeLauncher => WEAPON_GRENADE_LAUNCHER_BIT,
            Self::RocketLauncher => WEAPON_ROCKET_LAUNCHER_BIT,
            Self::Lightning => WEAPON_LIGHTNING_BIT,
        }
    }

    /// `weapon_touch`'s line for this weapon's id1 `netname`. The axe and the
    /// single shotgun have no `weapon_*` entity to be picked up from, so the
    /// original never gives them a netname either.
    const fn pickup_netname(self) -> Option<&'static str> {
        Some(match self {
            Self::Axe | Self::Shotgun => return None,
            Self::SuperShotgun => "You got the Double-barrelled Shotgun",
            Self::Nailgun => "You got the nailgun",
            Self::SuperNailgun => "You got the Super Nailgun",
            Self::GrenadeLauncher => "You got the Grenade Launcher",
            Self::RocketLauncher => "You got the Rocket Launcher",
            Self::Lightning => "You got the Thunderbolt",
        })
    }

    pub const fn model_id(self) -> i16 {
        match self {
            Self::Axe => AXE_MODEL_ID,
            Self::Shotgun => SHOTGUN_MODEL_ID,
            Self::SuperShotgun => SUPER_SHOTGUN_MODEL_ID,
            Self::Nailgun => NAILGUN_MODEL_ID,
            Self::SuperNailgun => SUPER_NAILGUN_MODEL_ID,
            Self::GrenadeLauncher => GRENADE_LAUNCHER_MODEL_ID,
            Self::RocketLauncher => ROCKET_LAUNCHER_MODEL_ID,
            Self::Lightning => LIGHTNING_MODEL_ID,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ArmorTier {
    Green,
    Yellow,
    Red,
}

impl ArmorTier {
    const fn absorption_percent(self) -> i32 {
        match self {
            Self::Green => 30,
            Self::Yellow => 60,
            Self::Red => 80,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pickup {
    Health {
        amount: u16,
        maximum: u16,
    },
    Armor {
        tier: ArmorTier,
        amount: u16,
    },
    Ammo {
        kind: AmmoKind,
        amount: u16,
    },
    Weapon {
        weapon: Weapon,
        ammo: AmmoKind,
        amount: u16,
    },
    Key {
        bit: u8,
    },
    Powerup {
        kind: PowerupKind,
    },
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PickupOutcome {
    pub consumed: bool,
    pub switched_weapon: bool,
    pub sound_id: Option<i16>,
    /// The id1 line this pickup prints.
    ///
    /// `health_touch` prints `"You receive " + healamount + " health"` and
    /// every other touch function prints `"You got the " + netname`, so the
    /// amounts and netnames are folded into whole lines: this port has no
    /// integer formatting and the shareware boxes only ever heal 15, 25 or
    /// 100. Keys keep the medieval wording for the same reason
    /// [`crate::door::needs_key_message`] does, the cooked map carrying no
    /// `worldtype` to select the keycard and runekey variants with.
    pub message: Option<&'static str>,
}

/// Decode implemented single-player shareware pickups from a cooked entity.
pub const fn pickup_for_entity(class_name: u8, spawn_flags: u16) -> Option<Pickup> {
    let large = spawn_flags & 1 != 0;
    match class_name {
        0x1d | 0x1e | 0x1f | 0x20 => match PowerupKind::from_class_name(class_name) {
            Some(kind) => Some(Pickup::Powerup { kind }),
            None => None,
        },
        0x1a => Some(Pickup::Armor {
            tier: ArmorTier::Green,
            amount: 100,
        }),
        0x1b => Some(Pickup::Armor {
            tier: ArmorTier::Yellow,
            amount: 150,
        }),
        0x1c => Some(Pickup::Armor {
            tier: ArmorTier::Red,
            amount: 200,
        }),
        0x21 => Some(Pickup::Ammo {
            kind: AmmoKind::Cells,
            amount: if large { 12 } else { 6 },
        }),
        0x22 => Some(Pickup::Health {
            amount: if large {
                15
            } else if spawn_flags & 2 != 0 {
                100
            } else {
                25
            },
            maximum: if spawn_flags & 2 != 0 { 250 } else { 100 },
        }),
        0x23 => Some(Pickup::Key { bit: 1 }),
        0x24 => Some(Pickup::Key { bit: 2 }),
        0x25 => Some(Pickup::Ammo {
            kind: AmmoKind::Rockets,
            amount: if large { 10 } else { 5 },
        }),
        0x26 => Some(Pickup::Ammo {
            kind: AmmoKind::Shells,
            amount: if large { 40 } else { 20 },
        }),
        0x28 => Some(Pickup::Ammo {
            kind: AmmoKind::Nails,
            amount: if large { 50 } else { 25 },
        }),
        0x56 => Some(Pickup::Weapon {
            weapon: Weapon::RocketLauncher,
            ammo: AmmoKind::Rockets,
            amount: 5,
        }),
        0x53 => Some(Pickup::Weapon {
            weapon: Weapon::GrenadeLauncher,
            ammo: AmmoKind::Rockets,
            amount: 5,
        }),
        0x54 => Some(Pickup::Weapon {
            weapon: Weapon::Lightning,
            ammo: AmmoKind::Cells,
            amount: 15,
        }),
        0x55 => Some(Pickup::Weapon {
            weapon: Weapon::Nailgun,
            ammo: AmmoKind::Nails,
            amount: 30,
        }),
        0x57 => Some(Pickup::Weapon {
            weapon: Weapon::SuperNailgun,
            ammo: AmmoKind::Nails,
            amount: 30,
        }),
        0x58 => Some(Pickup::Weapon {
            weapon: Weapon::SuperShotgun,
            ammo: AmmoKind::Shells,
            amount: 5,
        }),
        _ => None,
    }
}

/// State which must survive a resident-map replacement in single player.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Inventory {
    health: i16,
    armor: u16,
    armor_tier: Option<ArmorTier>,
    keys: u8,
    god_mode: bool,
    ammo: [u16; 4],
    owned_weapons: u8,
    active_weapon: Weapon,
    powerups: Powerups,
    megahealth_rot_ticks: u16,
}

impl Inventory {
    pub const fn new() -> Self {
        Self {
            health: 100,
            armor: 0,
            armor_tier: None,
            keys: 0,
            god_mode: false,
            ammo: [SHOTGUN_STARTING_SHELLS, 0, 0, 0],
            owned_weapons: WEAPON_AXE_BIT | WEAPON_SHOTGUN_BIT,
            active_weapon: Weapon::Shotgun,
            powerups: Powerups::new(),
            megahealth_rot_ticks: 0,
        }
    }

    pub const fn powerups(self) -> Powerups {
        self.powerups
    }

    /// True while a megahealth's rot think is still scheduled.
    pub const fn megahealth_rotting(self) -> bool {
        self.megahealth_rot_ticks != 0
    }

    /// `SetChangeParms`, the whole of it.
    ///
    /// ```text
    /// self.items = self.items - (self.items & (IT_KEY1 | IT_KEY2 |
    ///     IT_INVISIBILITY | IT_INVULNERABILITY | IT_SUIT | IT_QUAD));
    /// if (self.health > 100) self.health = 100;
    /// if (self.health < 50)  self.health = 50;
    /// parm4 = self.ammo_shells < 25 ? 25 : self.ammo_shells;
    /// ```
    ///
    /// So the four artifacts and both keys are stripped, super health is
    /// capped back to the ordinary maximum, a nearly dead player is topped up
    /// to half, and the next level always starts with at least a full shotgun
    /// load. Armor, armor type, nails, rockets, cells, owned weapons and the
    /// selected weapon all carry over untouched. The megahealth's rot think
    /// belongs to the item entity, which the level change destroys, so it
    /// stops here too.
    pub fn apply_change_parms(&mut self) {
        self.powerups.clear();
        self.megahealth_rot_ticks = 0;
        self.keys = 0;
        self.health = self
            .health
            .min(PLAYER_MAX_HEALTH)
            .max(CHANGE_LEVEL_MIN_HEALTH);
        self.ammo[AmmoKind::Shells.index()] =
            self.ammo[AmmoKind::Shells.index()].max(SHOTGUN_STARTING_SHELLS);
    }

    pub(crate) fn tick_powerups(&mut self, ticks: u16) -> (u8, u8) {
        self.powerups.tick(ticks)
    }

    /// One tick of `item_megahealth_rot`: five seconds after the pickup, one
    /// point per second until health is back down to `max_health`.
    pub(crate) fn tick_megahealth_rot(&mut self) {
        if self.megahealth_rot_ticks == 0 {
            return;
        }
        self.megahealth_rot_ticks -= 1;
        if self.megahealth_rot_ticks != 0 {
            return;
        }
        if self.health > PLAYER_MAX_HEALTH {
            self.health -= 1;
            self.megahealth_rot_ticks = MEGAHEALTH_ROT_INTERVAL_TICKS;
        }
    }

    pub const fn health(self) -> i16 {
        self.health
    }

    pub const fn armor(self) -> u16 {
        self.armor
    }

    pub const fn armor_tier(self) -> Option<ArmorTier> {
        self.armor_tier
    }

    pub const fn keys(self) -> u8 {
        self.keys
    }

    /// `FL_GODMODE`: every Quake damage source reaches `take_damage`, so the
    /// original developer toggle belongs at this one admission point.
    pub fn set_god_mode(&mut self, enabled: bool) {
        self.god_mode = enabled;
    }

    pub const fn god_mode(self) -> bool {
        self.god_mode
    }

    /// Original Quake's `impulse 9` grants all eight weapons and fills the
    /// four ammo pools. It deliberately neither changes the active weapon nor
    /// gives either key.
    pub fn impulse_nine(&mut self) {
        self.owned_weapons = u8::MAX;
        for kind in [
            AmmoKind::Shells,
            AmmoKind::Nails,
            AmmoKind::Rockets,
            AmmoKind::Cells,
        ] {
            self.ammo[kind.index()] = kind.maximum();
        }
    }

    /// `door_touch`'s `other.items = other.items - self.items`. A key door
    /// consumes the key it accepted in original WinQuake single player.
    pub fn take_key(&mut self, bit: u8) -> bool {
        if self.keys & bit != bit {
            return false;
        }
        self.keys &= !bit;
        true
    }

    pub const fn ammo(self, kind: AmmoKind) -> u16 {
        self.ammo[kind.index()]
    }

    pub const fn owns(self, weapon: Weapon) -> bool {
        self.owned_weapons & weapon.bit() != 0
    }

    /// Original `items` weapon bits, retained for the status-bar inventory
    /// strip. Bit zero is the axe; bits one through seven are its icon slots.
    pub const fn owned_weapons(self) -> u8 {
        self.owned_weapons
    }

    pub const fn can_use(self, weapon: Weapon) -> bool {
        if !self.owns(weapon) {
            return false;
        }
        match weapon {
            Weapon::Axe => true,
            Weapon::Shotgun => self.ammo(AmmoKind::Shells) != 0,
            Weapon::SuperShotgun => self.ammo(AmmoKind::Shells) != 0,
            Weapon::Nailgun | Weapon::SuperNailgun => self.ammo(AmmoKind::Nails) != 0,
            Weapon::GrenadeLauncher | Weapon::RocketLauncher => self.ammo(AmmoKind::Rockets) != 0,
            Weapon::Lightning => self.ammo(AmmoKind::Cells) != 0,
        }
    }

    pub const fn active_weapon(self) -> Weapon {
        self.active_weapon
    }

    pub fn select(&mut self, weapon: Weapon) -> bool {
        if !self.can_use(weapon) {
            return false;
        }
        self.active_weapon = weapon;
        true
    }

    #[optimize(size)]
    pub fn cycle(&mut self, forward: bool) -> Weapon {
        const ORDER: [Weapon; 8] = [
            Weapon::Axe,
            Weapon::Shotgun,
            Weapon::SuperShotgun,
            Weapon::Nailgun,
            Weapon::SuperNailgun,
            Weapon::GrenadeLauncher,
            Weapon::RocketLauncher,
            Weapon::Lightning,
        ];
        let current = ORDER
            .iter()
            .position(|weapon| *weapon == self.active_weapon)
            .unwrap_or(0);
        // Walking by a precomputed step rather than branching on `forward`
        // inside the loop, and over an opaque bound: written the obvious way
        // LLVM unswitched the direction into two loops and then unrolled each
        // seven times, so `can_use` landed here fourteen times over.
        let step = if forward { 1 } else { ORDER.len() - 1 };
        let bound = core::hint::black_box(ORDER.len());
        let mut index = current;
        let mut offset = 1usize;
        while offset < bound {
            index = (index + step) % ORDER.len();
            if self.can_use(ORDER[index]) {
                self.active_weapon = ORDER[index];
                break;
            }
            offset += 1;
        }
        self.active_weapon
    }

    #[optimize(size)]
    fn best_usable(self, water_level: u8) -> Weapon {
        for weapon in [
            Weapon::Lightning,
            Weapon::RocketLauncher,
            Weapon::GrenadeLauncher,
            Weapon::SuperNailgun,
            Weapon::Nailgun,
            Weapon::SuperShotgun,
            Weapon::Shotgun,
            Weapon::Axe,
        ] {
            if water_level > 1 && weapon == Weapon::Lightning {
                continue;
            }
            if self.can_use(weapon) {
                return weapon;
            }
        }
        self.active_weapon
    }

    #[optimize(size)]
    pub fn apply_pickup(&mut self, pickup: Pickup) -> PickupOutcome {
        let (consumed, switched_weapon, sound_id, message) = match pickup {
            Pickup::Health { amount, maximum } => {
                if self.health >= maximum as i16 {
                    (false, false, None, None)
                } else {
                    self.health = self
                        .health
                        .saturating_add(amount as i16)
                        .min(maximum as i16);
                    // `health_touch` schedules `item_megahealth_rot` five
                    // seconds out, but only for the megahealth healtype.
                    if maximum as i16 > PLAYER_MAX_HEALTH {
                        self.megahealth_rot_ticks = MEGAHEALTH_ROT_DELAY_TICKS;
                    }
                    // `health_touch`: rotten plays r_item1, mega r_item2,
                    // the standard box health1. `healamount` picks the line
                    // the same way.
                    let (sound, message) = match amount {
                        15 => (0x61, "You receive 15 health"),
                        100 => (0x62, "You receive 100 health"),
                        _ => (0x59, "You receive 25 health"),
                    };
                    (true, false, Some(sound), Some(message))
                }
            }
            Pickup::Armor { tier, amount } => {
                let current = self
                    .armor_tier
                    .map(|value| i32::from(self.armor) * value.absorption_percent())
                    .unwrap_or(0);
                let offered = i32::from(amount) * tier.absorption_percent();
                if current >= offered {
                    (false, false, None, None)
                } else {
                    self.armor = amount;
                    self.armor_tier = Some(tier);
                    (true, false, Some(0x55), Some("You got armor"))
                }
            }
            Pickup::Ammo { kind, amount } => {
                let index = kind.index();
                if self.ammo[index] >= kind.maximum() {
                    (false, false, None, None)
                } else {
                    self.ammo[index] = self.ammo[index].saturating_add(amount).min(kind.maximum());
                    const AMMO: [&str; 4] = [
                        "You got the shells",
                        "You got the nails",
                        "You got the rockets",
                        "You got the cells",
                    ];
                    (true, false, Some(0xc5), Some(AMMO[index]))
                }
            }
            Pickup::Weapon {
                weapon,
                ammo,
                amount,
            } => {
                // `weapon_touch` never refuses in single player: it always
                // adds the ammo (bounded), takes the item and sets
                // `self.weapon = new` regardless of what was held.
                let index = ammo.index();
                self.owned_weapons |= weapon.bit();
                self.ammo[index] = self.ammo[index].saturating_add(amount).min(ammo.maximum());
                let switched = self.active_weapon != weapon;
                self.active_weapon = weapon;
                (true, switched, Some(0xc7), weapon.pickup_netname())
            }
            Pickup::Key { bit } => {
                if self.keys & bit != 0 {
                    (false, false, None, None)
                } else {
                    self.keys |= bit;
                    let message = if bit & crate::door::KEY_GOLD_BIT != 0 {
                        "You got the gold key"
                    } else {
                        "You got the silver key"
                    };
                    (true, false, Some(0x6d), Some(message))
                }
            }
            // `powerup_touch` has no refusal case: it always arms a fresh
            // thirty seconds and removes the artifact.
            Pickup::Powerup { kind } => {
                self.powerups.arm(kind);
                const POWERUPS: [&str; 4] = [
                    "You got the Quad Damage",
                    "You got the Pentagram of Protection",
                    "You got the Ring of Shadows",
                    "You got the Biosuit",
                ];
                (
                    true,
                    false,
                    Some(kind.pickup_sound()),
                    Some(POWERUPS[kind.index()]),
                )
            }
        };
        PickupOutcome {
            consumed,
            switched_weapon,
            sound_id,
            message,
        }
    }

    pub fn take_damage(&mut self, damage: i16) -> i16 {
        if self.god_mode {
            return 0;
        }
        let damage = damage.max(0);
        let saved = self
            .armor_tier
            .map(|tier| {
                // Quake applies ceil(take * armortype), so even one point of
                // damage consumes one point of any active armor tier.
                (i32::from(damage) * tier.absorption_percent() + 99) / 100
            })
            .unwrap_or(0)
            .clamp(0, i32::from(self.armor)) as u16;
        self.armor -= saved;
        if self.armor == 0 {
            self.armor_tier = None;
        }
        // `T_Damage` deducts armor and only then checks invincibility, so the
        // Pentagram protects health while the armor still burns down.
        if self.powerups.active(PowerupKind::Pentagram) {
            return 0;
        }
        let taken = damage.saturating_sub(saved as i16);
        self.health = self.health.saturating_sub(taken);
        taken
    }

    fn consume(&mut self, kind: AmmoKind, amount: u16) -> bool {
        let value = &mut self.ammo[kind.index()];
        if *value < amount {
            return false;
        }
        *value -= amount;
        true
    }
}

impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime collision bounds and default health assigned by the original
/// QuakeC monster spawn functions. These are the actual `setsize` values,
/// which occasionally differ from the preceding `/*QUAKED ... */` editor
/// comment (notably tarbaby and shalrath).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonsterProfile {
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    pub health: i16,
}

/// Return the original Quake runtime profile for a supported damageable
/// monster class. A positive map-authored health value overrides QuakeC's
/// class default exactly as the cooked entity contract requires.
pub fn monster_profile(class_name: u8, authored_health: i16) -> Option<MonsterProfile> {
    let standard = (
        Vec3I16 {
            x: -16,
            y: -16,
            z: -24,
        },
        Vec3I16 {
            x: 16,
            y: 16,
            z: 40,
        },
    );
    let short = (
        Vec3I16 {
            x: -16,
            y: -16,
            z: -24,
        },
        Vec3I16 {
            x: 16,
            y: 16,
            z: 24,
        },
    );
    let large = (
        Vec3I16 {
            x: -32,
            y: -32,
            z: -24,
        },
        Vec3I16 {
            x: 32,
            y: 32,
            z: 64,
        },
    );
    let (mins, maxs, default_health) = match class_name {
        0x36 => (standard.0, standard.1, 30), // monster_army
        0x38 => (large.0, large.1, 300),      // monster_demon1
        0x39 => (
            large.0,
            Vec3I16 {
                x: 32,
                y: 32,
                z: 40,
            },
            25,
        ), // monster_dog
        0x3a => (standard.0, standard.1, 80), // monster_enforcer
        0x3b => (short.0, short.1, 25),       // monster_fish
        0x3c => (standard.0, standard.1, 250), // monster_hell_knight
        0x3d => (standard.0, standard.1, 75), // monster_knight
        0x3e => (large.0, large.1, 200),      // monster_ogre
        0x40 => (large.0, large.1, 400),      // monster_shalrath
        0x41 => (large.0, large.1, 600),      // monster_shambler
        0x42 => (standard.0, standard.1, 80), // monster_tarbaby
        0x43 => (standard.0, standard.1, 80), // monster_wizard
        // The zombie takes damage but only a gib-level blow kills it; the
        // knockdown policy lives in `monster::MonsterRuntime::take_damage`.
        0x44 => (standard.0, standard.1, 60), // monster_zombie
        // Chthon carries a body box so he blocks and can be traced against,
        // but the scene never marks him damageable: `event_lightning` is his
        // only kill. Health is his shock count, set when he is woken.
        0x37 => (
            Vec3I16 {
                x: -128,
                y: -128,
                z: -24,
            },
            Vec3I16 {
                x: 128,
                y: 128,
                z: 256,
            },
            3,
        ), // monster_boss
        // The Old One (0x3f) is registered-episode content with no authored
        // shareware instance and no runtime.
        _ => return None,
    };
    Some(MonsterProfile {
        mins,
        maxs,
        health: if authored_health > 0 {
            authored_health
        } else {
            default_health
        },
    })
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WeaponView {
    pub model_id: i16,
    pub frame: u16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShotgunAttack {
    pub start: Vec3I32,
    pub ends: [Vec3I32; MAX_SHOTGUN_PELLETS],
    pub pellet_count: u8,
    pub damage_per_pellet: i16,
    pub sound_id: i16,
    pub sequence: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HitscanAttack {
    pub start: Vec3I32,
    pub end: Vec3I32,
    pub damage: i16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RocketSpawn {
    pub origin: Vec3I32,
    pub step: Vec3I32,
    pub lifetime_ticks: u16,
    pub direct_damage: i16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NailSpawn {
    pub origin: Vec3I32,
    pub step: Vec3I32,
    pub lifetime_ticks: u16,
    pub damage: i16,
    pub sound_id: i16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GrenadeSpawn {
    pub origin: Vec3I32,
    pub velocity: Vec3I32,
    pub angles: Vec3I16,
    pub lifetime_ticks: u16,
    pub damage: i16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LightningAttack {
    pub beam_start: Vec3I32,
    pub start: Vec3I32,
    pub end: Vec3I32,
    pub forward: Vec3I32,
    pub right: Vec3I32,
    pub damage: i16,
    pub sound_id: Option<i16>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LightningDischarge {
    pub origin: Vec3I32,
    pub damage: i16,
    pub sound_id: Option<i16>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LightningTraceGeometry {
    pub beam_start: Vec3I32,
    pub beam_end: Vec3I32,
    pub starts: [Vec3I32; 3],
    pub ends: [Vec3I32; 3],
}

pub fn lightning_trace_geometry(
    attack: LightningAttack,
    world_fraction_q12: i32,
) -> LightningTraceGeometry {
    let fraction = world_fraction_q12.clamp(0, Q12_ONE);
    let component =
        |from: i32, to: i32| from.saturating_add(mul_q12_i32(to.saturating_sub(from), fraction));
    let clipped = Vec3I32 {
        x: component(attack.beam_start.x, attack.end.x)
            .saturating_add(attack.forward.x.saturating_mul(4)),
        y: component(attack.beam_start.y, attack.end.y)
            .saturating_add(attack.forward.y.saturating_mul(4)),
        z: component(attack.beam_start.z, attack.end.z)
            .saturating_add(attack.forward.z.saturating_mul(4)),
    };
    let offset = Vec3I32 {
        x: attack.right.x.saturating_mul(16),
        y: attack.right.y.saturating_mul(16),
        z: attack.right.z.saturating_mul(16),
    };
    LightningTraceGeometry {
        beam_start: attack.beam_start,
        beam_end: clipped,
        starts: [
            attack.start,
            add_combat_vec(attack.start, offset),
            sub_combat_vec(attack.start, offset),
        ],
        ends: [
            clipped,
            add_combat_vec(clipped, offset),
            sub_combat_vec(clipped, offset),
        ],
    }
}

const fn add_combat_vec(a: Vec3I32, b: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: a.x.saturating_add(b.x),
        y: a.y.saturating_add(b.y),
        z: a.z.saturating_add(b.z),
    }
}

const fn sub_combat_vec(a: Vec3I32, b: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: a.x.saturating_sub(b.x),
        y: a.y.saturating_sub(b.y),
        z: a.z.saturating_sub(b.z),
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WeaponAttack {
    Axe(HitscanAttack),
    Shotgun(ShotgunAttack),
    Nail(NailSpawn),
    Grenade(GrenadeSpawn),
    Rocket(RocketSpawn),
    Lightning(LightningAttack),
    LightningDischarge(LightningDischarge),
}

impl WeaponAttack {
    pub const fn sound_id(self) -> Option<i16> {
        match self {
            Self::Axe(_) => Some(0xc0),
            Self::Shotgun(attack) => Some(attack.sound_id),
            Self::Nail(spawn) => Some(spawn.sound_id),
            Self::Grenade(_) => Some(0xc2),
            Self::Rocket(_) => Some(0xcd),
            Self::Lightning(attack) => attack.sound_id,
            Self::LightningDischarge(discharge) => discharge.sound_id,
        }
    }

    /// Original `punchangle_x` authored by each Quake 1 firing function.
    /// The underwater Thunderbolt returns before its kick, but its player
    /// attack frame has already raised the muzzle flash.
    pub const fn recoil_pitch(self) -> i32 {
        match self {
            Self::Axe(_) | Self::LightningDischarge(_) => 0,
            Self::Shotgun(attack) if attack.pellet_count as usize > SHOTGUN_PELLETS => -4,
            Self::Shotgun(_)
            | Self::Nail(_)
            | Self::Grenade(_)
            | Self::Rocket(_)
            | Self::Lightning(_) => -2,
        }
    }

    /// `player_shot1`, `player_nail1`, `player_rocket1` and `player_light1`
    /// all set `EF_MUZZLEFLASH`; only the axe attack omits it.
    pub const fn muzzle_flashes(self) -> bool {
        !matches!(self, Self::Axe(_))
    }
}

/// Persistent single-player weapon state. It intentionally lives outside a
/// resident map so shells, cadence, and animation survive level transitions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WeaponState {
    inventory: Inventory,
    survival: Survival,
    refire_ticks: u16,
    view_frame: u16,
    view_frame_end: u16,
    view_frame_ticks: u8,
    random_state: u32,
    shots_fired: u32,
    sustained_weapon: Option<Weapon>,
    nail_next_positive: bool,
}

impl WeaponState {
    pub const fn new() -> Self {
        Self {
            inventory: Inventory::new(),
            survival: Survival::new(),
            refire_ticks: 0,
            view_frame: 0,
            view_frame_end: 0,
            view_frame_ticks: 0,
            random_state: INITIAL_RANDOM_STATE,
            shots_fired: 0,
            sustained_weapon: None,
            nail_next_positive: true,
        }
    }

    /// Advance Quake's environmental survival rules against the persistent
    /// inventory. This is the single entry point the guest frame loop needs.
    pub fn tick_survival(&mut self, input: SurvivalInput) -> SurvivalFrame {
        self.survival.tick(&mut self.inventory, input)
    }

    /// Restart a life with `SetNewParms`: axe and shotgun, 25 shells, 100
    /// health, no armor, no keys, no powerups, and a fresh air supply. Every
    /// other level change keeps the inventory, so this is deliberately the
    /// only path that discards it.
    pub fn respawn(&mut self) {
        *self = Self::new();
    }

    pub const fn shells(&self) -> u16 {
        self.inventory.ammo(AmmoKind::Shells)
    }

    pub const fn inventory(&self) -> Inventory {
        self.inventory
    }

    /// Spend a key on the door that just accepted it.
    pub fn take_key(&mut self, bit: u8) -> bool {
        self.inventory.take_key(bit)
    }

    #[cfg(test)]
    fn inventory_mut_for_test(&mut self) -> &mut Inventory {
        &mut self.inventory
    }

    pub const fn active_weapon(&self) -> Weapon {
        self.inventory.active_weapon()
    }

    pub fn apply_pickup(&mut self, pickup: Pickup) -> PickupOutcome {
        let outcome = self.inventory.apply_pickup(pickup);
        if outcome.switched_weapon {
            self.reset_animation();
        }
        outcome
    }

    pub fn select(&mut self, weapon: Weapon) -> bool {
        let selected = self.inventory.select(weapon);
        if selected {
            self.reset_animation();
        }
        selected
    }

    pub fn cycle(&mut self, forward: bool) -> Weapon {
        let selected = self.inventory.cycle(forward);
        self.reset_animation();
        selected
    }

    pub fn take_damage(&mut self, damage: i16) -> i16 {
        self.inventory.take_damage(damage)
    }

    pub fn set_god_mode(&mut self, enabled: bool) {
        self.inventory.set_god_mode(enabled);
    }

    pub const fn god_mode(&self) -> bool {
        self.inventory.god_mode()
    }

    pub fn impulse_nine(&mut self) {
        self.inventory.impulse_nine();
    }

    pub const fn shots_fired(&self) -> u32 {
        self.shots_fired
    }

    pub const fn view(&self) -> WeaponView {
        WeaponView {
            model_id: self.inventory.active_weapon().model_id(),
            frame: self.view_frame,
        }
    }

    /// Advance the 60 Hz weapon clock without coupling it to presentation.
    pub fn tick(&mut self, elapsed_ticks: u16) {
        let ticks = elapsed_ticks.clamp(1, 4);
        self.refire_ticks = self.refire_ticks.saturating_sub(ticks);
        // `W_CheckNoAmmo`: once the last round is gone the original swaps
        // to `W_BestWeapon` on the next `W_Attack` after `attack_finished`.
        // Doing it here as soon as the refire window closes keeps that
        // timing for a held trigger and lets the HUD show the swap when the
        // trigger was released.
        let active = self.inventory.active_weapon();
        if self.refire_ticks == 0 && !self.inventory.can_use(active) {
            let best = self.inventory.best_usable(self.survival.water_level());
            if best != active {
                self.inventory.active_weapon = best;
                self.reset_animation();
            }
        }
        if self.view_frame == 0 || self.sustained_weapon.is_some() {
            return;
        }
        self.view_frame_ticks = self.view_frame_ticks.saturating_add(ticks as u8);
        while self.view_frame_ticks >= VIEW_MODEL_FRAME_TICKS {
            self.view_frame_ticks -= VIEW_MODEL_FRAME_TICKS;
            if self.view_frame < self.view_frame_end {
                self.view_frame += 1;
            } else {
                self.view_frame = 0;
                self.view_frame_ticks = 0;
                break;
            }
        }
    }

    /// Fire while the trigger is held, matching Quake's half-second shotgun
    /// cadence. The caller supplies the real cooked view-model frame count.
    pub fn try_fire(
        &mut self,
        trigger_held: bool,
        camera_origin: Vec3I32,
        view_angles: [i16; 3],
        view_model_frames: u16,
    ) -> Option<ShotgunAttack> {
        match self.try_attack(trigger_held, camera_origin, view_angles, view_model_frames) {
            Some(WeaponAttack::Shotgun(attack)) => Some(attack),
            _ => None,
        }
    }

    pub fn try_attack(
        &mut self,
        trigger_held: bool,
        camera_origin: Vec3I32,
        view_angles: [i16; 3],
        view_model_frames: u16,
    ) -> Option<WeaponAttack> {
        self.try_attack_in_water_with_admission(
            trigger_held,
            camera_origin,
            view_angles,
            view_model_frames,
            0,
            AttackAdmission::ALL,
        )
    }

    pub fn try_attack_in_water(
        &mut self,
        trigger_held: bool,
        camera_origin: Vec3I32,
        view_angles: [i16; 3],
        view_model_frames: u16,
        water_level: u8,
    ) -> Option<WeaponAttack> {
        self.try_attack_in_water_with_admission(
            trigger_held,
            camera_origin,
            view_angles,
            view_model_frames,
            water_level,
            AttackAdmission::ALL,
        )
    }

    /// True when the refire delay has elapsed, so a held trigger fires this
    /// frame.
    pub const fn ready_to_fire(&self) -> bool {
        self.refire_ticks == 0
    }

    pub fn attack_weapon(&self, trigger_held: bool, water_level: u8) -> Weapon {
        if !trigger_held || self.refire_ticks != 0 {
            return self.inventory.active_weapon();
        }
        let mut weapon = self.inventory.active_weapon();
        if !self.inventory.can_use(weapon) {
            weapon = self.inventory.best_usable(water_level);
        }
        if weapon == Weapon::SuperShotgun && self.inventory.ammo(AmmoKind::Shells) == 1 {
            weapon = Weapon::Shotgun;
        }
        weapon
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_attack_in_water_with_admission(
        &mut self,
        trigger_held: bool,
        camera_origin: Vec3I32,
        view_angles: [i16; 3],
        view_model_frames: u16,
        water_level: u8,
        admission: AttackAdmission,
    ) -> Option<WeaponAttack> {
        self.try_attack_aimed(
            trigger_held,
            camera_origin,
            view_angles,
            view_model_frames,
            water_level,
            admission,
            None,
        )
    }

    /// `aim_forward` is the world's `aim(self, ...)` answer for this frame,
    /// used as the shot direction by the weapons that call it in the
    /// original (`Weapon::auto_aims`); `None` fires straight down `v_forward`.
    #[allow(clippy::too_many_arguments)]
    pub fn try_attack_aimed(
        &mut self,
        trigger_held: bool,
        camera_origin: Vec3I32,
        view_angles: [i16; 3],
        view_model_frames: u16,
        water_level: u8,
        admission: AttackAdmission,
        aim_forward: Option<Vec3I32>,
    ) -> Option<WeaponAttack> {
        if !trigger_held {
            self.reset_sustained_fire();
            return None;
        }
        if self.refire_ticks != 0 {
            return None;
        }
        let attack_weapon = self.attack_weapon(true, water_level);
        if !admission.allows(attack_weapon) {
            return None;
        }
        if self.inventory.active_weapon() != attack_weapon {
            self.inventory.active_weapon = attack_weapon;
            self.reset_animation();
        }
        let (forward, right, up) = view_basis(view_angles);
        let aim = aim_forward.unwrap_or(forward);
        let active = self.inventory.active_weapon();
        let continued = self.sustained_weapon == Some(active);
        if !matches!(
            active,
            Weapon::Nailgun | Weapon::SuperNailgun | Weapon::Lightning
        ) {
            self.sustained_weapon = None;
            self.nail_next_positive = true;
        }
        let mut attack = match active {
            Weapon::Axe => {
                self.refire_ticks = AXE_REFIRE_TICKS;
                let start = Vec3I32 {
                    x: camera_origin.x,
                    y: camera_origin.y,
                    z: camera_origin.z.saturating_sub(6 << 12),
                };
                WeaponAttack::Axe(HitscanAttack {
                    start,
                    end: Vec3I32 {
                        x: start
                            .x
                            .saturating_add(forward.x.saturating_mul(AXE_RANGE_UNITS)),
                        y: start
                            .y
                            .saturating_add(forward.y.saturating_mul(AXE_RANGE_UNITS)),
                        z: start
                            .z
                            .saturating_add(forward.z.saturating_mul(AXE_RANGE_UNITS)),
                    },
                    damage: AXE_DAMAGE,
                })
            }
            Weapon::Shotgun => {
                if !self.inventory.consume(AmmoKind::Shells, 1) {
                    return None;
                }
                self.refire_ticks = SHOTGUN_REFIRE_TICKS;
                WeaponAttack::Shotgun(self.pellet_attack(
                    camera_origin,
                    forward,
                    aim,
                    right,
                    up,
                    SHOTGUN_PELLETS,
                    SHOTGUN_SPREAD_Q12,
                    SHOTGUN_SPREAD_Q12,
                    0xc3,
                ))
            }
            Weapon::SuperShotgun => {
                if !self.inventory.consume(AmmoKind::Shells, 2) {
                    return None;
                }
                self.refire_ticks = SUPER_SHOTGUN_REFIRE_TICKS;
                WeaponAttack::Shotgun(self.pellet_attack(
                    camera_origin,
                    forward,
                    aim,
                    right,
                    up,
                    SUPER_SHOTGUN_PELLETS,
                    SUPER_SHOTGUN_HORIZONTAL_SPREAD_Q12,
                    SUPER_SHOTGUN_VERTICAL_SPREAD_Q12,
                    0xce,
                ))
            }
            Weapon::Nailgun | Weapon::SuperNailgun => {
                let super_nail =
                    active == Weapon::SuperNailgun && self.inventory.ammo(AmmoKind::Nails) >= 2;
                let ammo = if super_nail { 2 } else { 1 };
                if !self.inventory.consume(AmmoKind::Nails, ammo) {
                    return None;
                }
                self.refire_ticks = NAIL_REFIRE_TICKS;
                let uses_lateral = active == Weapon::Nailgun
                    || (active == Weapon::SuperNailgun && !continued && !super_nail);
                let lateral = if uses_lateral {
                    let lateral = if self.nail_next_positive { 4 } else { -4 };
                    self.nail_next_positive = !self.nail_next_positive;
                    lateral
                } else {
                    0
                };
                self.sustained_weapon = Some(active);
                let base = Vec3I32 {
                    x: camera_origin.x,
                    y: camera_origin.y,
                    z: camera_origin.z.saturating_sub(6 << 12),
                };
                WeaponAttack::Nail(NailSpawn {
                    origin: Vec3I32 {
                        x: base.x.saturating_add(right.x.saturating_mul(lateral)),
                        y: base.y.saturating_add(right.y.saturating_mul(lateral)),
                        z: base.z.saturating_add(right.z.saturating_mul(lateral)),
                    },
                    step: Vec3I32 {
                        x: mul_q12_i32(aim.x, ROCKET_STEP_Q12),
                        y: mul_q12_i32(aim.y, ROCKET_STEP_Q12),
                        z: mul_q12_i32(aim.z, ROCKET_STEP_Q12),
                    },
                    lifetime_ticks: NAIL_LIFETIME_TICKS,
                    damage: if super_nail {
                        SUPER_NAIL_DAMAGE
                    } else {
                        NAIL_DAMAGE
                    },
                    sound_id: if active == Weapon::SuperNailgun {
                        0xcf
                    } else {
                        0xcc
                    },
                })
            }
            Weapon::GrenadeLauncher => {
                if !self.inventory.consume(AmmoKind::Rockets, 1) {
                    return None;
                }
                self.refire_ticks = GRENADE_REFIRE_TICKS;
                let forward_step = 600 * Q12_ONE / 60;
                let upward_step = 200 * Q12_ONE / 60;
                let mut velocity = if view_angles[0] != 0 {
                    Vec3I32 {
                        x: mul_q12_i32(forward.x, forward_step)
                            .saturating_add(mul_q12_i32(up.x, upward_step)),
                        y: mul_q12_i32(forward.y, forward_step)
                            .saturating_add(mul_q12_i32(up.y, upward_step)),
                        z: mul_q12_i32(forward.z, forward_step)
                            .saturating_add(mul_q12_i32(up.z, upward_step)),
                    }
                } else {
                    // `missile.velocity = aim(self, 10000) * 600;
                    // missile.velocity_z = 200`.
                    Vec3I32 {
                        x: mul_q12_i32(aim.x, forward_step),
                        y: mul_q12_i32(aim.y, forward_step),
                        z: upward_step,
                    }
                };
                if view_angles[0] != 0 {
                    let random_right = mul_q12_i32(self.trace_random(), 10 * Q12_ONE / 60);
                    let random_up = mul_q12_i32(self.trace_random(), 10 * Q12_ONE / 60);
                    velocity.x = velocity
                        .x
                        .saturating_add(mul_q12_i32(right.x, random_right))
                        .saturating_add(mul_q12_i32(up.x, random_up));
                    velocity.y = velocity
                        .y
                        .saturating_add(mul_q12_i32(right.y, random_right))
                        .saturating_add(mul_q12_i32(up.y, random_up));
                    velocity.z = velocity
                        .z
                        .saturating_add(mul_q12_i32(right.z, random_right))
                        .saturating_add(mul_q12_i32(up.z, random_up));
                }
                WeaponAttack::Grenade(GrenadeSpawn {
                    origin: Vec3I32 {
                        x: camera_origin.x,
                        y: camera_origin.y,
                        z: camera_origin.z.saturating_sub(22 << 12),
                    },
                    velocity,
                    angles: Vec3I16 {
                        x: view_angles[0],
                        y: view_angles[1],
                        z: view_angles[2],
                    },
                    lifetime_ticks: GRENADE_LIFETIME_TICKS,
                    damage: GRENADE_DAMAGE,
                })
            }
            Weapon::RocketLauncher => {
                if !self.inventory.consume(AmmoKind::Rockets, 1) {
                    return None;
                }
                self.refire_ticks = ROCKET_REFIRE_TICKS;
                let base = Vec3I32 {
                    x: camera_origin.x,
                    y: camera_origin.y,
                    z: camera_origin.z.saturating_sub(6 << 12),
                };
                WeaponAttack::Rocket(RocketSpawn {
                    origin: Vec3I32 {
                        x: base.x.saturating_add(forward.x.saturating_mul(8)),
                        y: base.y.saturating_add(forward.y.saturating_mul(8)),
                        z: base.z.saturating_add(forward.z.saturating_mul(8)),
                    },
                    step: Vec3I32 {
                        x: mul_q12_i32(aim.x, ROCKET_STEP_Q12),
                        y: mul_q12_i32(aim.y, ROCKET_STEP_Q12),
                        z: mul_q12_i32(aim.z, ROCKET_STEP_Q12),
                    },
                    lifetime_ticks: ROCKET_LIFETIME_TICKS,
                    direct_damage: ROCKET_DIRECT_DAMAGE
                        + mul_q12_i32(self.random_fraction_q12(), 20) as i16,
                })
            }
            Weapon::Lightning => {
                let cells = self.inventory.ammo(AmmoKind::Cells);
                let sound_id = if !continued {
                    Some(0xc6)
                } else if self.view_frame == 1 {
                    Some(0xc4)
                } else {
                    None
                };
                if water_level > 1 {
                    if !self.inventory.consume(AmmoKind::Cells, cells) {
                        return None;
                    }
                    self.refire_ticks = LIGHTNING_REFIRE_TICKS;
                    WeaponAttack::LightningDischarge(LightningDischarge {
                        origin: Vec3I32 {
                            x: camera_origin.x,
                            y: camera_origin.y,
                            z: camera_origin.z.saturating_sub(22 << 12),
                        },
                        damage: (i32::from(cells) * 35).min(i16::MAX as i32) as i16,
                        sound_id,
                    })
                } else {
                    if !self.inventory.consume(AmmoKind::Cells, 1) {
                        return None;
                    }
                    self.refire_ticks = LIGHTNING_REFIRE_TICKS;
                    let start = Vec3I32 {
                        x: camera_origin.x,
                        y: camera_origin.y,
                        z: camera_origin.z.saturating_sub(6 << 12),
                    };
                    self.sustained_weapon = Some(Weapon::Lightning);
                    WeaponAttack::Lightning(LightningAttack {
                        beam_start: Vec3I32 {
                            x: camera_origin.x,
                            y: camera_origin.y,
                            z: camera_origin.z.saturating_sub(22 << 12),
                        },
                        start,
                        end: Vec3I32 {
                            x: start
                                .x
                                .saturating_add(forward.x.saturating_mul(LIGHTNING_RANGE_UNITS)),
                            y: start
                                .y
                                .saturating_add(forward.y.saturating_mul(LIGHTNING_RANGE_UNITS)),
                            z: start
                                .z
                                .saturating_add(forward.z.saturating_mul(LIGHTNING_RANGE_UNITS)),
                        },
                        forward,
                        right,
                        damage: LIGHTNING_DAMAGE,
                        sound_id,
                    })
                }
            }
        };
        // `T_Damage` multiplies by four while the attacker holds the quad.
        // The original tests it when the damage lands, so a projectile fired
        // in the last instant of the quad would still be quadded on impact
        // here where it would not be in the original; every hitscan weapon,
        // which is where the quad is felt, is identical either way.
        if self.inventory.powerups.active(PowerupKind::Quad) {
            quad_damage(&mut attack);
        }
        if matches!(attack, WeaponAttack::LightningDischarge(_)) {
            self.reset_animation();
        } else if matches!(
            active,
            Weapon::Nailgun | Weapon::SuperNailgun | Weapon::Lightning
        ) {
            let first_frame = 1.min(view_model_frames.saturating_sub(1));
            let last_frame = view_model_frames.saturating_sub(1);
            self.view_frame = if !continued {
                first_frame
            } else if self.view_frame < last_frame {
                self.view_frame.saturating_add(1)
            } else {
                first_frame
            };
            self.view_frame_end = last_frame;
            self.view_frame_ticks = 0;
        } else {
            self.view_frame = 1.min(view_model_frames.saturating_sub(1));
            self.view_frame_end = view_model_frames.saturating_sub(1);
            self.view_frame_ticks = 0;
        }
        self.shots_fired = self.shots_fired.wrapping_add(1);
        Some(attack)
    }

    #[allow(clippy::too_many_arguments)]
    fn pellet_attack(
        &mut self,
        camera_origin: Vec3I32,
        forward: Vec3I32,
        aim: Vec3I32,
        right: Vec3I32,
        up: Vec3I32,
        pellet_count: usize,
        horizontal_spread: i32,
        vertical_spread: i32,
        sound_id: i16,
    ) -> ShotgunAttack {
        let start = Vec3I32 {
            x: camera_origin.x.saturating_add(forward.x.saturating_mul(10)),
            y: camera_origin.y.saturating_add(forward.y.saturating_mul(10)),
            z: camera_origin.z.saturating_sub(7 << 12),
        };
        let mut ends = [Vec3I32::default(); MAX_SHOTGUN_PELLETS];
        let mut index = 0usize;
        while index < pellet_count {
            let random_up = mul_q12_i32(self.trace_random(), vertical_spread);
            let random_right = mul_q12_i32(self.trace_random(), horizontal_spread);
            // `FireBullets`: `src = origin + v_forward * 10` but the pellet
            // spreads around the `aim()` direction.
            let direction = Vec3I32 {
                x: aim
                    .x
                    .saturating_add(mul_q12_i32(random_up, up.x))
                    .saturating_add(mul_q12_i32(random_right, right.x)),
                y: aim
                    .y
                    .saturating_add(mul_q12_i32(random_up, up.y))
                    .saturating_add(mul_q12_i32(random_right, right.y)),
                z: aim
                    .z
                    .saturating_add(mul_q12_i32(random_up, up.z))
                    .saturating_add(mul_q12_i32(random_right, right.z)),
            };
            ends[index] = Vec3I32 {
                x: start
                    .x
                    .saturating_add(direction.x.saturating_mul(SHOTGUN_RANGE_UNITS)),
                y: start
                    .y
                    .saturating_add(direction.y.saturating_mul(SHOTGUN_RANGE_UNITS)),
                z: start
                    .z
                    .saturating_add(direction.z.saturating_mul(SHOTGUN_RANGE_UNITS)),
            };
            index += 1;
        }
        ShotgunAttack {
            start,
            ends,
            pellet_count: pellet_count as u8,
            damage_per_pellet: SHOTGUN_DAMAGE_PER_PELLET,
            sound_id,
            sequence: self.shots_fired,
        }
    }

    fn reset_animation(&mut self) {
        self.view_frame = 0;
        self.view_frame_end = 0;
        self.view_frame_ticks = 0;
        self.reset_sustained_fire();
    }

    fn reset_sustained_fire(&mut self) {
        self.sustained_weapon = None;
        self.nail_next_positive = true;
    }

    /// Clear per-map firing state and carry the inventory across a level
    /// change exactly as `SetChangeParms` does.
    pub fn map_loaded(&mut self) {
        self.refire_ticks = 0;
        self.random_state = INITIAL_RANDOM_STATE;
        self.survival.map_loaded();
        // A player who arrives at the change-level volume dead would have gone
        // through `SetNewParms` instead, but this port restarts the level on
        // death rather than carrying a corpse through, so the live path is the
        // only reachable one.
        self.inventory.apply_change_parms();
        self.reset_animation();
    }

    /// The historical port's LCG and `trace_random()` range, retained exactly
    /// so a shot sequence is stable across host and MIPS builds.
    fn trace_random(&mut self) -> i32 {
        let q12 = self.random_fraction_q12();
        2 * (q12 - (Q12_ONE >> 1))
    }

    fn random_fraction_q12(&mut self) -> i32 {
        self.random_state = self
            .random_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        ((self.random_state >> 1) & 0x7fff_ffff) as i32 & (Q12_ONE - 1)
    }
}

impl Default for WeaponState {
    fn default() -> Self {
        Self::new()
    }
}

/// Quadruple an attack's carried damage. The rocket's radius damage is not
/// carried on the spawn, so it stays at its base value.
fn quad_damage(attack: &mut WeaponAttack) {
    let scale = |damage: &mut i16| *damage = damage.saturating_mul(QUAD_DAMAGE_MULTIPLIER);
    match attack {
        WeaponAttack::Axe(attack) => scale(&mut attack.damage),
        WeaponAttack::Shotgun(attack) => scale(&mut attack.damage_per_pellet),
        WeaponAttack::Nail(spawn) => scale(&mut spawn.damage),
        WeaponAttack::Grenade(spawn) => scale(&mut spawn.damage),
        WeaponAttack::Rocket(spawn) => scale(&mut spawn.direct_damage),
        WeaponAttack::Lightning(attack) => scale(&mut attack.damage),
        WeaponAttack::LightningDischarge(discharge) => scale(&mut discharge.damage),
    }
}

/// Segment/AABB entry fraction in Q12. A start on or inside the box returns
/// zero. Parallel outside axes and non-overlapping slabs return `None`.
pub fn segment_aabb_fraction(
    start: Vec3I32,
    end: Vec3I32,
    mins: Vec3I32,
    maxs: Vec3I32,
) -> Option<i32> {
    let starts = [start.x, start.y, start.z];
    let ends = [end.x, end.y, end.z];
    let mins = [mins.x, mins.y, mins.z];
    let maxs = [maxs.x, maxs.y, maxs.z];
    let mut enter = 0i32;
    let mut leave = Q12_ONE;
    let mut axis = 0usize;
    while axis < 3 {
        let delta = i64::from(ends[axis]) - i64::from(starts[axis]);
        if delta == 0 {
            if starts[axis] < mins[axis] || starts[axis] > maxs[axis] {
                return None;
            }
            axis += 1;
            continue;
        }
        let mut near = difference_ratio_q12(i64::from(mins[axis]) - i64::from(starts[axis]), delta);
        let mut far = difference_ratio_q12(i64::from(maxs[axis]) - i64::from(starts[axis]), delta);
        if near > far {
            core::mem::swap(&mut near, &mut far);
        }
        enter = enter.max(near);
        leave = leave.min(far);
        if enter > leave {
            return None;
        }
        axis += 1;
    }
    (leave >= 0 && enter <= Q12_ONE).then_some(enter.clamp(0, Q12_ONE))
}

/// Conservative whole-unit broad phase for a Q20.12 segment and absolute
/// model bounds. This is shared with mover-occlusion tests so the cull cannot
/// silently diverge from the guest's point-trace path.
pub fn segment_overlaps_i16_bounds(
    start: Vec3I32,
    end: Vec3I32,
    mins: [i16; 3],
    maxs: [i16; 3],
) -> bool {
    let starts = [start.x >> 12, start.y >> 12, start.z >> 12];
    let ends = [end.x >> 12, end.y >> 12, end.z >> 12];
    (0..3).all(|axis| {
        starts[axis].min(ends[axis]) <= i32::from(maxs[axis])
            && starts[axis].max(ends[axis]) >= i32::from(mins[axis])
    })
}

/// Divide two signed coordinate differences without introducing a 64-bit
/// division helper on MIPS. A Q20.12 endpoint subtraction can be one bit wider
/// than i32; when the denominator has that extra bit, halve both differences
/// toward zero before calling the engine's 32-bit Q12 divider. If only the
/// numerator is wider, the ratio is necessarily outside [-1, 1], so a signed
/// saturated result preserves every slab decision in the segment interval.
#[inline]
fn difference_ratio_q12(numerator: i64, denominator: i64) -> i32 {
    debug_assert_ne!(denominator, 0);
    if denominator > i64::from(i32::MAX) || denominator < i64::from(i32::MIN) {
        return div_q12_i32(
            half_toward_zero(numerator) as i32,
            half_toward_zero(denominator) as i32,
        );
    }
    if numerator > i64::from(i32::MAX) || numerator < i64::from(i32::MIN) {
        return if (numerator < 0) ^ (denominator < 0) {
            i32::MIN
        } else {
            i32::MAX
        };
    }
    div_q12_i32(numerator as i32, denominator as i32)
}

#[inline(always)]
fn half_toward_zero(value: i64) -> i64 {
    (value >> 1) + i64::from(value < 0 && value & 1 != 0)
}

/// `v_forward` for the view angles, in Q12.
pub fn view_forward(angles: [i16; 3]) -> Vec3I32 {
    view_basis(angles).0
}

pub fn view_basis(angles: [i16; 3]) -> (Vec3I32, Vec3I32, Vec3I32) {
    let pitch = angles[0] as u16 & 0x0fff;
    let yaw = angles[1] as u16 & 0x0fff;
    let roll = angles[2] as u16 & 0x0fff;
    let (sp, cp) = (sin_q12(pitch), cos_q12(pitch));
    let (sy, cy) = (sin_q12(yaw), cos_q12(yaw));
    let (sr, cr) = (sin_q12(roll), cos_q12(roll));
    let multiply = mul_q12_i32;
    let forward = Vec3I32 {
        x: multiply(cp, cy),
        y: multiply(cp, sy),
        z: -sp,
    };
    let right = Vec3I32 {
        x: multiply(multiply(-sr, sp), cy).saturating_add(multiply(-cr, -sy)),
        y: multiply(multiply(-sr, sp), sy).saturating_add(multiply(-cr, cy)),
        z: multiply(-sr, cp),
    };
    let up = Vec3I32 {
        x: multiply(multiply(cr, sp), cy).saturating_add(multiply(-sr, -sy)),
        y: multiply(multiply(cr, sp), sy).saturating_add(multiply(-sr, cy)),
        z: multiply(cr, cp),
    };
    (forward, right, up)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: Vec3I32 = Vec3I32 { x: 0, y: 0, z: 0 };

    #[optimize(size)]
    #[test]
    fn quake_weapon_impulses_cover_the_complete_arsenal_in_order() {
        let expected = [
            Weapon::Axe,
            Weapon::Shotgun,
            Weapon::SuperShotgun,
            Weapon::Nailgun,
            Weapon::SuperNailgun,
            Weapon::GrenadeLauncher,
            Weapon::RocketLauncher,
            Weapon::Lightning,
        ];
        for (index, weapon) in expected.into_iter().enumerate() {
            assert_eq!(Weapon::from_impulse(index as u8 + 1), Some(weapon));
        }
        assert_eq!(Weapon::from_impulse(0), None);
        assert_eq!(Weapon::from_impulse(9), None);
    }

    #[test]
    fn respawning_restores_set_new_parms_and_discards_everything_else() {
        let mut weapon = WeaponState::new();
        weapon.apply_pickup(pickup_for_entity(0x56, 0).expect("rocket launcher"));
        weapon.apply_pickup(Pickup::Armor {
            tier: ArmorTier::Red,
            amount: 200,
        });
        weapon.apply_pickup(Pickup::Key { bit: 1 });
        weapon.apply_pickup(Pickup::Key { bit: 2 });
        weapon.apply_pickup(Pickup::Ammo {
            kind: AmmoKind::Nails,
            amount: 200,
        });
        weapon.take_damage(1000);
        assert!(weapon.inventory().health() <= 0);

        weapon.respawn();
        let inventory = weapon.inventory();
        assert_eq!(inventory.health(), 100);
        assert_eq!(inventory.armor(), 0);
        assert_eq!(inventory.armor_tier(), None);
        assert_eq!(inventory.keys(), 0);
        assert_eq!(inventory.active_weapon(), Weapon::Shotgun);
        assert_eq!(inventory.ammo(AmmoKind::Shells), SHOTGUN_STARTING_SHELLS);
        assert_eq!(inventory.ammo(AmmoKind::Nails), 0);
        assert_eq!(inventory.ammo(AmmoKind::Rockets), 0);
        assert_eq!(inventory.ammo(AmmoKind::Cells), 0);
        assert!(inventory.owns(Weapon::Axe));
        assert!(inventory.owns(Weapon::Shotgun));
        for weapon_kind in [
            Weapon::SuperShotgun,
            Weapon::Nailgun,
            Weapon::SuperNailgun,
            Weapon::GrenadeLauncher,
            Weapon::RocketLauncher,
            Weapon::Lightning,
        ] {
            assert!(!inventory.owns(weapon_kind));
        }
        assert_eq!(weapon, WeaponState::new());
    }

    /// An ordinary level change carries the armour, ammo and weapons. Only
    /// death restarts a life.
    #[test]
    fn an_ordinary_map_change_keeps_armour_ammo_and_weapons() {
        let mut weapon = WeaponState::new();
        weapon.apply_pickup(pickup_for_entity(0x56, 0).expect("rocket launcher"));
        weapon.apply_pickup(Pickup::Armor {
            tier: ArmorTier::Yellow,
            amount: 150,
        });
        weapon.apply_pickup(Pickup::Ammo {
            kind: AmmoKind::Rockets,
            amount: 5,
        });
        weapon.take_damage(30);
        let before = weapon.inventory();
        weapon.map_loaded();
        let after = weapon.inventory();
        assert_eq!(after.armor(), before.armor());
        assert_eq!(after.armor_tier(), before.armor_tier());
        assert_eq!(
            after.ammo(AmmoKind::Rockets),
            before.ammo(AmmoKind::Rockets)
        );
        assert_eq!(after.ammo(AmmoKind::Nails), before.ammo(AmmoKind::Nails));
        assert_eq!(after.ammo(AmmoKind::Cells), before.ammo(AmmoKind::Cells));
        assert!(after.owns(Weapon::RocketLauncher));
        assert_eq!(after.active_weapon(), before.active_weapon());
        assert_eq!(after.health(), before.health());
    }

    /// `SetChangeParms` strips both keys along with the artifacts: a key opens
    /// one level's doors, never the next one's.
    #[test]
    fn a_map_change_drops_both_keys() {
        let mut weapon = WeaponState::new();
        weapon.apply_pickup(Pickup::Key { bit: 1 });
        weapon.apply_pickup(Pickup::Key { bit: 2 });
        assert_eq!(weapon.inventory().keys(), 3);
        weapon.map_loaded();
        assert_eq!(weapon.inventory().keys(), 0);
    }

    /// `if (self.health > 100) self.health = 100;` and
    /// `if (self.health < 50) self.health = 50;`. Super health does not travel
    /// and a nearly dead player is not sent into the next level to die.
    #[test]
    fn a_map_change_caps_health_at_a_hundred_and_floors_it_at_fifty() {
        let mut weapon = WeaponState::new();
        weapon.apply_pickup(Pickup::Health {
            amount: 100,
            maximum: 250,
        });
        assert!(weapon.inventory().health() > PLAYER_MAX_HEALTH);
        weapon.map_loaded();
        assert_eq!(weapon.inventory().health(), PLAYER_MAX_HEALTH);

        let mut weapon = WeaponState::new();
        weapon.take_damage(97);
        assert_eq!(weapon.inventory().health(), 3);
        weapon.map_loaded();
        assert_eq!(weapon.inventory().health(), CHANGE_LEVEL_MIN_HEALTH);

        // A health already between the two bounds is untouched.
        let mut weapon = WeaponState::new();
        weapon.take_damage(30);
        weapon.map_loaded();
        assert_eq!(weapon.inventory().health(), 70);
    }

    /// `parm4 = self.ammo_shells < 25 ? 25 : self.ammo_shells;`, so the next
    /// level always starts with at least a full shotgun load, and a player who
    /// hoarded more keeps all of it.
    #[test]
    fn a_map_change_floors_shells_at_twenty_five() {
        let mut weapon = WeaponState::new();
        for _ in 0..20 {
            assert!(weapon.inventory_mut_for_test().consume(AmmoKind::Shells, 1));
        }
        assert_eq!(weapon.inventory().ammo(AmmoKind::Shells), 5);
        weapon.map_loaded();
        assert_eq!(
            weapon.inventory().ammo(AmmoKind::Shells),
            SHOTGUN_STARTING_SHELLS
        );

        let mut weapon = WeaponState::new();
        weapon.apply_pickup(Pickup::Ammo {
            kind: AmmoKind::Shells,
            amount: 40,
        });
        let hoard = weapon.inventory().ammo(AmmoKind::Shells);
        assert!(hoard > SHOTGUN_STARTING_SHELLS);
        weapon.map_loaded();
        assert_eq!(weapon.inventory().ammo(AmmoKind::Shells), hoard);
    }

    /// `SetChangeParms` is the one place the original strips artifacts.
    #[test]
    fn a_map_change_drops_every_artifact() {
        let mut weapon = WeaponState::new();
        for kind in PowerupKind::ALL {
            weapon.apply_pickup(Pickup::Powerup { kind });
            assert!(weapon.inventory().powerups().active(kind));
        }
        weapon.map_loaded();
        for kind in PowerupKind::ALL {
            assert!(!weapon.inventory().powerups().active(kind));
        }
    }

    #[test]
    fn the_quad_quadruples_every_carried_attack_damage() {
        let cases: &[(Option<u8>, i16, i16)] = &[
            (None, AXE_DAMAGE, AXE_DAMAGE * 4),
            (
                Some(0x58),
                SHOTGUN_DAMAGE_PER_PELLET,
                SHOTGUN_DAMAGE_PER_PELLET * 4,
            ),
            (Some(0x55), NAIL_DAMAGE, NAIL_DAMAGE * 4),
            (Some(0x53), GRENADE_DAMAGE, GRENADE_DAMAGE * 4),
            (Some(0x54), LIGHTNING_DAMAGE, LIGHTNING_DAMAGE * 4),
        ];
        for &(class_name, plain, quadded) in cases {
            let damage = |quad: bool| {
                let mut state = WeaponState::new();
                match class_name {
                    Some(class_name) => {
                        state.apply_pickup(pickup_for_entity(class_name, 0).expect("weapon"));
                    }
                    None => assert!(state.select(Weapon::Axe)),
                }
                if quad {
                    state.apply_pickup(Pickup::Powerup {
                        kind: PowerupKind::Quad,
                    });
                }
                match state
                    .try_attack(true, ORIGIN, [0, 0, 0], 7)
                    .expect("attack")
                {
                    WeaponAttack::Axe(attack) => attack.damage,
                    WeaponAttack::Shotgun(attack) => attack.damage_per_pellet,
                    WeaponAttack::Nail(spawn) => spawn.damage,
                    WeaponAttack::Grenade(spawn) => spawn.damage,
                    WeaponAttack::Rocket(spawn) => spawn.direct_damage,
                    WeaponAttack::Lightning(attack) => attack.damage,
                    WeaponAttack::LightningDischarge(discharge) => discharge.damage,
                }
            };
            assert_eq!(damage(false), plain);
            assert_eq!(damage(true), quadded);
        }
    }

    #[test]
    fn the_quad_expires_and_stops_quadrupling() {
        let mut state = WeaponState::new();
        assert!(state.select(Weapon::Axe));
        state.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Quad,
        });
        let quadded = state
            .try_attack(true, ORIGIN, [0, 0, 0], 7)
            .expect("attack");
        assert!(matches!(quadded, WeaponAttack::Axe(attack) if attack.damage == AXE_DAMAGE * 4));
        for _ in 0..(crate::survival::POWERUP_TICKS / 4 + 1) {
            state.tick(4);
            state.tick_survival(SurvivalInput {
                elapsed_ticks: 4,
                ..SurvivalInput::default()
            });
        }
        assert!(!state.inventory().powerups().active(PowerupKind::Quad));
        let plain = state
            .try_attack(true, ORIGIN, [0, 0, 0], 7)
            .expect("attack");
        assert!(matches!(plain, WeaponAttack::Axe(attack) if attack.damage == AXE_DAMAGE));
    }

    #[test]
    fn monster_profiles_match_original_runtime_setsize_and_health() {
        const CASES: &[(u8, [i16; 3], [i16; 3], i16)] = &[
            (0x36, [-16, -16, -24], [16, 16, 40], 30),
            (0x38, [-32, -32, -24], [32, 32, 64], 300),
            (0x39, [-32, -32, -24], [32, 32, 40], 25),
            (0x3a, [-16, -16, -24], [16, 16, 40], 80),
            (0x3b, [-16, -16, -24], [16, 16, 24], 25),
            (0x3c, [-16, -16, -24], [16, 16, 40], 250),
            (0x3d, [-16, -16, -24], [16, 16, 40], 75),
            (0x3e, [-32, -32, -24], [32, 32, 64], 200),
            (0x40, [-32, -32, -24], [32, 32, 64], 400),
            (0x41, [-32, -32, -24], [32, 32, 64], 600),
            (0x42, [-16, -16, -24], [16, 16, 40], 80),
            (0x43, [-16, -16, -24], [16, 16, 40], 80),
        ];
        for &(class_name, mins, maxs, health) in CASES {
            assert_eq!(
                monster_profile(class_name, 0),
                Some(MonsterProfile {
                    mins: Vec3I16 {
                        x: mins[0],
                        y: mins[1],
                        z: mins[2],
                    },
                    maxs: Vec3I16 {
                        x: maxs[0],
                        y: maxs[1],
                        z: maxs[2],
                    },
                    health,
                }),
                "class 0x{class_name:02x}",
            );
        }
    }

    #[test]
    fn monster_profiles_honor_authored_health_and_exclude_bespoke_classes() {
        assert_eq!(monster_profile(0x36, 123).unwrap().health, 123);
        // The zombie takes damage but only a gib-level blow kills it.
        assert_eq!(monster_profile(0x44, 0).unwrap().health, 60);
        // Chthon carries a body box for blocking and tracing; the scene never
        // marks him damageable, so his health is his shock count.
        let boss = monster_profile(0x37, 0).expect("Chthon has a body box");
        assert_eq!(boss.health, 3);
        assert_eq!(
            (boss.mins.x, boss.maxs.z),
            (-128, 256),
            "Chthon's authored setsize"
        );
        assert!(monster_profile(0x3f, 0).is_none()); // the Old One
        assert!(monster_profile(0xff, 0).is_none());
    }

    #[test]
    fn held_shotgun_obeys_quake_refire_and_preserves_ammo_across_ticks() {
        let mut weapon = WeaponState::new();
        let first = weapon
            .try_fire(true, ORIGIN, [0, 0, 0], 7)
            .expect("first shot");
        assert_eq!(first.sequence, 0);
        assert_eq!(weapon.shells(), 24);
        assert_eq!(weapon.view().frame, 1);
        for _ in 0..29 {
            weapon.tick(1);
            assert!(weapon.try_fire(true, ORIGIN, [0, 0, 0], 7).is_none());
        }
        weapon.tick(1);
        assert_eq!(
            weapon
                .try_fire(true, ORIGIN, [0, 0, 0], 7)
                .unwrap()
                .sequence,
            1
        );
        assert_eq!(weapon.shells(), 23);
    }

    #[test]
    fn pellet_pattern_is_reproducible_and_spreads_around_forward() {
        let mut left = WeaponState::new();
        let mut right = WeaponState::new();
        let a = left.try_fire(true, ORIGIN, [0, 0, 0], 7).unwrap();
        let b = right.try_fire(true, ORIGIN, [0, 0, 0], 7).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.start,
            Vec3I32 {
                x: 40_960,
                y: 0,
                z: -28_672
            }
        );
        let ends = &a.ends[..a.pellet_count as usize];
        assert_eq!(ends.len(), SHOTGUN_PELLETS);
        assert!(ends.iter().all(|end| end.x > 2_000 << 12));
        assert!(ends.iter().any(|end| end.y != 0));
        assert!(ends.iter().any(|end| end.z != a.start.z));
    }

    #[test]
    fn segment_box_fraction_handles_entry_inside_parallel_and_miss() {
        let mins = Vec3I32 {
            x: 4 << 12,
            y: -1 << 12,
            z: -1 << 12,
        };
        let maxs = Vec3I32 {
            x: 6 << 12,
            y: 1 << 12,
            z: 1 << 12,
        };
        assert_eq!(
            segment_aabb_fraction(
                ORIGIN,
                Vec3I32 {
                    x: 8 << 12,
                    y: 0,
                    z: 0
                },
                mins,
                maxs
            ),
            Some(2_048)
        );
        assert_eq!(
            segment_aabb_fraction(
                Vec3I32 {
                    x: 5 << 12,
                    y: 0,
                    z: 0
                },
                Vec3I32 {
                    x: 8 << 12,
                    y: 0,
                    z: 0
                },
                mins,
                maxs,
            ),
            Some(0)
        );
        assert_eq!(
            segment_aabb_fraction(
                ORIGIN,
                Vec3I32 {
                    x: 8 << 12,
                    y: 3 << 12,
                    z: 0
                },
                mins,
                maxs
            ),
            None
        );
        assert_eq!(
            segment_aabb_fraction(
                Vec3I32 {
                    x: 5 << 12,
                    y: 2 << 12,
                    z: 0
                },
                Vec3I32 {
                    x: 8 << 12,
                    y: 2 << 12,
                    z: 0
                },
                mins,
                maxs,
            ),
            None
        );
    }

    #[test]
    fn segment_box_fraction_handles_full_i32_endpoint_span_without_wide_division() {
        let mins = Vec3I32 {
            x: -1 << 12,
            y: -1 << 12,
            z: -1 << 12,
        };
        let maxs = Vec3I32 {
            x: 1 << 12,
            y: 1 << 12,
            z: 1 << 12,
        };
        assert_eq!(
            segment_aabb_fraction(
                Vec3I32 {
                    x: i32::MIN,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: i32::MAX,
                    y: 0,
                    z: 0,
                },
                mins,
                maxs,
            ),
            Some(2_047)
        );
        assert_eq!(
            segment_aabb_fraction(
                Vec3I32 {
                    x: i32::MAX,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: i32::MIN,
                    y: 0,
                    z: 0,
                },
                mins,
                maxs,
            ),
            Some(2_047)
        );
    }

    #[test]
    fn segment_box_fraction_saturates_out_of_interval_extreme_ratios() {
        let near_max = Vec3I32 {
            x: i32::MAX - (5 << 12),
            y: -1 << 12,
            z: -1 << 12,
        };
        let max = Vec3I32 {
            x: i32::MAX,
            y: 1 << 12,
            z: 1 << 12,
        };
        assert_eq!(
            segment_aabb_fraction(
                Vec3I32 {
                    x: i32::MIN,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: i32::MIN + (10 << 12),
                    y: 0,
                    z: 0,
                },
                near_max,
                max,
            ),
            None
        );
    }

    #[test]
    fn cooked_pickups_update_persistent_health_armor_keys_and_ammo() {
        let mut inventory = Inventory::new();
        assert_eq!(inventory.take_damage(40), 40);
        assert_eq!(inventory.health(), 60);
        assert!(
            inventory
                .apply_pickup(pickup_for_entity(0x22, 0).unwrap())
                .consumed
        );
        assert_eq!(inventory.health(), 85);
        assert!(
            inventory
                .apply_pickup(pickup_for_entity(0x1a, 0).unwrap())
                .consumed
        );
        assert_eq!(inventory.take_damage(50), 35);
        assert_eq!(inventory.armor(), 85);
        assert_eq!(inventory.health(), 50);
        assert!(
            inventory
                .apply_pickup(pickup_for_entity(0x23, 0).unwrap())
                .consumed
        );
        assert_eq!(inventory.keys(), 1);
        assert!(
            !inventory
                .apply_pickup(pickup_for_entity(0x23, 0).unwrap())
                .consumed
        );
        assert!(
            inventory
                .apply_pickup(pickup_for_entity(0x25, 1).unwrap())
                .consumed
        );
        assert_eq!(inventory.ammo(AmmoKind::Rockets), 10);

        let transitioned = inventory;
        assert_eq!(transitioned.health(), 50);
        assert_eq!(transitioned.armor(), 85);
        assert_eq!(transitioned.keys(), 1);
        assert_eq!(transitioned.ammo(AmmoKind::Rockets), 10);
    }

    #[test]
    fn armor_absorption_rounds_up_like_quake() {
        let mut inventory = Inventory::new();
        assert!(
            inventory
                .apply_pickup(pickup_for_entity(0x1a, 0).unwrap())
                .consumed
        );
        assert_eq!(inventory.take_damage(1), 0);
        assert_eq!(inventory.armor(), 99);
        assert_eq!(inventory.take_damage(4), 2);
        assert_eq!(inventory.armor(), 97);
        assert_eq!(inventory.health(), 98);
    }

    #[test]
    fn empty_weapon_selection_is_gated_and_held_fire_falls_back_to_shotgun() {
        let mut state = WeaponState::new();
        assert!(
            state
                .apply_pickup(pickup_for_entity(0x56, 0).unwrap())
                .switched_weapon
        );
        assert_eq!(state.active_weapon(), Weapon::RocketLauncher);
        for (shot, expected_damage) in [105, 113, 119, 107, 118].into_iter().enumerate() {
            let attack = state
                .try_attack(true, ORIGIN, [0, 0, 0], 7)
                .expect("rocket shot");
            let WeaponAttack::Rocket(rocket) = attack else {
                panic!("wrong attack");
            };
            assert_eq!(rocket.step.x, ROCKET_STEP_Q12);
            assert_eq!(rocket.direct_damage, expected_damage);
            assert_eq!(state.inventory().ammo(AmmoKind::Rockets), 4 - shot as u16);
            for _ in 0..ROCKET_REFIRE_TICKS {
                state.tick(1);
            }
        }
        assert!(!state.select(Weapon::RocketLauncher));
        assert!(matches!(
            state.try_attack(true, ORIGIN, [0, 0, 0], 5),
            Some(WeaponAttack::Shotgun(_))
        ));
        assert_eq!(state.active_weapon(), Weapon::Shotgun);
        for _ in 0..SHOTGUN_REFIRE_TICKS {
            state.tick(1);
        }
        assert_eq!(state.cycle(false), Weapon::Axe);
    }

    #[test]
    fn switching_weapons_cannot_bypass_the_active_refire_clock() {
        let mut state = WeaponState::new();
        assert!(matches!(
            state.try_attack(true, ORIGIN, [0, 0, 0], 7),
            Some(WeaponAttack::Shotgun(_))
        ));
        assert!(
            state
                .apply_pickup(pickup_for_entity(0x56, 0).unwrap())
                .switched_weapon
        );
        assert!(state.try_attack(true, ORIGIN, [0, 0, 0], 7).is_none());
        for _ in 0..SHOTGUN_REFIRE_TICKS {
            state.tick(1);
        }
        assert!(matches!(
            state.try_attack(true, ORIGIN, [0, 0, 0], 7),
            Some(WeaponAttack::Rocket(_))
        ));
    }

    #[test]
    fn authored_weapon_pickups_grant_quake_ammo_and_select_the_weapon() {
        let cases = [
            (0x53, Weapon::GrenadeLauncher, AmmoKind::Rockets, 5),
            (0x54, Weapon::Lightning, AmmoKind::Cells, 15),
            (0x55, Weapon::Nailgun, AmmoKind::Nails, 30),
            (0x56, Weapon::RocketLauncher, AmmoKind::Rockets, 5),
            (0x57, Weapon::SuperNailgun, AmmoKind::Nails, 30),
            (0x58, Weapon::SuperShotgun, AmmoKind::Shells, 5),
        ];
        for (class_name, expected_weapon, ammo, amount) in cases {
            let mut inventory = Inventory::new();
            let before = inventory.ammo(ammo);
            let outcome = inventory.apply_pickup(pickup_for_entity(class_name, 0).unwrap());
            assert!(outcome.consumed);
            assert!(inventory.owns(expected_weapon));
            assert_eq!(inventory.active_weapon(), expected_weapon);
            assert_eq!(inventory.ammo(ammo), before + amount);
        }
    }

    #[test]
    fn firing_presentation_matches_original_quake_policy() {
        let mut state = WeaponState::new();
        let shotgun = state
            .try_attack(true, ORIGIN, [0, 0, 0], 7)
            .expect("shotgun attack");
        assert_eq!(shotgun.recoil_pitch(), -2);
        assert!(shotgun.muzzle_flashes());

        let mut state = WeaponState::new();
        assert!(state.select(Weapon::Axe));
        let axe = state
            .try_attack(true, ORIGIN, [0, 0, 0], 7)
            .expect("axe attack");
        assert_eq!(axe.recoil_pitch(), 0);
        assert!(!axe.muzzle_flashes());

        for (entity, expected_recoil) in [
            (0x58, -4), // Super Shotgun
            (0x55, -2), // Nailgun
            (0x57, -2), // Super Nailgun
            (0x53, -2), // Grenade Launcher
            (0x56, -2), // Rocket Launcher
            (0x54, -2), // Thunderbolt
        ] {
            let mut state = WeaponState::new();
            state.apply_pickup(pickup_for_entity(entity, 0).unwrap());
            let attack = state
                .try_attack_in_water(true, ORIGIN, [0, 0, 0], 7, 0)
                .expect("owned weapon attack");
            assert_eq!(attack.recoil_pitch(), expected_recoil);
            assert!(attack.muzzle_flashes());
        }

        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x54, 0).unwrap());
        let discharge = state
            .try_attack_in_water(true, ORIGIN, [0, 0, 0], 7, 2)
            .expect("underwater Thunderbolt discharge");
        assert!(matches!(discharge, WeaponAttack::LightningDischarge(_)));
        assert_eq!(discharge.recoil_pitch(), 0);
        assert!(discharge.muzzle_flashes());
    }

    #[test]
    fn complete_arsenal_consumes_ammo_and_emits_authored_attack_policy() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x58, 0).unwrap());
        let WeaponAttack::Shotgun(super_shotgun) =
            state.try_attack(true, ORIGIN, [0, 0, 0], 7).unwrap()
        else {
            panic!("super shotgun attack");
        };
        assert_eq!(super_shotgun.pellet_count as usize, SUPER_SHOTGUN_PELLETS);
        assert_eq!(super_shotgun.sound_id, 0xce);
        assert_eq!(state.inventory().ammo(AmmoKind::Shells), 28);

        for _ in 0..SUPER_SHOTGUN_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x55, 0).unwrap());
        let WeaponAttack::Nail(nail) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap() else {
            panic!("nailgun attack");
        };
        assert_eq!(nail.damage, NAIL_DAMAGE);
        assert_eq!(nail.sound_id, 0xcc);
        assert_eq!(state.inventory().ammo(AmmoKind::Nails), 29);

        for _ in 0..NAIL_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x57, 0).unwrap());
        let WeaponAttack::Nail(super_nail) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap()
        else {
            panic!("super nailgun attack");
        };
        assert_eq!(super_nail.damage, SUPER_NAIL_DAMAGE);
        assert_eq!(super_nail.sound_id, 0xcf);
        assert_eq!(state.inventory().ammo(AmmoKind::Nails), 57);

        for _ in 0..NAIL_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x53, 0).unwrap());
        let WeaponAttack::Grenade(grenade) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap()
        else {
            panic!("grenade attack");
        };
        assert_eq!(grenade.damage, GRENADE_DAMAGE);
        assert_eq!(grenade.lifetime_ticks, GRENADE_LIFETIME_TICKS);
        assert_eq!(state.inventory().ammo(AmmoKind::Rockets), 4);

        for _ in 0..GRENADE_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x54, 0).unwrap());
        let WeaponAttack::Lightning(lightning) = state
            .try_attack_in_water(true, ORIGIN, [0, 0, 0], 6, 0)
            .unwrap()
        else {
            panic!("lightning attack");
        };
        assert_eq!(lightning.damage, LIGHTNING_DAMAGE);
        assert_eq!(state.inventory().ammo(AmmoKind::Cells), 14);

        for _ in 0..LIGHTNING_REFIRE_TICKS {
            state.tick(1);
        }
        let WeaponAttack::LightningDischarge(discharge) = state
            .try_attack_in_water(true, ORIGIN, [0, 0, 0], 6, 2)
            .unwrap()
        else {
            panic!("underwater lightning discharge");
        };
        assert_eq!(discharge.damage, 14 * 35);
        assert_eq!(state.inventory().ammo(AmmoKind::Cells), 0);
    }

    #[optimize(size)]
    #[test]
    fn original_developer_cheats_cover_damage_and_the_whole_arsenal() {
        let mut state = WeaponState::new();
        let active = state.active_weapon();
        state.set_god_mode(true);
        assert!(state.god_mode());
        assert_eq!(state.take_damage(32_000), 0);
        assert_eq!(state.inventory().health(), 100);

        state.impulse_nine();
        let inventory = state.inventory();
        assert_eq!(inventory.owned_weapons(), u8::MAX);
        assert_eq!(inventory.ammo(AmmoKind::Shells), 100);
        assert_eq!(inventory.ammo(AmmoKind::Nails), 200);
        assert_eq!(inventory.ammo(AmmoKind::Rockets), 100);
        assert_eq!(inventory.ammo(AmmoKind::Cells), 100);
        assert_eq!(inventory.keys(), 0);
        assert_eq!(state.active_weapon(), active);

        state.set_god_mode(false);
        assert_eq!(state.take_damage(10), 10);
        assert_eq!(state.inventory().health(), 90);
    }

    #[test]
    fn low_ammo_super_weapons_use_their_single_ammo_fallbacks() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x58, 0).unwrap());
        state.inventory.ammo[AmmoKind::Shells.index()] = 1;
        let WeaponAttack::Shotgun(shot) = state.try_attack(true, ORIGIN, [0, 0, 0], 7).unwrap()
        else {
            panic!("shotgun fallback");
        };
        assert_eq!(state.active_weapon(), Weapon::Shotgun);
        assert_eq!(shot.pellet_count as usize, SHOTGUN_PELLETS);

        for _ in 0..SHOTGUN_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x57, 0).unwrap());
        state.inventory.ammo[AmmoKind::Nails.index()] = 1;
        let WeaponAttack::Nail(nail) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap() else {
            panic!("single super nail fallback");
        };
        assert_eq!(nail.damage, NAIL_DAMAGE);
        assert_eq!(nail.sound_id, 0xcf);
        assert_eq!(state.inventory().ammo(AmmoKind::Nails), 0);
    }

    #[test]
    fn sustained_lightning_uses_hit_sound_only_when_frame_one_cycles() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x54, 0).unwrap());
        let first = state
            .try_attack_in_water(true, ORIGIN, [0, 0, 0], 6, 0)
            .unwrap();
        assert_eq!(first.sound_id(), Some(0xc6));
        assert_eq!(state.view().frame, 1);
        for (expected_sound, expected_frame) in [
            (Some(0xc4), 2),
            (None, 3),
            (None, 4),
            (None, 5),
            (None, 1),
            (Some(0xc4), 2),
        ] {
            for _ in 0..LIGHTNING_REFIRE_TICKS {
                state.tick(1);
            }
            let sustained = state
                .try_attack_in_water(true, ORIGIN, [0, 0, 0], 6, 0)
                .unwrap();
            assert_eq!(sustained.sound_id(), expected_sound);
            assert_eq!(state.view().frame, expected_frame);
        }
        assert!(state
            .try_attack_in_water(false, ORIGIN, [0, 0, 0], 6, 0)
            .is_none());
        for _ in 0..LIGHTNING_REFIRE_TICKS {
            state.tick(1);
        }
        let restarted = state
            .try_attack_in_water(true, ORIGIN, [0, 0, 0], 6, 0)
            .unwrap();
        assert_eq!(restarted.sound_id(), Some(0xc6));
    }

    #[test]
    fn trigger_release_resets_sustained_phase_without_cancelling_attack_animation() {
        let mut state = WeaponState::new();
        assert!(matches!(
            state.try_attack(true, ORIGIN, [0, 0, 0], 7),
            Some(WeaponAttack::Shotgun(_))
        ));
        let view = state.view();
        assert!(state.try_attack(false, ORIGIN, [0, 0, 0], 7).is_none());
        assert_eq!(state.view(), view);
        assert_eq!(state.sustained_weapon, None);
        assert!(state.nail_next_positive);
    }

    #[test]
    fn nail_pool_capacity_covers_a_full_lifetime_and_denial_is_transactional() {
        assert_eq!(NAIL_POOL_CAPACITY, 60);
        let mut slots = [0u16; NAIL_POOL_CAPACITY];
        let mut fired = 0usize;
        for tick in 0..=NAIL_LIFETIME_TICKS.saturating_mul(2) {
            if tick != 0 {
                for remaining in &mut slots {
                    if *remaining != 0 {
                        let _ = projectile_expires_this_tick(remaining);
                    }
                }
            }
            if tick % NAIL_REFIRE_TICKS == 0 {
                let slot = slots
                    .iter_mut()
                    .find(|remaining| **remaining == 0)
                    .expect("standard sustained fire must always find a slot");
                *slot = NAIL_LIFETIME_TICKS;
                fired += 1;
            }
        }
        assert_eq!(fired, 121);

        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x55, 0).unwrap());
        state.inventory.ammo[AmmoKind::Nails.index()] = 100;
        for _ in 0..NAIL_POOL_CAPACITY {
            assert!(matches!(
                state.try_attack_in_water_with_admission(
                    true,
                    ORIGIN,
                    [0, 0, 0],
                    6,
                    0,
                    AttackAdmission::ALL,
                ),
                Some(WeaponAttack::Nail(_))
            ));
            for _ in 0..NAIL_REFIRE_TICKS {
                state.tick(1);
            }
        }
        let before = state;
        assert!(state
            .try_attack_in_water_with_admission(
                true,
                ORIGIN,
                [0, 0, 0],
                6,
                0,
                AttackAdmission {
                    nail: false,
                    ..AttackAdmission::ALL
                },
            )
            .is_none());
        assert_eq!(state, before);
    }

    #[test]
    fn underwater_empty_fallback_excludes_lightning_and_uses_new_model_frames() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x54, 0).unwrap());
        state.apply_pickup(pickup_for_entity(0x56, 0).unwrap());
        state.inventory.ammo[AmmoKind::Cells.index()] = 0;
        state.inventory.active_weapon = Weapon::Lightning;

        let prepared = state.attack_weapon(true, 2);
        assert_eq!(prepared, Weapon::RocketLauncher);
        assert!(matches!(
            state.try_attack_in_water(true, ORIGIN, [0, 0, 0], 7, 2),
            Some(WeaponAttack::Rocket(_))
        ));
        assert_eq!(state.view_frame_end, 6);
    }

    #[test]
    fn full_pool_denial_does_not_commit_an_empty_weapon_fallback() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x56, 0).unwrap());
        state.apply_pickup(pickup_for_entity(0x55, 0).unwrap());
        state.inventory.ammo[AmmoKind::Rockets.index()] = 0;
        state.inventory.active_weapon = Weapon::RocketLauncher;
        let before = state;

        assert_eq!(state.attack_weapon(true, 0), Weapon::Nailgun);
        assert!(state
            .try_attack_in_water_with_admission(
                true,
                ORIGIN,
                [0, 0, 0],
                6,
                0,
                AttackAdmission {
                    nail: false,
                    ..AttackAdmission::ALL
                },
            )
            .is_none());
        assert_eq!(state, before);
    }

    #[test]
    fn lightning_side_rays_share_the_world_clipped_center_endpoint() {
        let attack = LightningAttack {
            beam_start: ORIGIN,
            start: ORIGIN,
            end: Vec3I32 {
                x: LIGHTNING_RANGE_UNITS << 12,
                y: 0,
                z: 0,
            },
            forward: Vec3I32 {
                x: Q12_ONE,
                y: 0,
                z: 0,
            },
            right: Vec3I32 {
                x: 0,
                y: Q12_ONE,
                z: 0,
            },
            damage: LIGHTNING_DAMAGE,
            sound_id: None,
        };
        let geometry = lightning_trace_geometry(attack, Q12_ONE / 2);
        let clipped_x = (LIGHTNING_RANGE_UNITS / 2 + 4) << 12;
        assert_eq!(geometry.beam_start, ORIGIN);
        assert_eq!(geometry.beam_end.x, clipped_x);
        assert_eq!(geometry.ends[0].x, clipped_x);
        assert_eq!(geometry.ends[1].x, clipped_x);
        assert_eq!(geometry.ends[2].x, clipped_x);
        assert_eq!(geometry.ends[1].y, 16 << 12);
        assert_eq!(geometry.ends[2].y, -(16 << 12));
    }

    #[test]
    fn grenade_rest_threshold_zeros_linear_and_angular_motion_exactly() {
        let moving = Vec3I32 {
            x: 10,
            y: -20,
            z: GRENADE_REST_Z_STEP_Q12 - 1,
        };
        assert!(!grenade_should_rest(moving, GRENADE_FLOOR_NORMAL_Z_Q12));
        assert!(grenade_should_rest(moving, GRENADE_FLOOR_NORMAL_Z_Q12 + 1));
        assert!(!grenade_should_rest(
            Vec3I32 {
                z: GRENADE_REST_Z_STEP_Q12,
                ..moving
            },
            GRENADE_FLOOR_NORMAL_Z_Q12 + 1
        ));
        let mut velocity = moving;
        let mut angular = Vec3I16 { x: 57, y: 3, z: -4 };
        assert!(settle_grenade_motion(
            &mut velocity,
            &mut angular,
            GRENADE_FLOOR_NORMAL_Z_Q12 + 1,
        ));
        assert_eq!(velocity, Vec3I32::default());
        assert_eq!(angular, Vec3I16::default());

        let mut fuse = 3;
        assert_eq!(grenade_tick(&mut fuse, true), GrenadeTick::Rest);
        assert_eq!(grenade_tick(&mut fuse, true), GrenadeTick::Rest);
        assert_eq!(grenade_tick(&mut fuse, true), GrenadeTick::Explode);
        assert_eq!(fuse, 0);
    }

    #[test]
    fn projectile_expiry_and_sky_policies_match_preserved_weapon_classes() {
        let mut remaining = 3;
        assert!(!projectile_expires_this_tick(&mut remaining));
        assert!(!projectile_expires_this_tick(&mut remaining));
        assert!(projectile_expires_this_tick(&mut remaining));
        assert_eq!(remaining, 0);
        assert!(projectile_expires_this_tick(&mut remaining));
        assert_eq!(
            projectile_sky_impact(ProjectileKind::Rocket),
            SkyImpact::RemoveSilently
        );
        assert_eq!(
            projectile_sky_impact(ProjectileKind::Nail),
            SkyImpact::Impact
        );
        assert_eq!(
            projectile_sky_impact(ProjectileKind::Grenade),
            SkyImpact::Bounce
        );
    }

    #[test]
    fn nail_barrel_phase_is_weapon_local_cycles_view_and_resets_on_switch() {
        let mut state = WeaponState::new();
        assert!(matches!(
            state.try_attack(true, ORIGIN, [0, 0, 0], 7),
            Some(WeaponAttack::Shotgun(_))
        ));
        for _ in 0..SHOTGUN_REFIRE_TICKS {
            state.tick(1);
        }
        state.apply_pickup(pickup_for_entity(0x55, 0).unwrap());
        let (_, right, _) = view_basis([0, 0, 0]);
        let base = Vec3I32 {
            x: 0,
            y: 0,
            z: -(6 << 12),
        };
        let expected_positive = add_combat_vec(
            base,
            Vec3I32 {
                x: right.x * 4,
                y: right.y * 4,
                z: right.z * 4,
            },
        );
        let expected_negative = sub_combat_vec(
            base,
            Vec3I32 {
                x: right.x * 4,
                y: right.y * 4,
                z: right.z * 4,
            },
        );
        let WeaponAttack::Nail(first) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap()
        else {
            panic!("first nail");
        };
        assert_eq!(first.origin, expected_positive);
        assert_eq!(state.view().frame, 1);
        for _ in 0..NAIL_REFIRE_TICKS {
            state.tick(1);
        }
        let WeaponAttack::Nail(second) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap()
        else {
            panic!("second nail");
        };
        assert_eq!(second.origin, expected_negative);
        assert_eq!(state.view().frame, 2);

        assert!(state.select(Weapon::Shotgun));
        assert!(state.select(Weapon::Nailgun));
        for _ in 0..NAIL_REFIRE_TICKS {
            state.tick(1);
        }
        let WeaponAttack::Nail(reset) = state.try_attack(true, ORIGIN, [0, 0, 0], 6).unwrap()
        else {
            panic!("reset nail");
        };
        assert_eq!(reset.origin, expected_positive);
    }

    #[test]
    fn map_load_resets_transient_weapon_state_but_preserves_inventory() {
        let mut state = WeaponState::new();
        state.apply_pickup(pickup_for_entity(0x56, 0).unwrap());
        let first = state.try_attack(true, ORIGIN, [0, 0, 0], 7).unwrap();
        assert!(matches!(first, WeaponAttack::Rocket(_)));
        let inventory = state.inventory();
        let shots = state.shots_fired();

        state.map_loaded();
        assert_eq!(state.inventory(), inventory);
        assert_eq!(state.shots_fired(), shots);
        assert_eq!(state.refire_ticks, 0);
        assert_eq!(state.view().frame, 0);
        let WeaponAttack::Rocket(after_load) =
            state.try_attack(true, ORIGIN, [0, 0, 0], 7).unwrap()
        else {
            panic!("rocket after load");
        };
        assert_eq!(after_load.direct_damage, 105);
    }

    #[test]
    fn rocket_splash_requires_visibility_and_halves_self_damage() {
        // Falloff is half the distance: 120 damage reaches 240 units.
        assert_eq!(rocket_splash_points(0, false, true), 120);
        assert_eq!(rocket_splash_points(120, false, true), 60);
        assert_eq!(rocket_splash_points(238, false, true), 1);
        assert_eq!(rocket_splash_points(240, false, true), 0);
        assert_eq!(rocket_splash_points(0, true, true), 60);
        assert_eq!(rocket_splash_points(4, true, true), 59);
        assert_eq!(rocket_splash_points(236, true, true), 1);
        assert_eq!(rocket_splash_points(238, true, true), 0);
        assert_eq!(rocket_splash_points(240, true, true), 0);
        assert_eq!(rocket_splash_points(0, false, false), 0);
    }

    #[test]
    fn shambler_resistance_applies_to_direct_and_radius_explosion_damage() {
        assert_eq!(rocket_direct_points(100, false), 100);
        assert_eq!(rocket_direct_points(100, true), 50);
        assert_eq!(explosion_splash_points(120, 0, false, true, true), 60);
        assert_eq!(explosion_splash_points(120, 236, false, true, true), 1);
        assert_eq!(explosion_splash_points(120, 238, false, true, true), 0);
        assert_eq!(explosion_splash_points(120, 240, false, true, true), 0);
    }

    #[test]
    fn grenade_touch_target_remains_eligible_for_radius_damage() {
        assert!(ExplosionKind::Rocket.radius_ignores_direct_target());
        assert!(!ExplosionKind::Grenade.radius_ignores_direct_target());
        assert_eq!(
            explosion_splash_points(GRENADE_DAMAGE, 0, false, true, false),
            GRENADE_DAMAGE
        );
    }

    #[test]
    fn occluded_self_takes_no_rocket_splash() {
        assert_eq!(rocket_splash_points(16, true, false), 0);
    }

    #[test]
    fn zero_elapsed_ticks_do_not_advance_rockets() {
        assert_eq!(rocket_elapsed_ticks(0), 0);
        assert_eq!(rocket_elapsed_ticks(1), 1);
        assert_eq!(rocket_elapsed_ticks(8), 4);
    }
}
