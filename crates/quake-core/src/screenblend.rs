//! Quake's `V_ParseDamage`/`V_UpdatePalette` screen blends, in PS1 terms.
//!
//! The original tints the whole screen by rebuilding the palette every frame:
//! red when you are hurt, gold when you pick something up, and a sustained
//! murk while you are inside water, slime or lava. A PS1 has no palette to
//! rebuild, so the same signal is carried by full-screen semi-transparent
//! quads. The GPU offers four fixed blend equations rather than an arbitrary
//! alpha, so the shapes are mapped onto the two that fit:
//!
//! * the sustained contents tint uses `(background + foreground) / 2`, which
//!   is within a few percent of the original's own 128/255, 150/255 and
//!   150/255 alphas and darkens as well as tints, exactly like the original;
//! * the transient damage and pickup flashes use `background + foreground`,
//!   with the colour pre-scaled by the original's percentage, so a flash
//!   brightens and never dims the frame it lands on.
//!
//! Everything below is integer and holds the original's units: percentages
//! out of 255, the damage ramp `3 * count` clamped at 150, the pickup's flat
//! 50, and the fades of 150 and 100 percentage points per second.
//!
//! Death changes none of it, and there is nothing extra to implement for it.
//! The original has no death fade: `V_CalcRefdef` rolls the dead view and
//! returns, while `V_UpdatePalette` goes on subtracting `host_frametime*150`
//! from `CSHIFT_DAMAGE` whatever `STAT_HEALTH` says. The killing blow's red
//! drains away in under a second and the corpse watches an untinted screen
//! until it respawns. All that fidelity takes is calling `tick` on dead
//! frames, which the game loop already does.
//!
//! `V_CalcPowerupCshift`'s tints ride the SAME sustained quad as the contents
//! murk rather than a quad of their own, because the original does not draw a
//! quad per shift either: `V_UpdatePalette` folds every live cshift into one
//! palette. See `contents_tint` for the composition and for the one thing a
//! fixed 50/50 blend cannot reproduce.

use crate::survival::{PowerupKind, Powerups};

/// `CONTENTS_WATER`.
pub const CONTENTS_WATER: i16 = -3;
/// `CONTENTS_SLIME`.
pub const CONTENTS_SLIME: i16 = -4;
/// `CONTENTS_LAVA`.
pub const CONTENTS_LAVA: i16 = -5;

/// `cshift_water`.
const WATER_COLOR: (u8, u8, u8) = (130, 80, 50);
/// `cshift_slime`.
const SLIME_COLOR: (u8, u8, u8) = (0, 25, 5);
/// `cshift_lava`.
const LAVA_COLOR: (u8, u8, u8) = (255, 80, 0);
/// The `percent` beside each of those three: `cshift_water` carries 128,
/// slime and lava 150. Only the composition below reads them, because the
/// quad's own 50/50 already stands in for them on its own. `V_UpdatePalette`
/// shifts these right by eight, so 128 is exactly that half.
const WATER_PERCENT: i32 = 128;
const SLIME_PERCENT: i32 = 150;
const LAVA_PERCENT: i32 = 150;

/// `V_CalcPowerupCshift`, indexed by `PowerupKind::index`: `destcolor` then
/// `percent`, in the original's units.
const POWERUP_SHIFTS: [((u8, u8, u8), i32); 4] = [
    ((0, 0, 255), 30),      // Quad, item_artifact_super_damage.
    ((255, 255, 0), 30),    // Pentagram, invulnerability.
    ((100, 100, 100), 100), // Ring, invisibility.
    ((0, 255, 0), 20),      // Biosuit, enviro suit.
];

/// The background level the composition below assumes when it inverts the
/// GPU's fixed 50/50 blend. Mid grey: a rendered Quake frame is dark, but the
/// tint has to hold up over a lava pool as well as a corridor, and the mean is
/// the only choice that is wrong by the same amount in both directions.
const NOMINAL_BACKGROUND: i32 = 128;
/// `V_BonusFlash`'s `destcolor`.
const BONUS_COLOR: (u8, u8, u8) = (215, 186, 69);
/// `V_ParseDamage` with no armor.
const DAMAGE_BLOOD_COLOR: (u8, u8, u8) = (255, 0, 0);
/// `V_ParseDamage` with some armor.
const DAMAGE_ARMOR_COLOR: (u8, u8, u8) = (220, 50, 50);
/// `V_ParseDamage` when armor took more than health did.
const DAMAGE_MOSTLY_ARMOR_COLOR: (u8, u8, u8) = (200, 100, 100);

/// `cl.cshifts[CSHIFT_DAMAGE].percent` ceiling.
const DAMAGE_MAX_PERCENT: i32 = 150;
/// `V_BonusFlash` sets exactly this.
const BONUS_PERCENT: i32 = 50;
/// `cl.cshifts[CSHIFT_DAMAGE].percent -= host_frametime * 150`.
const DAMAGE_FADE_PER_SECOND: i32 = 150;
/// `cl.cshifts[CSHIFT_BONUS].percent -= host_frametime * 100`.
const BONUS_FADE_PER_SECOND: i32 = 100;
/// PORT ADDITION, not id1. The original has no level-transition fade at all:
/// `SCR_UpdateScreen` cuts straight from the last gameplay frame into the
/// intermission panel and straight out of it into `GotoNextMap`. This is a
/// console-side transition riding the full-screen quad the palette blends
/// already draw, so it costs one more quad and nothing else. Full black.
const TRANSITION_MAX_PERCENT: i32 = 100;
/// A third of a second in each direction, which is a transition rather than a
/// wait.
const TRANSITION_FADE_PER_SECOND: i32 = 300;
/// Fixed ticks per second. Every percentage below is stored multiplied by
/// this, so a fade of 150 points per second loses exactly 150 per tick with
/// no truncation: at one tick per frame an integer `percent * rate / 60`
/// would throw away the remainder and strand a flash at a fifth of its
/// starting value forever.
const TICKS_PER_SECOND: i32 = 60;

/// `V_UpdatePalette`'s per-pixel loop, run once on a nominal background and
/// then handed back as the foreground the GPU's fixed 50/50 blend needs.
///
/// Out of line, and never inlined into the renderer's quad builder: it runs
/// once a frame and only while an artifact is worn, so folding it into
/// `draw_screen_tints` buys nothing and costs six hundred bytes of image.
#[optimize(size)]
#[inline(never)]
fn compose(contents: Option<((u8, u8, u8), i32)>, powerup: PowerupKind) -> (u8, u8, u8) {
    /// One iteration of the original's inner loop, literally: `r += (percent
    /// * (destcolor - r)) >> 8`. The shift is id1's, not a division by 255
    /// dressed up, which is why `cshift_water`'s 128 lands on exactly the
    /// half the GPU blend already does.
    #[optimize(size)]
    fn shift(pixel: &mut [i32; 3], color: (u8, u8, u8), percent: i32) {
        let dest = [i32::from(color.0), i32::from(color.1), i32::from(color.2)];
        for channel in 0..3 {
            pixel[channel] += ((dest[channel] - pixel[channel]) * percent) >> 8;
        }
    }
    let mut pixel = [NOMINAL_BACKGROUND; 3];
    // `CSHIFT_CONTENTS` comes before `CSHIFT_POWERUP` in `cl.cshifts`, so the
    // liquid goes on first and the artifact tints what is left.
    if let Some((color, percent)) = contents {
        shift(&mut pixel, color, percent);
    }
    let (color, percent) = POWERUP_SHIFTS[powerup.index()];
    shift(&mut pixel, color, percent);
    // Invert `(background + foreground) / 2` at the nominal background.
    let undo = |value: i32| (2 * value - NOMINAL_BACKGROUND).clamp(0, 255) as u8;
    (undo(pixel[0]), undo(pixel[1]), undo(pixel[2]))
}

/// One full-screen quad the renderer should draw.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScreenTint {
    pub color: (u8, u8, u8),
    /// True for the sustained contents murk, which halves the background.
    /// False for a transient flash, which only adds to it.
    pub average: bool,
}

/// Every screen blend the player currently has, in the original's own units.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ScreenBlend {
    damage_percent: i32,
    damage_color: (u8, u8, u8),
    bonus_percent: i32,
    /// Port addition: the level-transition fade, positive towards black.
    transition_percent: i32,
    /// Signed points per second the transition is currently moving at.
    transition_rate: i32,
    contents: i16,
    /// `V_CalcPowerupCshift` is an else-if chain, so at most one artifact
    /// tints at a time and one slot holds the winner.
    powerup: Option<PowerupKind>,
}

impl ScreenBlend {
    /// How long a transition takes in either direction, for the caller that
    /// has to start the fade out before the thing it is fading out of ends.
    pub const TRANSITION_TICKS: u16 =
        (TRANSITION_MAX_PERCENT * TICKS_PER_SECOND / TRANSITION_FADE_PER_SECOND) as u16;

    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            damage_percent: 0,
            damage_color: DAMAGE_BLOOD_COLOR,
            bonus_percent: 0,
            transition_percent: 0,
            transition_rate: 0,
            contents: 0,
            powerup: None,
        }
    }

    /// `V_ParseDamage`. `count` is the original's `blood + armor` view damage.
    #[optimize(size)]
    pub fn take_damage(&mut self, health_damage: u16, armor_damage: u16) {
        let count = i32::from(health_damage) + i32::from(armor_damage);
        if count <= 0 {
            return;
        }
        self.damage_percent = (self.damage_percent + 3 * count * TICKS_PER_SECOND)
            .min(DAMAGE_MAX_PERCENT * TICKS_PER_SECOND);
        self.damage_color = if armor_damage > health_damage {
            DAMAGE_MOSTLY_ARMOR_COLOR
        } else if armor_damage > 0 {
            DAMAGE_ARMOR_COLOR
        } else {
            DAMAGE_BLOOD_COLOR
        };
    }

    /// `V_BonusFlash`, run by every item touch.
    #[optimize(size)]
    pub fn pick_up(&mut self) {
        self.bonus_percent = BONUS_PERCENT * TICKS_PER_SECOND;
    }

    /// PORT ADDITION, not id1: start at black and come up into whatever is
    /// drawn behind the fade.
    #[optimize(size)]
    pub fn fade_in_from_black(&mut self) {
        self.transition_percent = TRANSITION_MAX_PERCENT * TICKS_PER_SECOND;
        self.transition_rate = -TRANSITION_FADE_PER_SECOND;
    }

    /// PORT ADDITION, not id1: run down to black and hold there, so whatever
    /// replaces the screen next arrives on an already dark frame.
    #[optimize(size)]
    pub fn fade_out_to_black(&mut self) {
        self.transition_rate = TRANSITION_FADE_PER_SECOND;
    }

    /// The leaf contents the player's eye is in, for the sustained tint.
    #[optimize(size)]
    pub fn set_contents(&mut self, contents: i16) {
        self.contents = contents;
    }

    /// `V_CalcPowerupCshift`. The original is a single else-if chain, so only
    /// the first live artifact in this order tints, and a player holding the
    /// pentagram and the quad at once sees gold, never blue.
    #[optimize(size)]
    pub fn set_powerups(&mut self, powerups: Powerups) {
        self.powerup = if powerups.active(PowerupKind::Pentagram) {
            Some(PowerupKind::Pentagram)
        } else if powerups.active(PowerupKind::Biosuit) {
            Some(PowerupKind::Biosuit)
        } else if powerups.active(PowerupKind::Ring) {
            Some(PowerupKind::Ring)
        } else if powerups.active(PowerupKind::Quad) {
            Some(PowerupKind::Quad)
        } else {
            None
        };
    }

    /// `V_UpdatePalette`'s fades, at the fixed tick rate.
    #[optimize(size)]
    pub fn tick(&mut self, ticks: u16) {
        let ticks = i32::from(ticks.max(1));
        self.damage_percent = (self.damage_percent - DAMAGE_FADE_PER_SECOND * ticks).max(0);
        self.bonus_percent = (self.bonus_percent - BONUS_FADE_PER_SECOND * ticks).max(0);
        // Port addition, and the one blend here that can move either way.
        self.transition_percent = (self.transition_percent + self.transition_rate * ticks)
            .clamp(0, TRANSITION_MAX_PERCENT * TICKS_PER_SECOND);
    }

    /// PORT ADDITION, not id1: how hard the transition quad subtracts from the
    /// finished frame, out of 255. `None` when no transition is on screen.
    #[optimize(size)]
    pub const fn transition_shade(&self) -> Option<u8> {
        let percent = self.transition_percent / TICKS_PER_SECOND;
        if percent <= 0 {
            return None;
        }
        Some((percent * 255 / TRANSITION_MAX_PERCENT) as u8)
    }

    /// The one sustained quad: the contents murk, the powerup tint, or both
    /// folded together.
    ///
    /// There is no second quad for the powerup, because the original has no
    /// second anything: `V_UpdatePalette` walks `cl.cshifts` and applies each
    /// live shift to the SAME pixel, contents first and `CSHIFT_POWERUP` last.
    /// So this reproduces that loop once, on one nominal background pixel, and
    /// then works out the foreground colour that drives the GPU's fixed
    /// `(background + foreground) / 2` to the same answer.
    ///
    /// What it reproduces: the hue of every shift, their priority, their
    /// relative strength (a 20-percent suit is much weaker than a 100-percent
    /// ring), and the order they compose in when a powerup is worn in water.
    ///
    /// What it CANNOT reproduce, and no reader should assume it does: the
    /// blend is locked at 50/50, while the original's is `percent`. Matching
    /// the mean brightness therefore costs contrast. A quad-damage frame that
    /// should be `0.88 * background + blue` comes out `0.5 * background +
    /// blue-grey`: correct on a mid-grey pixel, too dark on a bright one and
    /// too bright on a black one. Bright liquids are hit hardest, and the
    /// ring, whose 100 flattens the frame in the original too, is hit least.
    /// The alternative was a fifth quad the fill rate cannot pay for.
    #[optimize(size)]
    pub fn contents_tint(&self) -> Option<ScreenTint> {
        let contents = match self.contents {
            CONTENTS_WATER => Some((WATER_COLOR, WATER_PERCENT)),
            CONTENTS_SLIME => Some((SLIME_COLOR, SLIME_PERCENT)),
            CONTENTS_LAVA => Some((LAVA_COLOR, LAVA_PERCENT)),
            _ => None,
        };
        let Some(powerup) = self.powerup else {
            // No powerup: the contents colour is already what the 50/50 blend
            // wants, within a rounding step of the original's own 128 and 150.
            return contents.map(|(color, _)| ScreenTint {
                color,
                average: true,
            });
        };
        Some(ScreenTint {
            color: compose(contents, powerup),
            average: true,
        })
    }

    /// The transient flash quad: the damage and pickup shifts summed and
    /// clamped, each already scaled by its own percentage.
    #[optimize(size)]
    pub fn flash_tint(&self) -> Option<ScreenTint> {
        let mut color = (0i32, 0i32, 0i32);
        let mut add = |source: (u8, u8, u8), percent: i32| {
            if percent <= 0 {
                return;
            }
            let scale = percent / TICKS_PER_SECOND;
            color.0 += i32::from(source.0) * scale / 255;
            color.1 += i32::from(source.1) * scale / 255;
            color.2 += i32::from(source.2) * scale / 255;
        };
        add(self.damage_color, self.damage_percent);
        add(BONUS_COLOR, self.bonus_percent);
        if color == (0, 0, 0) {
            return None;
        }
        Some(ScreenTint {
            color: (
                color.0.min(255) as u8,
                color.1.min(255) as u8,
                color.2.min(255) as u8,
            ),
            average: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn damage_uses_the_original_ramp_and_ceiling() {
        let mut blend = ScreenBlend::new();
        blend.take_damage(10, 0);
        assert_eq!(blend.damage_percent, 30 * TICKS_PER_SECOND);
        blend.take_damage(10, 0);
        assert_eq!(blend.damage_percent, 60 * TICKS_PER_SECOND);
        blend.take_damage(200, 0);
        assert_eq!(blend.damage_percent, DAMAGE_MAX_PERCENT * TICKS_PER_SECOND);
    }

    #[optimize(size)]
    #[test]
    fn damage_color_follows_the_armor_split() {
        let mut blend = ScreenBlend::new();
        blend.take_damage(10, 0);
        assert_eq!(blend.damage_color, DAMAGE_BLOOD_COLOR);
        blend.take_damage(10, 4);
        assert_eq!(blend.damage_color, DAMAGE_ARMOR_COLOR);
        blend.take_damage(2, 9);
        assert_eq!(blend.damage_color, DAMAGE_MOSTLY_ARMOR_COLOR);
    }

    #[optimize(size)]
    #[test]
    fn zero_damage_never_flashes() {
        let mut blend = ScreenBlend::new();
        blend.take_damage(0, 0);
        assert_eq!(blend.flash_tint(), None);
    }

    #[optimize(size)]
    #[test]
    fn a_flash_fades_to_nothing() {
        let mut blend = ScreenBlend::new();
        blend.take_damage(20, 0);
        assert!(blend.flash_tint().is_some());
        for _ in 0..60 {
            blend.tick(1);
        }
        assert_eq!(blend.flash_tint(), None);
        // The original's damage fade is 150 points per second, so a full
        // 150-point hit is gone in exactly one second and not before.
        let mut slow = ScreenBlend::new();
        slow.take_damage(50, 0);
        assert_eq!(slow.damage_percent, DAMAGE_MAX_PERCENT * TICKS_PER_SECOND);
        for _ in 0..59 {
            slow.tick(1);
        }
        assert!(slow.damage_percent > 0);
        slow.tick(1);
        assert_eq!(slow.damage_percent, 0);
    }

    #[optimize(size)]
    #[test]
    fn pickup_flash_is_gold_and_short() {
        let mut blend = ScreenBlend::new();
        blend.pick_up();
        let tint = blend.flash_tint().expect("bonus flash");
        assert!(!tint.average);
        assert!(tint.color.0 > tint.color.2, "{:?}", tint.color);
        for _ in 0..30 {
            blend.tick(1);
        }
        assert_eq!(blend.flash_tint(), None);
    }

    #[optimize(size)]
    #[test]
    fn contents_tint_matches_the_original_shifts() {
        let mut blend = ScreenBlend::new();
        assert_eq!(blend.contents_tint(), None);
        blend.set_contents(CONTENTS_WATER);
        let water = blend.contents_tint().expect("water");
        assert!(water.average);
        assert_eq!(water.color, WATER_COLOR);
        blend.set_contents(CONTENTS_LAVA);
        assert_eq!(blend.contents_tint().expect("lava").color, LAVA_COLOR);
        blend.set_contents(CONTENTS_SLIME);
        assert_eq!(blend.contents_tint().expect("slime").color, SLIME_COLOR);
        // Empty and solid never tint.
        blend.set_contents(-1);
        assert_eq!(blend.contents_tint(), None);
        blend.set_contents(-2);
        assert_eq!(blend.contents_tint(), None);
    }

    #[optimize(size)]
    #[test]
    fn contents_and_flash_are_independent_quads() {
        let mut blend = ScreenBlend::new();
        blend.set_contents(CONTENTS_WATER);
        blend.take_damage(10, 0);
        let contents = blend.contents_tint().expect("contents");
        let flash = blend.flash_tint().expect("flash");
        assert!(contents.average);
        assert!(!flash.average);
    }

    /// The port's level transition, which no id1 code path drives: it must
    /// reach full black, come back to nothing in its advertised time, and hold
    /// at black once it gets there rather than overshooting into a negative
    /// shade that would read as a flash.
    #[optimize(size)]
    #[test]
    fn the_transition_fade_runs_both_ways_and_holds_at_each_end() {
        let ticks = ScreenBlend::TRANSITION_TICKS;
        assert_eq!(ticks, 20);

        let mut blend = ScreenBlend::new();
        assert_eq!(blend.transition_shade(), None);

        blend.fade_in_from_black();
        assert_eq!(blend.transition_shade(), Some(255));
        for _ in 0..ticks - 1 {
            blend.tick(1);
        }
        assert!(blend.transition_shade().is_some());
        blend.tick(1);
        assert_eq!(blend.transition_shade(), None);
        // A fade that has arrived stays arrived.
        blend.tick(1);
        assert_eq!(blend.transition_shade(), None);

        blend.fade_out_to_black();
        blend.tick(1);
        assert!(blend.transition_shade().is_some());
        for _ in 0..ticks - 1 {
            blend.tick(1);
        }
        assert_eq!(blend.transition_shade(), Some(255));
        blend.tick(1);
        assert_eq!(blend.transition_shade(), Some(255));
    }

    #[optimize(size)]
    fn worn(kinds: &[PowerupKind]) -> ScreenBlend {
        let mut powerups = Powerups::new();
        for kind in kinds {
            powerups.arm(*kind);
        }
        let mut blend = ScreenBlend::new();
        blend.set_powerups(powerups);
        blend
    }

    /// Each artifact's own `destcolor` and `percent` from
    /// `V_CalcPowerupCshift`, carried through the 50/50 inversion. The numbers
    /// are hand-worked so a change to either the table or the composition has
    /// to be argued for rather than re-baselined.
    #[optimize(size)]
    #[test]
    fn every_powerup_tints_with_its_own_color_and_percent() {
        // Pentagram: 255 255 0 at 30. Red and green climb by 127*30/255 = 14
        // from the nominal 128 and blue drops by 15, doubled around 128.
        assert_eq!(
            worn(&[PowerupKind::Pentagram]).contents_tint(),
            Some(ScreenTint {
                color: (156, 156, 98),
                average: true
            })
        );
        // Biosuit: 0 255 0 at 20, the weakest of the four, so it moves least.
        assert_eq!(
            worn(&[PowerupKind::Biosuit]).contents_tint(),
            Some(ScreenTint {
                color: (108, 146, 108),
                average: true
            })
        );
        // Ring: a flat 100 100 100 at 100, by far the strongest percent.
        assert_eq!(
            worn(&[PowerupKind::Ring]).contents_tint(),
            Some(ScreenTint {
                color: (106, 106, 106),
                average: true
            })
        );
        // Quad: 0 0 255 at 30, the pentagram's mirror.
        assert_eq!(
            worn(&[PowerupKind::Quad]).contents_tint(),
            Some(ScreenTint {
                color: (98, 98, 156),
                average: true
            })
        );
        // The percent is what separates them: the suit's 20 lands nearer the
        // untinted 128 than the pentagram's 30 does.
        let suit = worn(&[PowerupKind::Biosuit]).contents_tint().expect("suit");
        let pentagram = worn(&[PowerupKind::Pentagram])
            .contents_tint()
            .expect("pentagram");
        assert!((suit.color.1 as i32 - 128).abs() < (pentagram.color.1 as i32 - 128).abs());
    }

    /// `V_CalcPowerupCshift` is one else-if chain, so a stacked player sees
    /// exactly one tint and always the same one.
    #[optimize(size)]
    #[test]
    fn the_powerup_chain_keeps_the_original_priority() {
        let pentagram = worn(&[PowerupKind::Pentagram]).contents_tint();
        let ring = worn(&[PowerupKind::Ring]).contents_tint();
        let biosuit = worn(&[PowerupKind::Biosuit]).contents_tint();
        let quad = worn(&[PowerupKind::Quad]).contents_tint();

        assert_eq!(worn(&PowerupKind::ALL).contents_tint(), pentagram);
        assert_eq!(
            worn(&[PowerupKind::Quad, PowerupKind::Ring, PowerupKind::Biosuit]).contents_tint(),
            biosuit
        );
        assert_eq!(
            worn(&[PowerupKind::Quad, PowerupKind::Ring]).contents_tint(),
            ring
        );
        assert_eq!(worn(&[PowerupKind::Quad]).contents_tint(), quad);
        // Nothing worn is still nothing drawn.
        assert_eq!(worn(&[]).contents_tint(), None);
    }

    /// The case the fifth-quad shortcut would have got wrong: a powerup and a
    /// liquid at once compose into ONE quad, in the original's own order
    /// (`CSHIFT_CONTENTS` before `CSHIFT_POWERUP`), and the answer is neither
    /// of the two colours on its own.
    #[optimize(size)]
    #[test]
    fn a_powerup_in_water_folds_into_the_one_sustained_quad() {
        let mut blend = worn(&[PowerupKind::Quad]);
        blend.set_contents(CONTENTS_WATER);
        let tint = blend.contents_tint().expect("water and quad");
        assert!(tint.average);
        // 128 -> water at 128 -> quad at 30, each `>> 8`, doubled around 128.
        assert_eq!(tint.color, (98, 54, 88));
        // Water alone is browner and the quad alone is bluer; the pair sits
        // between them, blue pulled up out of water's 50 and red pulled down.
        assert_eq!(
            {
                let mut water = ScreenBlend::new();
                water.set_contents(CONTENTS_WATER);
                water.contents_tint().expect("water").color
            },
            WATER_COLOR
        );
        assert!(tint.color.2 > WATER_COLOR.2);
        assert!(tint.color.0 < WATER_COLOR.0);
        // Still one quad, and the flash quad is untouched by any of it.
        blend.take_damage(10, 0);
        assert!(!blend.flash_tint().expect("flash").average);
    }

    /// Dropping a powerup must put the sustained quad back exactly where it
    /// was, not leave a stale tint on screen.
    #[optimize(size)]
    #[test]
    fn losing_the_last_powerup_restores_the_plain_contents_tint() {
        let mut blend = worn(&[PowerupKind::Ring]);
        blend.set_contents(CONTENTS_LAVA);
        assert_ne!(
            blend.contents_tint().expect("lava and ring").color,
            LAVA_COLOR
        );
        blend.set_powerups(Powerups::new());
        assert_eq!(blend.contents_tint().expect("lava").color, LAVA_COLOR);
    }

    /// The transition is a separate quad from the palette blends, so it must
    /// not disturb either of them.
    #[optimize(size)]
    #[test]
    fn the_transition_leaves_the_palette_blends_alone() {
        let mut blend = ScreenBlend::new();
        blend.set_contents(CONTENTS_WATER);
        blend.take_damage(10, 0);
        blend.fade_out_to_black();
        blend.tick(1);
        assert_eq!(blend.contents_tint().expect("contents").color, WATER_COLOR);
        assert!(blend.flash_tint().is_some());
    }

    /// Every flash is red-heavy, so a full stack overruns the red channel and
    /// has to SATURATE. The channel type alone cannot show that: a wrapping
    /// sum is also a valid `u8`, and it would read as a dark flash right when
    /// the screen should be at its brightest.
    #[optimize(size)]
    #[test]
    fn the_flash_blends_stack_additively_without_wrapping() {
        let mut damage_only = ScreenBlend::new();
        damage_only.take_damage(200, 0);
        let single = damage_only.flash_tint().expect("flash");
        // 255 * 150 / 255, the damage ramp at its own ceiling.
        assert_eq!(single.color.0, 150);

        let mut blend = ScreenBlend::new();
        blend.take_damage(200, 0);
        blend.pick_up();
        let stacked = blend.flash_tint().expect("flash");
        // 150 red from the damage ramp plus the pickup's 215 * 50 / 255.
        assert_eq!(stacked.color.0, 192);
        assert!(
            stacked.color.0 > single.color.0,
            "stacking wrapped: {:?} is below the single damage flash {:?}",
            stacked.color,
            single.color
        );
        // The pickup carries the other two channels on its own: 186 and 69
        // scaled by the same 50 percent. Damage adds nothing there.
        assert_eq!(stacked.color.1, 36);
        assert_eq!(stacked.color.2, 13);
        // The 255 clamp in `flash_tint` is now unreachable through the public
        // API: with the dlight stand-ins gone the two remaining blends peak at
        // 192 together. It stays as a guard, not as live arithmetic.
    }
}
