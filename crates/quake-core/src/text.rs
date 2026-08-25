//! Allocation-free layout for Quake's byte-oriented console font.

/// One visible console-font glyph and its screen-space origin.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TextGlyph {
    pub character: u8,
    pub x: i16,
    pub y: i16,
}

/// Iterate visible glyphs while preserving Quake's spaces and newlines.
///
/// The original renderer resets X and advances one glyph row for `\n`.
/// Authored centerprints rely on this: their apparent word wrapping is
/// already encoded as newline bytes in the BSP entity string.
pub struct TextGlyphs<'a> {
    bytes: &'a [u8],
    cursor: usize,
    origin_x: i16,
    x: i16,
    y: i16,
    glyph_width: i16,
    glyph_height: i16,
}

impl<'a> TextGlyphs<'a> {
    #[optimize(size)]
    pub const fn new(
        text: &'a str,
        x: i16,
        y: i16,
        glyph_width: i16,
        glyph_height: i16,
    ) -> Self {
        Self {
            bytes: text.as_bytes(),
            cursor: 0,
            origin_x: x,
            x,
            y,
            glyph_width,
            glyph_height,
        }
    }
}

impl Iterator for TextGlyphs<'_> {
    type Item = TextGlyph;

    #[optimize(size)]
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(&character) = self.bytes.get(self.cursor) {
            self.cursor += 1;
            match character {
                b' ' => self.x += self.glyph_width,
                b'\n' => {
                    self.x = self.origin_x;
                    self.y += self.glyph_height;
                }
                _ => {
                    let glyph = TextGlyph {
                        character,
                        x: self.x,
                        y: self.y,
                    };
                    self.x += self.glyph_width;
                    return Some(glyph);
                }
            }
        }
        None
    }
}

/// Center the first authored line, matching the C Quake-PSX centerprint.
#[optimize(size)]
pub fn centered_first_line_x(
    text: &str,
    screen_width: i16,
    glyph_width: i16,
    max_columns: usize,
) -> i16 {
    let columns = text
        .bytes()
        .take_while(|&character| character != b'\n')
        .take(max_columns)
        .count() as i16;
    (screen_width - columns * glyph_width) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    #[test]
    fn authored_shareware_message_keeps_all_three_lines_inside_the_screen() {
        const MESSAGE: &str =
            "This is the shareware version.\n\nPlease register!\n\nCall 1-800-idgames today!";
        let x = centered_first_line_x(MESSAGE, 320, 8, 40);
        assert_eq!(x, 40);

        let mut count = 0;
        let mut saw_first = false;
        let mut saw_register = false;
        let mut saw_phone = false;
        for glyph in TextGlyphs::new(MESSAGE, x, 96, 8, 8) {
            count += 1;
            saw_first |= glyph == TextGlyph { character: b'T', x: 40, y: 96 };
            saw_register |= glyph == TextGlyph { character: b'P', x: 40, y: 112 };
            saw_phone |= glyph == TextGlyph { character: b'C', x: 40, y: 128 };
            assert!(glyph.x >= 0 && glyph.x + 8 <= 320);
        }
        assert_eq!(count, 64);
        assert!(saw_first && saw_register && saw_phone);
    }
}
