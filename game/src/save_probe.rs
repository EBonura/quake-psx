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
use quake_core::combat::{AmmoKind, WeaponState};
use quake_core::level_session::LevelPresentation;
use quake_core::menu::MenuAction;

use crate::asset::ResidentMap;
use crate::entity::EntityScene;
use crate::player::Player;

/// The scripted run, in order: a load right after boot (a card from an earlier
/// boot restores it; an empty card answers NO SAVED GAME and the run goes on),
/// a save, a load, then one level start per map to log each map's save size.
const BOOT_LOAD_AT_TICK: u32 = 60;
const SAVE_AT_TICK: u32 = 300;
const PERTURB_AT_TICK: u32 = 600;
const LOAD_AT_TICK: u32 = 900;
const SURVEY_AT_TICK: u32 = 1200;
const SURVEY_STEP_TICKS: u32 = 150;
const SURVEY_MAPS: u8 = 9;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Stage {
    BootLoad,
    Save,
    Perturb,
    Load,
    Survey(u8),
    Done,
}

static mut TICK: u32 = 0;
static mut STAGE: Stage = Stage::BootLoad;
/// The load in flight is the boot one: it logs under its own phase names.
static mut BOOT_LOAD_ISSUED: bool = false;
static mut PERTURB_DUE: bool = false;

/// The menu press due this frame, if any.
pub fn scripted(elapsed_ticks: u16) -> Option<MenuAction> {
    // SAFETY: single guest thread.
    unsafe {
        TICK = TICK.wrapping_add(u32::from(elapsed_ticks));
        match STAGE {
            Stage::BootLoad if TICK >= BOOT_LOAD_AT_TICK => {
                BOOT_LOAD_ISSUED = true;
                STAGE = Stage::Save;
                Some(MenuAction::LoadGame)
            }
            Stage::Save if TICK >= SAVE_AT_TICK => {
                STAGE = Stage::Perturb;
                Some(MenuAction::SaveGame)
            }
            Stage::Perturb if TICK >= PERTURB_AT_TICK => {
                STAGE = Stage::Load;
                PERTURB_DUE = true;
                None
            }
            Stage::Load if TICK >= LOAD_AT_TICK => {
                STAGE = Stage::Survey(0);
                Some(MenuAction::LoadGame)
            }
            Stage::Survey(map)
                if TICK >= SURVEY_AT_TICK + SURVEY_STEP_TICKS * u32::from(map) =>
            {
                STAGE = if map + 1 < SURVEY_MAPS {
                    Stage::Survey(map + 1)
                } else {
                    Stage::Done
                };
                Some(MenuAction::StartLevel(map))
            }
            _ => None,
        }
    }
}

/// Between the save and the load the player is moved, hurt and handed every
/// weapon, so that the load has a position, a health and an inventory to put
/// back (the idle probe otherwise stands still). True once, on the due frame.
pub fn take_perturb() -> bool {
    // SAFETY: single guest thread.
    unsafe { core::mem::take(&mut PERTURB_DUE) }
}

/// The tick the script has reached.
pub fn phase_tick() -> u32 {
    // SAFETY: single guest thread.
    unsafe { TICK }
}

/// A load that restored a saved game ends the script: the run was only there
/// to show what a fresh boot gets back from a card.
fn note_load_outcome(restored: bool) {
    // SAFETY: single guest thread.
    unsafe {
        if BOOT_LOAD_ISSUED {
            BOOT_LOAD_ISSUED = false;
            if restored {
                STAGE = Stage::Done;
            }
        }
    }
}

/// Phase name for a menu press: the boot load logs as `boot-load`, a level
/// start as `survey`.
pub fn phase_name(action: MenuAction, side: &'static str) -> &'static str {
    // SAFETY: single guest thread.
    let boot = unsafe { BOOT_LOAD_ISSUED };
    match (action, side, boot) {
        (MenuAction::SaveGame, "before", _) => "before-save",
        (MenuAction::SaveGame, _, _) => "after-save",
        (MenuAction::LoadGame, "before", true) => "before-boot-load",
        (MenuAction::LoadGame, _, true) => "after-boot-load",
        (MenuAction::LoadGame, "before", false) => "before-load",
        (MenuAction::LoadGame, _, false) => "after-load",
        _ => "survey",
    }
}

/// Log the fingerprint of the current state under `phase`.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
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
    let mut staging = [0u8; crate::savegame::STAGING_BYTES];
    let (encoded, hash) = monster_code.encode_fingerprint(
        world,
        entities,
        player,
        weapon,
        presentation,
        &mut staging,
    );
    // A game too large for the staging area logs the size it asked for, flagged.
    let (len, need) = match encoded {
        Ok(len) => (len, 0),
        Err(need) => (0, need),
    };
    crate::quake::reinstall_monster_code(world, monster_code);
    music.resume_after_load(psx_rt::interrupts::vblank_count());
    let origin = player.origin();
    let inventory = weapon.inventory();
    let line = format!(
        "SV {phase} t={} len={len} need={need} fp={hash:08x} pos={},{},{} hp={} armor={} keys={} wpn={:02x} \
         ammo={},{},{},{} map={:?} outcome={outcome}",
        phase_tick(),
        origin.x,
        origin.y,
        origin.z,
        inventory.health(),
        inventory.armor(),
        inventory.keys(),
        inventory.owned_weapons(),
        inventory.ammo(AmmoKind::Shells),
        inventory.ammo(AmmoKind::Nails),
        inventory.ammo(AmmoKind::Rockets),
        inventory.ammo(AmmoKind::Cells),
        world.map(),
    );
    psx_telemetry::emit::debug_log(&line);
    if phase == "after-boot-load" {
        note_load_outcome(outcome == "loaded");
    }
}
