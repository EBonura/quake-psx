//! End-of-level bookkeeping: the authored monster population, the authored
//! level titles, and the values the intermission panel prints between maps.
//!
//! The original keeps `killed_monsters`, `total_monsters`, `found_secrets` and
//! `total_secrets` as per-level globals and prints all four on the
//! intermission screen together with the worldspawn `message`. Secrets already
//! live in [`crate::secrets`]; this module owns the other three.

use crate::monster::MonsterKind;
use crate::targets::{excluded_for_skill, TargetEntitySource};

const CLASS_MONSTER_ZOMBIE: u8 = 0x44;
/// `monster_zombie` spawnflag 1: authored wall decoration.
const SPAWNFLAG_ZOMBIE_CRUCIFIED: u16 = 1;

/// Authored shareware level titles, indexed by [`level_index`] order.
///
/// These are the worldspawn `message` values of the shipping shareware BSPs.
/// The cook drops worldspawn's message (it keeps only `worldtype` and the CD
/// track), so the runtime carries them here instead, and the host builder
/// asserts every entry against the real entity lump in PAK0 so a wrong title
/// fails `check` rather than reaching a screen.
pub const LEVEL_TITLES: [&str; 9] = [
    "Introduction",
    "the Slipgate Complex",
    "Castle of the Damned",
    "the Necropolis",
    "the Grisly Grotto",
    "Gloom Keep",
    "The Door To Chthon",
    "The House of Chthon",
    "Ziggurat Vertigo",
];

/// Cooked map file names in [`LEVEL_TITLES`] order.
pub const LEVEL_NAMES: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

#[optimize(size)]
pub fn level_title(index: u8) -> &'static str {
    LEVEL_TITLES.get(index as usize).copied().unwrap_or("")
}

/// `killed_monsters` / `total_monsters` for one level.
///
/// Deliberate deviation, stated rather than hidden: a crucified
/// `monster_zombie` is authored decoration this runtime never lets the player
/// hurt, so it is excluded from the total. Counting it would make every map
/// that authors one unable to reach 100% kills. Chthon is counted: he is not
/// damageable by weapons, but the authored `event_lightning` chain does kill
/// him, so he belongs in E1M7's total.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MonsterCounter {
    killed: u16,
    total: u16,
}

impl MonsterCounter {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            killed: 0,
            total: 0,
        }
    }

    #[optimize(size)]
    pub const fn killed(&self) -> u16 {
        self.killed
    }

    #[optimize(size)]
    pub const fn total(&self) -> u16 {
        self.total
    }

    /// Recount `total_monsters` for a freshly loaded map.
    #[optimize(size)]
    pub fn load<S>(&mut self, source: &S, skill: u8)
    where
        S: TargetEntitySource + ?Sized,
    {
        self.killed = 0;
        self.total = count_authored(source, skill);
    }

    /// The runtime rescans its live monsters each frame, so the kill count is
    /// set rather than incremented: a scan can never double-count a death or
    /// miss a telefrag.
    #[optimize(size)]
    pub fn set_killed(&mut self, killed: u16) {
        self.killed = killed.min(self.total);
    }
}

/// Number of killable monsters a skill actually spawns.
#[optimize(size)]
pub fn count_authored<S>(source: &S, skill: u8) -> u16
where
    S: TargetEntitySource + ?Sized,
{
    let mut total = 0u16;
    for index in 0..source.entity_count() {
        let Some(entity) = source.entity_at(index) else {
            continue;
        };
        if !counts_toward_total(entity.class_name, entity.spawn_flags, skill) {
            continue;
        }
        total = total.saturating_add(1);
    }
    total
}

#[optimize(size)]
pub fn counts_toward_total(class_name: u8, spawn_flags: u16, skill: u8) -> bool {
    if excluded_for_skill(spawn_flags, skill) {
        return false;
    }
    if MonsterKind::from_class_name(class_name).is_none() {
        return false;
    }
    !(class_name == CLASS_MONSTER_ZOMBIE && spawn_flags & SPAWNFLAG_ZOMBIE_CRUCIFIED != 0)
}

/// One end-of-level panel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IntermissionView {
    /// Worldspawn `message` of the level that was just finished.
    pub title: &'static str,
    pub kills: u16,
    pub total_kills: u16,
    pub secrets: u16,
    pub total_secrets: u16,
    /// `cl.completed_time`: whole seconds of gameplay spent on the level.
    pub seconds: u16,
    /// The Chthon return, as a stage rather than a flag so the view does not
    /// grow: [`IntermissionView::EPISODE_NONE`] is an ordinary panel,
    /// [`IntermissionView::EPISODE_PANEL`] the episode-complete panel, and the
    /// stages above it the finale text pages up to
    /// [`IntermissionView::FINALE_LAST`].
    pub episode: u8,
}

impl IntermissionView {
    pub const EPISODE_HEADLINE: &'static str = "EPISODE 1 COMPLETE";
    pub const EPISODE_LINE: &'static str = "THE RUNE OF EARTH MAGIC IS YOURS";
    pub const HEADLINE: &'static str = "LEVEL COMPLETE";

    /// An ordinary between-maps panel.
    pub const EPISODE_NONE: u8 = 0;
    /// The episode-complete panel, still counting kills, secrets and time.
    pub const EPISODE_PANEL: u8 = 1;
    /// Last finale stage; the stage after it returns to Start.
    pub const FINALE_LAST: u8 = 3;

    /// `ExitIntermission`'s shareware Episode 1 finale, byte for byte as
    /// PAK0's `progs.dat` carries the unregistered `SVC_FINALE` string, and
    /// wrapped on its own authored newlines.
    ///
    /// The original prints all twelve lines at once. This port pages them
    /// because the whole text is 349 glyphs and the menu packet arena holds
    /// 256; six lines a page is the largest split that fits without buying
    /// heap the monster harness does not have. Each page holds for the same
    /// dwell as the panel, so a headless run crosses both without pressing.
    pub const FINALE_PAGE_1: &'static str = "As the corpse of the monstrous entity\n\
        Chthon sinks back into the lava whence\n\
        it rose, you grip the Rune of Earth\n\
        Magic tightly. Now that you have\n\
        conquered the Dimension of the Doomed,\n\
        realm of Earth Magic, you are ready to";
    pub const FINALE_PAGE_2: &'static str = "complete your task in the other three\n\
        haunted lands of Quake. Or are you? If\n\
        you don't register Quake, you'll never\n\
        know what awaits you in the Realm of\n\
        Black Magic, the Netherworld, and the\n\
        Elder World!";

    /// Left edge both finale pages are drawn at.
    ///
    /// The original centres each line; this port draws the page as one
    /// left-aligned block at the first line's centred origin, which is what
    /// [`crate::text::TextGlyphs`] already does for authored centerprints. The
    /// value is a constant so the draw costs no layout pass on an image with
    /// under a kilobyte of slack, and the test below re-derives it from
    /// [`crate::text::centered_first_line_x`] so it cannot rot.
    pub const FINALE_X: i16 = 12;

    #[optimize(size)]
    pub const fn finale_text(stage: u8) -> &'static str {
        if stage == Self::EPISODE_PANEL + 1 {
            Self::FINALE_PAGE_1
        } else {
            Self::FINALE_PAGE_2
        }
    }

    /// Completion time as the original prints it: whole minutes and the
    /// leftover seconds, which the panel pads to two digits.
    #[optimize(size)]
    pub const fn time(&self) -> (u16, u16) {
        (self.seconds / 60, self.seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::MapEntity;

    #[optimize(size)]
    fn monster(class_name: u8, spawn_flags: u16) -> MapEntity {
        MapEntity {
            class_name,
            spawn_flags,
            ..MapEntity::default()
        }
    }

    #[optimize(size)]
    #[test]
    fn crucified_zombies_and_skill_exclusions_stay_out_of_the_total() {
        let entities = [
            monster(0x36, 0),                                  // monster_army
            monster(0x44, 0),                                  // monster_zombie
            monster(CLASS_MONSTER_ZOMBIE, 1),                  // crucified decoration
            monster(0x3e, crate::targets::SPAWNFLAG_NOT_EASY), // ogre, not on easy
            monster(0x37, 0),                                  // Chthon
            MapEntity::default(),                              // worldspawn
        ];
        assert_eq!(count_authored(&entities[..], 0), 3);
        assert_eq!(count_authored(&entities[..], 1), 4);
    }

    #[optimize(size)]
    #[test]
    fn the_kill_count_never_passes_the_authored_total() {
        let entities = [monster(0x36, 0), monster(0x3d, 0)];
        let mut counter = MonsterCounter::new();
        counter.load(&entities[..], 0);
        assert_eq!(counter.total(), 2);
        counter.set_killed(9);
        assert_eq!(counter.killed(), 2);
        counter.load(&entities[..], 0);
        assert_eq!(counter.killed(), 0);
    }

    #[optimize(size)]
    #[test]
    fn the_intermission_splits_its_completion_time_into_minutes_and_seconds() {
        let panel = |seconds| IntermissionView {
            title: "",
            kills: 0,
            total_kills: 0,
            secrets: 0,
            total_secrets: 0,
            seconds,
            episode: IntermissionView::EPISODE_NONE,
        };
        assert_eq!(panel(0).time(), (0, 0));
        assert_eq!(panel(7).time(), (0, 7));
        assert_eq!(panel(59).time(), (0, 59));
        assert_eq!(panel(60).time(), (1, 0));
        assert_eq!(panel(127).time(), (2, 7));
        assert_eq!(panel(3599).time(), (59, 59));
    }

    /// The finale pages are drawn one quad per visible glyph into the 256-quad
    /// menu arena at a hard-coded left edge, so the column count, the glyph
    /// count and that edge are all part of the contract, not cosmetics.
    #[optimize(size)]
    #[test]
    fn finale_pages_are_laid_out_where_the_font_centres_them() {
        for stage in IntermissionView::EPISODE_PANEL + 1..=IntermissionView::FINALE_LAST {
            let text = IntermissionView::finale_text(stage);
            assert_eq!(text.lines().count(), 6);
            let columns = text.lines().map(str::len).max().unwrap_or(0) as i16;
            assert!(IntermissionView::FINALE_X + columns * 8 <= 320);
            assert_eq!(
                crate::text::centered_first_line_x(text, 320, 8, 40),
                IntermissionView::FINALE_X
            );
            let glyphs = text.bytes().filter(|byte| !b" \n".contains(byte)).count();
            assert!(glyphs <= 256, "stage {stage} needs {glyphs} quads");
        }
    }

    #[optimize(size)]
    #[test]
    fn every_authored_level_carries_a_title() {
        assert_eq!(LEVEL_TITLES.len(), LEVEL_NAMES.len());
        for index in 0..LEVEL_TITLES.len() as u8 {
            assert!(!level_title(index).is_empty());
        }
        assert_eq!(level_title(9), "");
    }
}
