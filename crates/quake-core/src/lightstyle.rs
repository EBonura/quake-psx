//! Quake's predefined light styles and `R_AnimateLight`.
//!
//! A cooked face and a cooked leaf each carry two style indices. The renderer
//! multiplies its two lightmap samples by the style values, so the whole of
//! Quake's animated lighting is one 65-entry `u16` table that has to be
//! rewritten ten times a second. This port shipped that table frozen at a
//! constant 256, which is why every torch, fire and fluorescent tube in
//! Episode 1 stood still.
//!
//! The values are the original's exactly: `d_lightstylevalue[j] = (c - 'a')
//! * 22`, sampled at `(int)(time * 10)` and wrapped by the pattern length, so
//! `'a'` is off, `'m'` is the authored normal 264 and `'z'` is a little over
//! double.
//!
//! Style 0 stays at this port's own 256 rather than the original's 264. A face
//! whose only style is 0 has its light BAKED at cook time
//! (`quake-cook/src/geometry.rs`), so 0 is not an animation channel here, it
//! is the constant the un-baked half of a two-style face is scaled by. Moving
//! it would rescale every such face by 3% for no visible gain and would move
//! every pinned frame hash.
//!
//! Styles 32 and up are the switchable ones: an authored `light` with a
//! `targetname` is toggled by its trigger, exactly like the original's
//! `light_use`, and never animates on its own.

/// Cooked style indices run 0..=63; 64 is the cooker's "no style" slot, which
/// the renderer holds at zero.
pub const STYLE_COUNT: usize = 64;
/// The renderer's dummy slot, always dark.
pub const DUMMY_STYLE: usize = STYLE_COUNT;
/// This port's constant for style 0. See the module note.
pub const NORMAL_VALUE: u16 = 256;
/// The original's scale: one letter step is 22.
const LETTER_SCALE: u16 = 22;
/// `'m' - 'a'`, the authored normal brightness.
const LETTER_NORMAL: u16 = 12;
/// A switchable light that is on, in the original's own units.
pub const SWITCHED_ON_VALUE: u16 = LETTER_NORMAL * LETTER_SCALE;
/// A switchable light that is off.
pub const SWITCHED_OFF_VALUE: u16 = 0;
/// The original samples the pattern at `(int)(cl.time * 10)`.
pub const ANIMATION_HZ: u32 = 10;
/// First switchable style index. Below this the styles animate.
pub const FIRST_SWITCHABLE_STYLE: usize = 32;

/// The original's twelve predefined patterns, `world.qc` order.
const PATTERNS: [&[u8]; 12] = [
    // 0 normal
    b"m",
    // 1 flicker
    b"mmnmmommommnonmmonqnmmo",
    // 2 slow strong pulse
    b"abcdefghijklmnopqrrqponmlkjihgfedcba",
    // 3 candle
    b"mmmmmaaaaammmmmaaaaaabcdefgabcdefg",
    // 4 fast strobe
    b"mamamamamama",
    // 5 gentle pulse
    b"jklmnopqrstuvwxyzyxwvutsrqponmlkj",
    // 6 flicker
    b"nmonqnmomnmomomno",
    // 7 candle
    b"mmmaaaabcdefgmmmmaaaammmaamm",
    // 8 candle
    b"mmmaaammmaaammmabcdefaaaammmmabcdefmmmaaaa",
    // 9 slow strobe
    b"aaaaaaaazzzzzzzz",
    // 10 fluorescent flicker
    b"mmamammmmammamamaaamammma",
    // 11 slow pulse, never fully dark
    b"abcdefghijklmnopqrrqponmlkjihgfedcba",
];

/// One predefined style sampled at an animation tick.
///
/// `tick` is `(time * 10)` truncated, so the caller owns the clock and this
/// stays a pure function that host tests can pin.
#[optimize(size)]
pub const fn animated_value(style: usize, tick: u32) -> u16 {
    if style == 0 || style >= PATTERNS.len() {
        return NORMAL_VALUE;
    }
    let pattern = PATTERNS[style];
    let index = (tick % pattern.len() as u32) as usize;
    (pattern[index] - b'a') as u16 * LETTER_SCALE
}

/// A switchable light's value. The original writes `"m"` or `"a"`.
#[optimize(size)]
pub const fn switched_value(on: bool) -> u16 {
    if on {
        SWITCHED_ON_VALUE
    } else {
        SWITCHED_OFF_VALUE
    }
}

/// `light`'s `START_OFF` spawnflag: the original spawns such a light dark and
/// its first use turns it on.
pub const SPAWNFLAG_LIGHT_START_OFF: u16 = 1;

/// The renderer's whole style table, rewritten for one animation tick.
///
/// Switchable styles are left exactly as the caller last set them, because the
/// original never animates them: `light_use` owns 32 and up.
#[optimize(size)]
pub fn animate(values: &mut [u16; DUMMY_STYLE + 1], tick: u32) {
    let mut style = 1usize;
    while style < FIRST_SWITCHABLE_STYLE {
        values[style] = animated_value(style, tick);
        style += 1;
    }
}

/// A fresh table: style 0 at this port's normal, every predefined style at its
/// own tick zero, every switchable style on, and the dummy slot dark.
///
/// Switchable styles start ON because the original's `light` spawn function
/// writes `"m"` unless `START_OFF` is authored, and the loader turns the
/// authored dark ones off as it walks the entity lump.
#[optimize(size)]
pub fn initial_values() -> [u16; DUMMY_STYLE + 1] {
    let mut values = [NORMAL_VALUE; DUMMY_STYLE + 1];
    let mut style = FIRST_SWITCHABLE_STYLE;
    while style < STYLE_COUNT {
        values[style] = SWITCHED_ON_VALUE;
        style += 1;
    }
    animate(&mut values, 0);
    values[DUMMY_STYLE] = 0;
    values
}

/// One entity's `R_LightPoint` sample from a cooked leaf.
///
/// Both world entities and the camera-relative weapon use this exact scalar
/// path in the original renderer.
#[optimize(size)]
pub fn sample_leaf(lightmap: [u8; 2], styles: [u8; 2], table: &[u16; DUMMY_STYLE + 1]) -> u8 {
    let value = |index: u8| -> u32 { u32::from(*table.get(index as usize).unwrap_or(&0)) };
    ((u32::from(lightmap[0]) * value(styles[0]) + u32::from(lightmap[1]) * value(styles[1])) >> 8)
        .min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn letters_match_the_original_scale() {
        // 'a' is off, 'm' is the authored normal, 'z' is the brightest.
        assert_eq!(animated_value(4, 0), LETTER_NORMAL * LETTER_SCALE);
        assert_eq!(animated_value(4, 1), 0);
        assert_eq!(animated_value(9, 0), 0);
        assert_eq!(animated_value(9, 8), 25 * LETTER_SCALE);
    }

    #[optimize(size)]
    #[test]
    fn style_zero_is_this_ports_constant() {
        assert_eq!(animated_value(0, 0), NORMAL_VALUE);
        assert_eq!(animated_value(0, 7), NORMAL_VALUE);
    }

    #[optimize(size)]
    #[test]
    fn patterns_wrap_by_their_own_length() {
        // Style 4 is "mamamamamama", twelve entries.
        assert_eq!(animated_value(4, 12), animated_value(4, 0));
        assert_eq!(animated_value(4, 13), animated_value(4, 1));
        // Style 1 is twenty-three entries and must not share style 4's period.
        assert_eq!(animated_value(1, 23), animated_value(1, 0));
        assert_ne!(animated_value(1, 12), animated_value(1, 0));
    }

    #[optimize(size)]
    #[test]
    fn every_predefined_style_stays_inside_the_table() {
        for style in 0..PATTERNS.len() {
            for tick in 0..64 {
                let value = animated_value(style, tick);
                assert!(value <= 25 * LETTER_SCALE, "style {style} tick {tick}");
            }
        }
        // An index past the predefined set is the authored-but-unanimated
        // case and must read as normal, never panic.
        assert_eq!(animated_value(63, 5), NORMAL_VALUE);
    }

    #[optimize(size)]
    #[test]
    fn animate_leaves_switchable_styles_alone() {
        let mut values = initial_values();
        values[32] = switched_value(false);
        values[40] = switched_value(true);
        animate(&mut values, 7);
        assert_eq!(values[32], SWITCHED_OFF_VALUE);
        assert_eq!(values[40], SWITCHED_ON_VALUE);
        assert_eq!(values[0], NORMAL_VALUE);
        assert_eq!(values[DUMMY_STYLE], 0);
    }

    #[optimize(size)]
    #[test]
    fn initial_table_is_lit() {
        let values = initial_values();
        assert_eq!(values[0], NORMAL_VALUE);
        assert_eq!(values[32], SWITCHED_ON_VALUE);
        assert_eq!(values[STYLE_COUNT - 1], SWITCHED_ON_VALUE);
        assert_eq!(values[DUMMY_STYLE], 0);
        // Every animated style starts at its pattern's first letter.
        assert_eq!(values[2], 0);
        assert_eq!(values[4], SWITCHED_ON_VALUE);
    }

    #[optimize(size)]
    #[test]
    fn animation_actually_moves() {
        // The whole point: a style must change value across a second.
        let mut early = initial_values();
        animate(&mut early, 0);
        let mut late = initial_values();
        animate(&mut late, 5);
        assert_ne!(early[1], late[1]);
        assert_ne!(early[2], late[2]);
        // Style 10's fluorescent pattern happens to hold 'm' across ticks 0
        // and 5, so its blink is checked where it actually falls dark.
        let mut dark = initial_values();
        animate(&mut dark, 2);
        assert_ne!(early[10], dark[10]);
        assert_eq!(dark[10], 0);
    }

    #[optimize(size)]
    #[test]
    fn leaf_sample_matches_the_pinned_e1m1_camera() {
        let values = initial_values();
        assert_eq!(sample_leaf([120, 0], [0, DUMMY_STYLE as u8], &values), 120);
        assert_eq!(sample_leaf([255, 255], [63, 63], &values), 255);
        assert_eq!(sample_leaf([255, 255], [255, 255], &values), 0);
    }
}
