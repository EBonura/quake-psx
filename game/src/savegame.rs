//! Memory-card save and load: three manual slots, one card file each.
//!
//! Saving is only ever the player's own action from the pause menu; nothing
//! here writes on its own. The save is the map, the skill and the runes in
//! hand, the player, the weapon state and the scene's moving parts
//! (`entity/save.rs`), framed by `quake_core::save` with a version and a
//! checksum, so a card from another build or a damaged one is refused rather
//! than misread.
//!
//! The code lives in the map loader's retained 30 KiB transfer buffer, which
//! nothing else touches while a menu action runs, so the feature holds almost
//! no resident code. Its three halves are overlay packages
//! (`monster_code.rs`): the encoder, the decoder and the card driver, each
//! installed in turn. The payload waits in a `STAGING_BYTES` buffer on the
//! caller's stack frame (`quake.rs`), which both packages read and write.
//! A load reads the card twice: the first read decides which level to load,
//! and the level load reuses the transfer buffer.

use quake_core::combat::WeaponState;
use quake_core::level_session::LevelPresentation;
use quake_core::save::{self, Reader, Snapshot as _, Writer};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::player::Player;

/// Directory names on the card, one per slot, in the retail `BExxx-xxxxx<id>`
/// shape so a real console lists them beside commercial saves.
const FILES: [&str; quake_core::menu::SAVE_SLOTS as usize] = [
    "BESLES-00000QPSX0001",
    "BESLES-00000QPSX0002",
    "BESLES-00000QPSX0003",
];
/// What the BIOS card manager shows.
const TITLE: &str = "QUAKE";
/// Largest payload a save may have: the buffer the caller keeps on its own
/// stack frame for the encoder and the card driver to share. The biggest
/// Episode 1 map needs about 4.3 KB (the regression logs every map's size); a
/// memory card block holds 7.6 KB. A save that does not fit reports SAVE TOO
/// LARGE rather than writing a short file.
pub const STAGING_BYTES: usize = 5 * 1024;

/// A slot number as an index into the three-entry tables.
const fn slot_index(slot: u8) -> usize {
    if slot as usize >= FILES.len() {
        FILES.len() - 1
    } else {
        slot as usize
    }
}

/// The answer shown under the pause menu.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Saved,
    NoCard,
    NoSave,
    Failed,
    /// A save from another build, or damaged.
    Refused,
    /// The game needs more room than the staging area has.
    TooLarge,
    /// `Host_Savegame_f` refuses "a dead player".
    Dead,
}

impl Outcome {
    /// What the pause menu says about the card's answer for `slot`.
    pub const fn notice(self, slot: u8) -> &'static str {
        match self {
            Self::Saved => ["GAME SAVED IN SLOT 1", "GAME SAVED IN SLOT 2", "GAME SAVED IN SLOT 3"]
                [slot_index(slot)],
            Self::NoCard => "NO MEMORY CARD",
            Self::NoSave => ["SLOT 1 IS EMPTY", "SLOT 2 IS EMPTY", "SLOT 3 IS EMPTY"]
                [slot_index(slot)],
            Self::Failed => "CARD ERROR",
            Self::Refused => "SAVE NOT USABLE",
            Self::TooLarge => "SAVE TOO LARGE",
            Self::Dead => "CANNOT SAVE WHEN DEAD",
        }
    }
}

/// What the header says to load before the rest can be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub map: EpisodeMap,
    pub skill: u8,
    pub runes: u8,
}

/// Serialise the whole game into `buf`; the byte count, or the count it needed.
#[inline(never)]
#[optimize(size)]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_save.text"
)]
pub fn encode(
    buf: &mut [u8],
    world: &ResidentMap,
    entities: &EntityScene,
    player: &Player,
    weapon: &WeaponState,
    presentation: &LevelPresentation,
) -> Result<usize, usize> {
    let map = EpisodeMap::ALL
        .iter()
        .position(|candidate| *candidate == world.map())
        .ok_or(0usize)?;
    save::frame(buf, |w: &mut Writer| {
        w.u8(map as u8);
        w.u8(entities.skill());
        w.u8(entities.runes());
        w.u32(presentation.level_ticks());
        player.save_state(w);
        weapon.save(w);
        entities.save_state(w);
    })
}

/// Read only the header of a framed save.
#[inline(never)]
#[optimize(size)]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_load.text"
)]
pub fn header(bytes: &[u8]) -> Option<Header> {
    let mut reader = Reader::open(bytes)?;
    let map = *EpisodeMap::ALL.get(usize::from(reader.u8()))?;
    let skill = reader.u8();
    let runes = reader.u8();
    if skill > 3 || reader.is_bad() {
        return None;
    }
    Some(Header { map, skill, runes })
}

/// Read the rest of a framed save into a level that the header has just
/// loaded. `false` means the save was inconsistent with the loaded map; the
/// caller must then discard the level and fall back.
#[inline(never)]
#[optimize(size)]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_load.text"
)]
pub fn restore(
    bytes: &[u8],
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut Player,
    weapon: &mut WeaponState,
    presentation: &mut LevelPresentation,
) -> bool {
    let Some(mut reader) = Reader::open(bytes) else {
        return false;
    };
    let _ = (reader.u8(), reader.u8(), reader.u8());
    let ticks = reader.u32();
    player.load_state(&mut reader);
    *weapon = WeaponState::load(&mut reader);
    entities.load_state(world, &mut reader);
    if !reader.finish() {
        return false;
    }
    presentation.set_level_ticks(ticks);
    true
}

/// The overlay entries: each package's single typed way in, called only through
/// the resident wrappers in `monster_code.rs` while the package is admitted.
#[inline(never)]
#[no_mangle]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_save.text"
)]
pub fn quake_save_encode(
    buf: &mut [u8],
    world: &ResidentMap,
    entities: &EntityScene,
    player: &Player,
    weapon: &WeaponState,
    presentation: &LevelPresentation,
) -> Result<usize, usize> {
    encode(buf, world, entities, player, weapon, presentation)
}

#[inline(never)]
#[no_mangle]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_load.text"
)]
pub fn quake_load_header(bytes: &[u8]) -> Option<Header> {
    header(bytes)
}

#[inline(never)]
#[no_mangle]
#[cfg_attr(
    all(feature = "monster-code-overlay", target_arch = "mips"),
    link_section = ".overlay.quake_load.text"
)]
pub fn quake_load_restore(
    bytes: &[u8],
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut Player,
    weapon: &mut WeaponState,
    presentation: &mut LevelPresentation,
) -> bool {
    restore(bytes, world, entities, player, weapon, presentation)
}

#[cfg(target_arch = "mips")]
mod card {
    use super::{slot_index, Outcome, FILES, TITLE};
    use psx_mc::{Card, HardwareCard, Slot};

    const SLOTS: [Slot; 2] = [Slot::One, Slot::Two];

    /// Write `bytes` to one card, formatting it first if it is blank.
    ///
    /// The tokenless constructor is the one the rest of the game's controller
    /// code uses (`psx_pad::poll_port1`); nothing else touches SIO0 while a
    /// menu action runs.
    #[allow(deprecated)]
    #[inline(never)]
    #[optimize(size)]
    fn write_to(port: Slot, bytes: &[u8], file: &str) -> Outcome {
        let mut card = Card::new(HardwareCard::new(port));
        match card.is_formatted() {
            Err(_) => Outcome::NoCard,
            Ok(formatted) => {
                if !formatted && card.format().is_err() {
                    return Outcome::Failed;
                }
                match card.write(file, TITLE, bytes) {
                    Ok(()) => Outcome::Saved,
                    Err(_) => Outcome::Failed,
                }
            }
        }
    }

    /// Read the save from one card into `staging`.
    #[allow(deprecated)]
    #[inline(never)]
    #[optimize(size)]
    fn read_from(port: Slot, staging: &mut [u8], file: &str) -> Result<usize, Outcome> {
        let mut card = Card::new(HardwareCard::new(port));
        match card.is_formatted() {
            Err(_) => Err(Outcome::NoCard),
            Ok(false) => Err(Outcome::NoSave),
            Ok(true) => card.read(file, staging).map_err(|_| Outcome::NoSave),
        }
    }

    /// Write `bytes` to the first card that takes it.
    #[inline(never)]
    #[optimize(size)]
    #[no_mangle]
    #[cfg_attr(
        all(feature = "monster-code-overlay", target_arch = "mips"),
        link_section = ".overlay.quake_card.text"
    )]
    pub fn quake_card_write(bytes: &[u8], slot: u8) -> Outcome {
        let file = FILES[slot_index(slot)];
        let mut result = Outcome::NoCard;
        for port in SLOTS {
            match write_to(port, bytes, file) {
                Outcome::Saved => return Outcome::Saved,
                Outcome::NoCard => {}
                other => result = other,
            }
        }
        result
    }

    /// Read the save into `staging` from the first card that has one.
    #[inline(never)]
    #[optimize(size)]
    #[no_mangle]
    #[cfg_attr(
        all(feature = "monster-code-overlay", target_arch = "mips"),
        link_section = ".overlay.quake_card.text"
    )]
    pub fn quake_card_read(staging: &mut [u8], slot: u8) -> Result<usize, Outcome> {
        let file = FILES[slot_index(slot)];
        let mut result = Outcome::NoCard;
        for port in SLOTS {
            match read_from(port, staging, file) {
                Ok(len) => return Ok(len),
                Err(Outcome::NoCard) => {}
                Err(other) => result = other,
            }
        }
        Err(result)
    }
}

#[cfg(not(target_arch = "mips"))]
mod card {
    use super::Outcome;

    pub fn quake_card_write(_bytes: &[u8], _slot: u8) -> Outcome {
        Outcome::NoCard
    }

    pub fn quake_card_read(_staging: &mut [u8], _slot: u8) -> Result<usize, Outcome> {
        Err(Outcome::NoCard)
    }
}

pub use card::{quake_card_read, quake_card_write};
