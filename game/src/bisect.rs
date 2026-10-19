//! BISECT diagnostic disc (feature `bisect`): the switches one variant of the
//! bisect disc flips. Never in a shipping image.
//!
//! The bisect disc boots a small menu program that chain-loads one of seven
//! complete builds of this game from the world pack, each built with
//! `BISECT_SUB=<letter>`:
//!
//! - A as shipped (plus the v6 panel, strip and auto E1M1)
//! - D no code overlays (feature `monster-code-overlay` off), so no runtime flush
//! - F overlays kept, every flush through the BIOS `A(44h) FlushCache`
//! - G overlays kept, every flush first waits for the present walk and GPU to be idle
//! - B blocking present, no VBlank-kicked queue (feature `blocking-present`)
//! - C strict VRAM uploads: GPU and DMA2 idle before and after, interrupts masked during
//! - E scratchpad stacks off (`ScratchpadStack::run` calls its closure in place)
//!
//! Each build has its own overlay packages in the pack, under chunk ids shifted by
//! `QUAKE_OVERLAY_CHUNK_BASE` (see `monster_code.rs`).

const SUB: &str = match option_env!("BISECT_SUB") {
    Some(letter) => letter,
    None => "A",
};

/// This build's variant letter.
pub const fn letter() -> u8 {
    SUB.as_bytes()[0]
}

/// Variant C: every VRAM upload runs with the GPU and DMA2 provably idle and
/// interrupts masked.
pub const fn strict_uploads() -> bool {
    letter() == b'C'
}


/// Name the variant for the panel and status line. Call once at boot.
pub fn boot() {
    psx_rt::bisect::set_variant(letter());
}
