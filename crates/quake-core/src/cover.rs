//! The boot logo's run-length code.
//!
//! The Bonnie Studios mark is a 128x128 4bpp image, 4096 sixteen-bit words, and
//! it is only ever needed once, at boot. As plain data it would stay in the
//! executable's RAM image for the whole session; coded as below it takes 57
//! percent of that and expands a word at a time straight into VRAM.
//!
//! Token byte: `0nnnnnnn` is `n + 1` literal words (little-endian, two bytes
//! each) and `1nnnnnnn` is `n + 3` copies of the one word that follows.

/// Expand a coded stream into its words. A truncated stream ends early.
pub fn decode_words(coded: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let mut at = 0usize;
    let mut run = 0usize;
    let mut literal = 0usize;
    let mut value = 0u16;
    core::iter::from_fn(move || {
        let word = |at: usize| -> Option<u16> {
            Some(u16::from_le_bytes([*coded.get(at)?, *coded.get(at + 1)?]))
        };
        if run > 0 {
            run -= 1;
            return Some(value);
        }
        if literal > 0 {
            literal -= 1;
            let next = word(at)?;
            at += 2;
            return Some(next);
        }
        let token = *coded.get(at)?;
        at += 1;
        if token & 0x80 != 0 {
            value = word(at)?;
            at += 2;
            run = usize::from(token & 0x7f) + 2;
            Some(value)
        } else {
            literal = usize::from(token);
            let next = word(at)?;
            at += 2;
            Some(next)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_literals_expand_in_order() {
        // 3 x 0x0000, literal 0x1234 0x5678, 4 x 0xBEEF.
        let coded = [
            0x80, 0x00, 0x00, 0x01, 0x34, 0x12, 0x78, 0x56, 0x81, 0xEF, 0xBE,
        ];
        let mut words = [0u16; 9];
        assert_eq!(decode_words(&coded).count(), 9);
        for (slot, word) in words.iter_mut().zip(decode_words(&coded)) {
            *slot = word;
        }
        assert_eq!(
            words,
            [0, 0, 0, 0x1234, 0x5678, 0xBEEF, 0xBEEF, 0xBEEF, 0xBEEF]
        );
    }

    #[test]
    fn a_truncated_stream_stops_without_panicking() {
        assert_eq!(decode_words(&[0x81, 0xEF]).count(), 0);
        assert_eq!(decode_words(&[0x01, 0x34, 0x12]).count(), 1);
        assert_eq!(decode_words(&[]).count(), 0);
    }
}
