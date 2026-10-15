//! The boot logo's coded image expands to exactly the image it replaced.
//!
//! The committed table is the guest's own `game/src/bonnie.rs`, compiled here
//! for the host; the FNV-1a digest of the 4096 original words is pinned beside
//! the table, so a re-encode that drifts is caught.

#[path = "../../../game/src/bonnie.rs"]
#[allow(dead_code)]
mod bonnie;

use quake_core::cover::decode_words;

fn fnv1a(words: impl Iterator<Item = u16>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[test]
fn the_coded_cover_expands_to_the_original_image() {
    let words: Vec<u16> = decode_words(&bonnie::COVER_BONNIE_RLE).collect();
    assert_eq!(words.len(), bonnie::COVER_BONNIE_WORDS);
    assert_eq!(fnv1a(words.iter().copied()), bonnie::COVER_BONNIE_FNV1A);
    // 128x128 at 4bpp is 32 words a row.
    assert_eq!(words.len(), 128 * 32);
    // The coding earns its place: well under the plain 8 KiB.
    assert!(bonnie::COVER_BONNIE_RLE.len() < 5000);
}
