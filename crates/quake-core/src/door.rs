//! Original `func_door` key locks and bounding-contact linking.

use quake_formats::Vec3I32;

/// Spawn in the open position.
pub const DOOR_START_OPEN: u16 = 1;
/// Never link to a touching neighbour.
pub const DOOR_DONT_LINK: u16 = 4;
/// `DOOR_GOLD_KEY`: the original assigns `items = IT_KEY2`.
pub const DOOR_GOLD_KEY: u16 = 8;
/// `DOOR_SILVER_KEY`: the original assigns `items = IT_KEY1`.
pub const DOOR_SILVER_KEY: u16 = 16;
/// Stay open until used again.
pub const DOOR_TOGGLE: u16 = 32;

/// `func_door_secret` bit 1: `SECRET_OPEN_ONCE`, the door never returns after
/// its first opening. Secret doors do not read the `func_door` flags above.
pub const SECRET_OPEN_ONCE: u16 = 1;
/// `SECRET_1ST_LEFT`: the sideways leg goes left instead of right.
pub const SECRET_1ST_LEFT: u16 = 2;
/// `SECRET_1ST_DOWN`: the first leg drops by the door's own height instead of
/// stepping sideways by its width.
pub const SECRET_1ST_DOWN: u16 = 4;

/// `IT_KEY1`, the silver key carried by `item_key1`.
pub const KEY_SILVER_BIT: u8 = 1;
/// `IT_KEY2`, the gold key carried by `item_key2`.
pub const KEY_GOLD_BIT: u8 = 2;

/// `door_touch` sets `attack_finished = time + 2` before every key report.
pub const DOOR_KEY_RETRY_TICKS: u16 = 120;
/// `if (!self.dmg) self.dmg = 2`.
pub const DOOR_DEFAULT_DAMAGE: i16 = 2;
/// This port has no pusher push-back, so a blocked door damages on bounds
/// overlap and re-arms on the same half second a blocked train uses.
pub const DOOR_BLOCK_COOLDOWN_TICKS: u16 = 30;

/// Inventory bit a door demands, or zero when it is not a key door.
///
/// The original runs two independent `if`s in this order, so a door carrying
/// both flags ends up demanding the silver key.
#[optimize(size)]
pub const fn door_key_bit(spawn_flags: u16) -> u8 {
    let mut bit = 0;
    if spawn_flags & DOOR_GOLD_KEY != 0 {
        bit = KEY_GOLD_BIT;
    }
    if spawn_flags & DOOR_SILVER_KEY != 0 {
        bit = KEY_SILVER_BIT;
    }
    bit
}

/// What `door_touch` does with the toucher's inventory.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DoorKeyOutcome {
    /// Not a key door; the ordinary door rules apply.
    NotLocked,
    /// The key was held. The original removes it from the toucher
    /// (`other.items = other.items - self.items`), disarms the door's touch,
    /// and opens the whole linked chain.
    Opened { consumed_bit: u8 },
    /// The key was missing. The door stays shut and reports which one.
    Refused { needed_bit: u8 },
}

/// `door_touch`'s key half.
#[optimize(size)]
pub const fn door_touch_key(spawn_flags: u16, held_keys: u8) -> DoorKeyOutcome {
    let bit = door_key_bit(spawn_flags);
    if bit == 0 {
        return DoorKeyOutcome::NotLocked;
    }
    if held_keys & bit == bit {
        DoorKeyOutcome::Opened { consumed_bit: bit }
    } else {
        DoorKeyOutcome::Refused { needed_bit: bit }
    }
}

/// Centerprint text for a refused key door.
///
/// The original also has worldtype-specific "silver keycard" and "silver
/// runekey" wording. The cooked runtime map does not carry `worldtype`, so
/// this port always prints the medieval phrasing.
#[optimize(size)]
pub const fn needs_key_message(bit: u8) -> &'static str {
    if bit == KEY_GOLD_BIT {
        "You need the gold key"
    } else {
        "You need the silver key"
    }
}

/// `EntitiesTouching`: an inclusive absolute-bounds overlap on all three axes.
#[optimize(size)]
pub const fn entities_touching(
    left_mins: Vec3I32,
    left_maxs: Vec3I32,
    right_mins: Vec3I32,
    right_maxs: Vec3I32,
) -> bool {
    left_mins.x <= right_maxs.x
        && left_maxs.x >= right_mins.x
        && left_mins.y <= right_maxs.y
        && left_maxs.y >= right_mins.y
        && left_mins.z <= right_maxs.z
        && left_maxs.z >= right_mins.z
}

/// `LinkDoors`: a door joins a neighbour's chain when their closed bounds
/// touch and neither one carries `DOOR_DONT_LINK`.
#[optimize(size)]
pub const fn doors_link(left_spawn_flags: u16, right_spawn_flags: u16) -> bool {
    left_spawn_flags & DOOR_DONT_LINK == 0 && right_spawn_flags & DOOR_DONT_LINK == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    fn point(x: i32) -> Vec3I32 {
        Vec3I32 { x, y: 0, z: 0 }
    }

    #[optimize(size)]
    #[test]
    fn spawnflag_16_is_the_silver_lock_and_8_is_the_gold_one() {
        // Confirmed against the cooked shareware maps: E1M2 authors item_key1
        // with spawnflag-16 doors and E1M3 authors item_key2 with
        // spawnflag-8 doors.
        assert_eq!(door_key_bit(DOOR_SILVER_KEY), KEY_SILVER_BIT);
        assert_eq!(door_key_bit(DOOR_GOLD_KEY), KEY_GOLD_BIT);
        assert_eq!(door_key_bit(0), 0);
        assert_eq!(door_key_bit(DOOR_TOGGLE | DOOR_START_OPEN), 0);
        assert_eq!(
            door_key_bit(DOOR_SILVER_KEY | DOOR_GOLD_KEY),
            KEY_SILVER_BIT
        );
    }

    #[optimize(size)]
    #[test]
    fn a_held_key_opens_the_door_and_is_consumed_exactly_like_winquake() {
        assert_eq!(
            door_touch_key(DOOR_SILVER_KEY, KEY_SILVER_BIT),
            DoorKeyOutcome::Opened {
                consumed_bit: KEY_SILVER_BIT
            }
        );
        assert_eq!(
            door_touch_key(DOOR_GOLD_KEY, KEY_SILVER_BIT | KEY_GOLD_BIT),
            DoorKeyOutcome::Opened {
                consumed_bit: KEY_GOLD_BIT
            }
        );
    }

    #[optimize(size)]
    #[test]
    fn a_missing_key_refuses_and_names_the_key_it_wants() {
        assert_eq!(
            door_touch_key(DOOR_SILVER_KEY, 0),
            DoorKeyOutcome::Refused {
                needed_bit: KEY_SILVER_BIT
            }
        );
        assert_eq!(
            door_touch_key(DOOR_GOLD_KEY, KEY_SILVER_BIT),
            DoorKeyOutcome::Refused {
                needed_bit: KEY_GOLD_BIT
            }
        );
        assert_eq!(door_touch_key(0, 0), DoorKeyOutcome::NotLocked);
        assert_eq!(needs_key_message(KEY_SILVER_BIT), "You need the silver key");
        assert_eq!(needs_key_message(KEY_GOLD_BIT), "You need the gold key");
    }

    #[optimize(size)]
    #[test]
    fn touching_bounds_link_and_dont_link_doors_never_do() {
        assert!(entities_touching(point(0), point(16), point(16), point(32)));
        assert!(!entities_touching(
            point(0),
            point(15),
            point(16),
            point(32)
        ));
        assert!(doors_link(0, DOOR_SILVER_KEY));
        assert!(!doors_link(DOOR_DONT_LINK, 0));
        assert!(!doors_link(0, DOOR_DONT_LINK));
    }
}
