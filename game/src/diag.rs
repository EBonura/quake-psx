//! Console diagnostic helpers for the DIAG disc only (feature `console-diag`).
//!
//! `step` names the point the game loop reached, with the frame number in the
//! low 24 bits of the id, for the SDK stop panel. `status` keeps a live line
//! (frame, VBlank count, pad state) that the notify area draws, so a photo
//! tells a running loop from a stopped one and a dead pad from both.

use core::ptr::addr_of_mut;

static mut FRAME: u32 = 0;
static mut LINE: [u8; 36] = *b"F000000 V0000 B0000 M0 L00 00 R00 00";

pub fn next_frame() {
    // SAFETY: the game loop is the only writer.
    unsafe { FRAME = FRAME.wrapping_add(1) };
}

pub fn frame() -> u32 {
    // SAFETY: single reader/writer.
    unsafe { FRAME }
}

pub fn step(phase: u32, text: &str) {
    psx_io::diag::step((phase << 24) | (frame() & 0x00FF_FFFF), text);
}

fn hex(dst: &mut [u8], mut value: u32) {
    let mut at = dst.len();
    while at > 0 {
        at -= 1;
        let nibble = (value & 15) as u8;
        dst[at] = if nibble < 10 { b'0' + nibble } else { b'A' + nibble - 10 };
        value >>= 4;
    }
}

/// Refresh the status text; layout `F<frame> V<vblank> B<buttons> M<mode> L<x> <y> R<x> <y>`.
pub fn status(held: u16, movement: [i16; 2], look: [i16; 2], analog: bool) -> &'static str {
    // SAFETY: single writer; the returned view is read only by the renderer
    // between frames and a torn glyph in a debug line is harmless.
    unsafe {
        let line = &mut *addr_of_mut!(LINE);
        hex(&mut line[1..7], frame());
        hex(&mut line[9..13], psx_rt::interrupts::vblank_count());
        hex(&mut line[15..19], u32::from(held));
        line[21] = if analog { b'A' } else { b'D' };
        hex(&mut line[24..26], movement[0] as u8 as u32);
        hex(&mut line[27..29], movement[1] as u8 as u32);
        hex(&mut line[31..33], look[0] as u8 as u32);
        hex(&mut line[34..36], look[1] as u8 as u32);
        core::str::from_utf8_unchecked(&*addr_of_mut!(LINE))
    }
}

/// Proof the stop panel works on this disc: hold L1+R1+L2+R2, then TRIANGLE
/// spins with interrupts on (stall panel after four seconds) or CROSS takes a
/// BREAK (fault panel at once).
pub fn selftest(held: u16) {
    use psx_pad::button::{CROSS, L1, L2, R1, R2, TRIANGLE};
    let chord = L1 | R1 | L2 | R2;
    if held & chord == chord {
        if held & TRIANGLE != 0 {
            step(0x70, "SELFTEST SPIN");
            loop {
                core::hint::spin_loop();
            }
        }
        if held & CROSS != 0 {
            step(0x71, "SELFTEST BREAK");
            // SAFETY: deliberate BREAK into the fault panel.
            unsafe { core::arch::asm!("break") };
        }
    }
}
