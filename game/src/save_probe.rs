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

/// The scripted run, in order: loads of slots 2 then 1 right after boot (a
/// card from an earlier boot restores them; a blank card answers that the slot
/// is empty and the run goes on), a save to slot 1, a perturbation, a save to
/// slot 2, a second perturbation, loads of slots 1, 2 and 3 (the last is
/// empty), then one level start per map to log each map's save size, with a
/// save into slot 3 on E1M3 (the largest save) on the way, and a last load of
/// that slot from E1M8: a save from another map, through a level change.
const BOOT_LOAD_AT_TICK: u32 = 60;
const BOOT_LOAD_FIRST_AT_TICK: u32 = 90;
const SAVE_FIRST_AT_TICK: u32 = 300;
const PERTURB_FIRST_AT_TICK: u32 = 450;
const SAVE_SECOND_AT_TICK: u32 = 600;
const PERTURB_SECOND_AT_TICK: u32 = 750;
const LOAD_AT_TICK: u32 = 900;
const SURVEY_AT_TICK: u32 = 1300;
const SURVEY_STEP_TICKS: u32 = 150;
const SURVEY_MAPS: u8 = 9;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Stage {
    BootLoadSecond,
    BootLoadFirst,
    SaveFirst,
    PerturbFirst,
    SaveSecond,
    PerturbSecond,
    Load(u8),
    Survey(u8),
    CrossLoad,
    Done,
}

static mut TICK: u32 = 0;
static mut STAGE: Stage = Stage::BootLoadSecond;
/// The load in flight is a boot one: it logs under its own phase names.
static mut BOOT_LOAD_ISSUED: bool = false;
static mut PERTURB_DUE: u8 = 0;
static mut THIRD_SAVED: bool = false;
/// The load in flight is the last one, from another map.
static mut CROSS_LOAD_ISSUED: bool = false;

/// The menu press due this frame, if any.
pub fn scripted(elapsed_ticks: u16) -> Option<MenuAction> {
    // SAFETY: single guest thread.
    unsafe {
        TICK = TICK.wrapping_add(u32::from(elapsed_ticks));
        match STAGE {
            Stage::BootLoadSecond if TICK >= BOOT_LOAD_AT_TICK => {
                BOOT_LOAD_ISSUED = true;
                STAGE = Stage::BootLoadFirst;
                Some(MenuAction::LoadGame(1))
            }
            Stage::BootLoadFirst if TICK >= BOOT_LOAD_FIRST_AT_TICK => {
                BOOT_LOAD_ISSUED = true;
                STAGE = Stage::SaveFirst;
                Some(MenuAction::LoadGame(0))
            }
            Stage::SaveFirst if TICK >= SAVE_FIRST_AT_TICK => {
                STAGE = Stage::PerturbFirst;
                Some(MenuAction::SaveGame(0))
            }
            Stage::PerturbFirst if TICK >= PERTURB_FIRST_AT_TICK => {
                STAGE = Stage::SaveSecond;
                PERTURB_DUE = 1;
                None
            }
            Stage::SaveSecond if TICK >= SAVE_SECOND_AT_TICK => {
                STAGE = Stage::PerturbSecond;
                Some(MenuAction::SaveGame(1))
            }
            Stage::PerturbSecond if TICK >= PERTURB_SECOND_AT_TICK => {
                STAGE = Stage::Load(0);
                PERTURB_DUE = 2;
                None
            }
            Stage::Load(slot) if TICK >= LOAD_AT_TICK + 100 * u32::from(slot) => {
                STAGE = if slot < 2 {
                    Stage::Load(slot + 1)
                } else {
                    Stage::Survey(0)
                };
                Some(MenuAction::LoadGame(slot))
            }
            Stage::Survey(map)
                if TICK >= SURVEY_AT_TICK + SURVEY_STEP_TICKS * u32::from(map) =>
            {
                // The map before E1M4 is E1M3: save its game first.
                if map == 4 && !THIRD_SAVED {
                    THIRD_SAVED = true;
                    return Some(MenuAction::SaveGame(2));
                }
                STAGE = if map + 1 < SURVEY_MAPS {
                    Stage::Survey(map + 1)
                } else {
                    Stage::CrossLoad
                };
                Some(MenuAction::StartLevel(map))
            }
            Stage::CrossLoad
                if TICK >= SURVEY_AT_TICK + SURVEY_STEP_TICKS * u32::from(SURVEY_MAPS) =>
            {
                STAGE = Stage::Done;
                CROSS_LOAD_ISSUED = true;
                Some(MenuAction::LoadGame(2))
            }
            _ => None,
        }
    }
}

/// Between the saves the player is moved, hurt and handed every weapon, so
/// that each load has a position, a health and an inventory to put back (the
/// idle probe otherwise stands still). The perturbation's number, once, on the
/// due frame.
pub fn take_perturb() -> Option<u8> {
    // SAFETY: single guest thread.
    match unsafe { core::mem::take(&mut PERTURB_DUE) } {
        0 => None,
        n => Some(n),
    }
}

/// The tick the script has reached.
pub fn phase_tick() -> u32 {
    // SAFETY: single guest thread.
    unsafe { TICK }
}

/// A boot-time load of slot 1 that restored a saved game ends the script: the
/// run was only there to show what a fresh boot gets back from a card.
fn note_load_outcome(slot: u8, restored: bool) {
    // SAFETY: single guest thread.
    unsafe {
        BOOT_LOAD_ISSUED = false;
        if slot == 0 && restored {
            STAGE = Stage::Done;
        }
    }
}

const BEFORE_SAVE: [&str; 3] = ["before-save-1", "before-save-2", "before-save-3"];
const AFTER_SAVE: [&str; 3] = ["after-save-1", "after-save-2", "after-save-3"];
const BEFORE_LOAD: [&str; 3] = ["before-load-1", "before-load-2", "before-load-3"];
const AFTER_LOAD: [&str; 3] = ["after-load-1", "after-load-2", "after-load-3"];
const BEFORE_CROSS_LOAD: &str = "before-cross-load-3";
const AFTER_CROSS_LOAD: &str = "after-cross-load-3";
const BEFORE_BOOT_LOAD: [&str; 3] = [
    "before-boot-load-1",
    "before-boot-load-2",
    "before-boot-load-3",
];
const AFTER_BOOT_LOAD: [&str; 3] = [
    "after-boot-load-1",
    "after-boot-load-2",
    "after-boot-load-3",
];

/// Phase name for a menu press, numbered by the slot the player sees: a boot
/// load logs as `boot-load`, a level start as `survey`.
pub fn phase_name(action: MenuAction, before: bool) -> &'static str {
    // SAFETY: single guest thread.
    let boot = unsafe { BOOT_LOAD_ISSUED };
    // SAFETY: single guest thread.
    if matches!(action, MenuAction::LoadGame(_)) && unsafe { CROSS_LOAD_ISSUED } {
        return if before { BEFORE_CROSS_LOAD } else { AFTER_CROSS_LOAD };
    }
    let table = |slot: u8, names: &[&'static str; 3]| names[usize::from(slot).min(2)];
    match (action, before, boot) {
        (MenuAction::SaveGame(slot), true, _) => table(slot, &BEFORE_SAVE),
        (MenuAction::SaveGame(slot), false, _) => table(slot, &AFTER_SAVE),
        (MenuAction::LoadGame(slot), true, true) => table(slot, &BEFORE_BOOT_LOAD),
        (MenuAction::LoadGame(slot), false, true) => table(slot, &AFTER_BOOT_LOAD),
        (MenuAction::LoadGame(slot), true, false) => table(slot, &BEFORE_LOAD),
        (MenuAction::LoadGame(slot), false, false) => table(slot, &AFTER_LOAD),
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
         ammo={},{},{},{} moved={} map={:?} outcome={outcome}",
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
        entities.probe_moved_movers(),
        world.map(),
    );
    psx_telemetry::emit::debug_log(&line);
    if let Some(slot) = AFTER_BOOT_LOAD.iter().position(|name| *name == phase) {
        note_load_outcome(slot as u8, outcome == "loaded");
    }
}
