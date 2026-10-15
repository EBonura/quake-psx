//! Save and load gate: scripts the pause menu's SAVE GAME and LOAD GAME
//! presses on E1M1 and logs a fingerprint of the whole saved state around
//! them, so the host builder can check the round trip through the real card
//! code on the emulator's memory card.
//!
//! ```text
//! SV <phase> t=<tick> len=<bytes> fp=<fnv1a hex> outcome=<text>
//! ```
//!
//! The fingerprint is the checksum of the game serialised with
//! `savegame::encode`. A load that puts back everything the save holds makes
//! the post-load fingerprint equal the pre-save one, however far the world
//! moved in between.

use alloc::format;
use quake_core::combat::WeaponState;
use quake_core::level_session::LevelPresentation;
use quake_core::menu::MenuAction;

use crate::asset::ResidentMap;
use crate::entity::EntityScene;
use crate::player::Player;

const SAVE_AT_TICK: u32 = 300;
const LOAD_AT_TICK: u32 = 900;

static mut TICK: u32 = 0;
static mut SAVED: bool = false;
static mut LOADED: bool = false;

/// The menu press due this frame, if any.
pub fn scripted(elapsed_ticks: u16) -> Option<MenuAction> {
    // SAFETY: single guest thread.
    unsafe {
        TICK = TICK.wrapping_add(u32::from(elapsed_ticks));
        if !SAVED && TICK >= SAVE_AT_TICK {
            SAVED = true;
            return Some(MenuAction::SaveGame);
        }
        if SAVED && !LOADED && TICK >= LOAD_AT_TICK {
            LOADED = true;
            return Some(MenuAction::LoadGame);
        }
    }
    None
}

/// Whether the load press has been issued, so the caller can fingerprint the
/// state it returns to.
pub fn phase_tick() -> u32 {
    // SAFETY: single guest thread.
    unsafe { TICK }
}

/// Log the fingerprint of the current state under `phase`.
#[allow(clippy::too_many_arguments)]
pub fn fingerprint(
    phase: &str,
    world: &mut ResidentMap,
    music: &mut crate::music::Music,
    monster_code: &mut crate::monster_code::MonsterCode,
    entities: &EntityScene,
    player: &Player,
    weapon: &WeaponState,
    presentation: &LevelPresentation,
    outcome: &str,
) {
    music.suspend_for_load();
    let (len, hash) =
        monster_code.encode_fingerprint(world, entities, player, weapon, presentation);
    crate::quake::reinstall_monster_code(world, monster_code);
    music.resume_after_load(psx_rt::interrupts::vblank_count());
    let line = format!(
        "SV {phase} t={} len={len} fp={hash:08x} outcome={outcome}",
        phase_tick()
    );
    psx_telemetry::emit::debug_log(&line);
}
