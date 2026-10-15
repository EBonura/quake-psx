//! Memory-card save and load: one manual slot per card.
//!
//! Saving is only ever the player's own action from the pause menu; nothing
//! here writes on its own. The save is the map, the skill and the runes in
//! hand, the player, the weapon state and the scene's moving parts
//! (`entity/save.rs`), framed by `quake_core::save` with a version and a
//! checksum, so a card from another build or a damaged one is refused rather
//! than misread.
//!
//! The code and the payload both live in the map loader's retained 30 KiB
//! transfer buffer, which nothing else touches while a menu action runs, so
//! this feature holds almost no memory of its own. Its three halves are
//! overlay packages (`monster_code.rs`): the encoder, the decoder and the
//! card driver, each installed in turn into the buffer's code half. The
//! payload is staged in the tail of the other half, which the package reads
//! never reach (`STAGING_BYTES`). A load reads the card twice: the first read
//! decides which level to load, and the level load reuses the buffer.

use quake_core::combat::WeaponState;
use quake_core::level_session::LevelPresentation;
use quake_core::save::{self, Reader, Snapshot as _, Writer};

use crate::asset::{EpisodeMap, ResidentMap};
use crate::entity::EntityScene;
use crate::player::Player;

/// Directory name on the card, in the retail `BExxx-xxxxx<id>` shape so a real
/// console lists it beside commercial saves.
const FILE: &str = "BESLES-00000QPSX0001";
/// What the BIOS card manager shows.
const TITLE: &str = "QUAKE";
/// Largest payload the staging area takes: the tail of the transfer buffer's
/// transport half, past the largest package (`monster_code::STAGING_OFFSET`).
/// A save that does not fit reports a card error rather than a short write.
pub const STAGING_BYTES: usize = 5 * 1024;

/// The answer shown under the pause menu.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Saved,
    NoCard,
    NoSave,
    Failed,
    /// A save from another build, or damaged.
    Refused,
}

impl Outcome {
    pub const fn notice(self) -> &'static str {
        match self {
            Self::Saved => "GAME SAVED",
            Self::NoCard => "NO MEMORY CARD",
            Self::NoSave => "NO SAVED GAME",
            Self::Failed => "CARD ERROR",
            Self::Refused => "SAVE NOT USABLE",
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

/// Serialise the whole game into `buf`; the byte count, or `None` if it did not fit.
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
) -> Option<usize> {
    let map = EpisodeMap::ALL.iter().position(|candidate| *candidate == world.map())?;
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
) -> Option<usize> {
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
    use super::{Outcome, FILE, TITLE};

    #[inline(never)]
    #[optimize(size)]
    fn with_card<T>(
        slot: psx_mc::Slot,
        f: impl FnOnce(&mut psx_mc::Card<psx_mc::HardwareCard>) -> T,
    ) -> T {
        let mut card = psx_mc::Card::new(psx_mc::HardwareCard::new(slot));
        f(&mut card)
    }

    const SLOTS: [psx_mc::Slot; 2] = [psx_mc::Slot::One, psx_mc::Slot::Two];

    /// Write `bytes` to the first card that takes it, formatting a blank card.
    #[inline(never)]
    #[optimize(size)]
    #[no_mangle]
    #[cfg_attr(
        all(feature = "monster-code-overlay", target_arch = "mips"),
        link_section = ".overlay.quake_card.text"
    )]
    pub fn quake_card_write(bytes: &[u8]) -> Outcome {
        let mut result = Outcome::NoCard;
        for slot in SLOTS {
            let outcome = with_card(slot, |card| match card.is_formatted() {
                Err(_) => Outcome::NoCard,
                Ok(formatted) => {
                    if !formatted && card.format().is_err() {
                        return Outcome::Failed;
                    }
                    match card.write(FILE, TITLE, bytes) {
                        Ok(()) => Outcome::Saved,
                        Err(_) => Outcome::Failed,
                    }
                }
            });
            match outcome {
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
    pub fn quake_card_read(staging: &mut [u8]) -> Result<usize, Outcome> {
        let mut result = Outcome::NoCard;
        for slot in SLOTS {
            let found = with_card(slot, |card| match card.is_formatted() {
                Err(_) => Err(Outcome::NoCard),
                Ok(false) => Err(Outcome::NoSave),
                Ok(true) => card.read(FILE, staging).map_err(|_| Outcome::NoSave),
            });
            match found {
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

    pub fn quake_card_write(_bytes: &[u8]) -> Outcome {
        Outcome::NoCard
    }

    pub fn quake_card_read(_staging: &mut [u8]) -> Result<usize, Outcome> {
        Err(Outcome::NoCard)
    }
}

pub use card::{quake_card_read, quake_card_write};
