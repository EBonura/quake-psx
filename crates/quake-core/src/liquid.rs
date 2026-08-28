//! Quake's original 64x64 turbulent liquid tile.
//!
//! The software renderer does not merely wobble a polygon's corners. It
//! resamples every texel through two crossed sine waves (`R_GenTurbTile` and
//! `D_DrawTurbulent8Span`), with a 128-step cycle, eight-texel amplitude and
//! twenty phase steps per second. Keeping this platform-independent makes the
//! cooker/host tests and the PS1 renderer use one exact integer policy.

use psx_math::SIN_TABLE;

pub const LIQUID_TILE_SIDE: usize = 64;
pub const LIQUID_TILE_BYTES: usize = LIQUID_TILE_SIDE * LIQUID_TILE_SIDE;
pub const LIQUID_CYCLE: usize = 128;

const fn turbulence_table() -> [u8; LIQUID_CYCLE] {
    let mut output = [0; LIQUID_CYCLE];
    let mut index = 0;
    while index < LIQUID_CYCLE {
        // Original Quake stores `(8 + sin(angle) * 8)` as Q16 and then
        // arithmetic-shifts it. Sampling PSoXide's 256-entry Q12 sine table
        // at every other entry gives the same 128 phases without guest float.
        let fixed = 8 * 4096 + SIN_TABLE[index * 2] as i32 * 8;
        output[index] = (fixed >> 12) as u8;
        index += 1;
    }
    output
}

const TURBULENCE: [u8; LIQUID_CYCLE] = turbulence_table();

const fn doubled_turbulence_table() -> [u8; LIQUID_CYCLE * 2] {
    let mut output = [0; LIQUID_CYCLE * 2];
    let mut index = 0;
    while index < output.len() {
        output[index] = TURBULENCE[index & (LIQUID_CYCLE - 1)];
        index += 1;
    }
    output
}

// Every tile row and column reads 64 consecutive phase entries. Duplicating
// this tiny table removes a mask and address reconstruction from every texel
// in the PS1's dense resampler.
static TURBULENCE_DOUBLE: [u8; LIQUID_CYCLE * 2] = doubled_turbulence_table();

/// Original Quake advances turbulence at 20 Hz over its 60 Hz game clock.
#[inline]
pub const fn phase_from_tick(tick_60hz: u32) -> u8 {
    ((tick_60hz / 3) & (LIQUID_CYCLE as u32 - 1)) as u8
}

/// Double-buffered liquid tiles: bit `index` set means liquid `index` is
/// currently sampling its ALTERNATE atlas copy. The renderer must warp into
/// the other copy and flip only once that upload has actually completed.
#[inline]
pub const fn alternate_tile_is_active(alternate_mask: u8, index: usize) -> bool {
    alternate_mask & (1 << index) != 0
}

/// Commit one successful inactive-tile upload: the freshly written copy
/// becomes the active one. A failed upload must simply not call this, which
/// keeps the old tile live and the retry targeting the same inactive copy.
#[inline]
#[must_use]
pub const fn commit_tile_upload(alternate_mask: u8, index: usize) -> u8 {
    alternate_mask ^ (1 << index)
}

/// Resample one 64x64 indexed tile using Quake's crossed sine displacement.
///
/// Returns false rather than accepting a partial tile. Source and destination
/// must not overlap; the renderer retains an immutable source tile and writes
/// a separate bounded upload buffer.
#[inline(never)]
pub fn warp_tile_64(source: &[u8], destination: &mut [u8], phase: u8) -> bool {
    if source.len() != LIQUID_TILE_BYTES || destination.len() != LIQUID_TILE_BYTES {
        return false;
    }
    #[cfg(target_arch = "mips")]
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            "addu  $24, $7, $6",
            "move  $8, $zero",
            "addiu $25, $zero, 64",
            "addu  $11, $24, $8",
            "2:",
            "lbu   $10, 0($11)",
            "move  $11, $24",
            // Two texels share one loop branch and counter update. Source
            // coordinates and write order remain exactly the original dense
            // Quake resample; only the MIPS-I schedule is unrolled.
            "addiu $9, $zero, 32",
            "3:",
            "lbu   $12, 0($11)",
            "addu  $13, $8, $12",
            "andi  $13, $13, 63",
            "sll   $13, $13, 6",
            "or    $13, $13, $10",
            "addu  $15, $4, $13",
            "lbu   $14, 0($15)",
            "sb    $14, 0($5)",
            "addiu $10, $10, 1",
            "andi  $10, $10, 63",
            "lbu   $12, 1($11)",
            "addu  $13, $8, $12",
            "andi  $13, $13, 63",
            "sll   $13, $13, 6",
            "or    $13, $13, $10",
            "addu  $15, $4, $13",
            "lbu   $14, 0($15)",
            "sb    $14, 1($5)",
            "addiu $10, $10, 1",
            "andi  $10, $10, 63",
            "addiu $11, $11, 2",
            "addiu $9, $9, -1",
            "bnez  $9, 3b",
            "addiu $5, $5, 2",
            "addiu $8, $8, 1",
            "bne   $8, $25, 2b",
            "addu  $11, $24, $8",
            ".set reorder",
            in("$4") source.as_ptr(),
            inout("$5") destination.as_mut_ptr() => _,
            in("$6") u32::from(phase & (LIQUID_CYCLE as u8 - 1)),
            in("$7") TURBULENCE_DOUBLE.as_ptr(),
            lateout("$8") _,
            lateout("$9") _,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") _,
            lateout("$13") _,
            lateout("$14") _,
            lateout("$15") _,
            lateout("$24") _,
            lateout("$25") _,
            options(nostack),
        );
        return true;
    }

    #[cfg(not(target_arch = "mips"))]
    {
        let phase = phase as usize & (LIQUID_CYCLE - 1);
        let mut y = 0usize;
        while y < LIQUID_TILE_SIDE {
            let x_offset = TURBULENCE_DOUBLE[phase + y] as usize;
            let mut x = 0usize;
            while x < LIQUID_TILE_SIDE {
                let source_x = (x + x_offset) & (LIQUID_TILE_SIDE - 1);
                let source_y = (y + TURBULENCE_DOUBLE[phase + x] as usize) & (LIQUID_TILE_SIDE - 1);
                // Lengths and both masked coordinates were validated above.
                unsafe {
                    *destination.get_unchecked_mut(y * LIQUID_TILE_SIDE + x) =
                        *source.get_unchecked(source_y * LIQUID_TILE_SIDE + source_x);
                }
                x += 1;
            }
            y += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_original_clock_advances_one_phase_every_three_ticks_and_wraps() {
        assert_eq!(phase_from_tick(0), 0);
        assert_eq!(phase_from_tick(2), 0);
        assert_eq!(phase_from_tick(3), 1);
        assert_eq!(phase_from_tick(383), 127);
        assert_eq!(phase_from_tick(384), 0);
    }

    #[test]
    fn constant_tiles_remain_constant_and_partial_tiles_fail_closed() {
        let source = [37; LIQUID_TILE_BYTES];
        let mut destination = [0; LIQUID_TILE_BYTES];
        assert!(warp_tile_64(&source, &mut destination, 91));
        assert_eq!(destination, source);
        assert!(!warp_tile_64(
            &source[..LIQUID_TILE_BYTES - 1],
            &mut destination,
            0
        ));
        assert!(!warp_tile_64(
            &source,
            &mut destination[..LIQUID_TILE_BYTES - 1],
            0
        ));
    }

    #[test]
    fn crossed_sine_warp_is_dense_periodic_and_phase_sensitive() {
        let mut source = [0; LIQUID_TILE_BYTES];
        for (index, pixel) in source.iter_mut().enumerate() {
            *pixel = (index ^ (index >> 6)) as u8;
        }
        let mut phase0 = [0; LIQUID_TILE_BYTES];
        let mut phase1 = [0; LIQUID_TILE_BYTES];
        let mut wrapped = [0; LIQUID_TILE_BYTES];
        assert!(warp_tile_64(&source, &mut phase0, 0));
        assert!(warp_tile_64(&source, &mut phase1, 1));
        assert!(warp_tile_64(&source, &mut wrapped, 128));
        assert_eq!(phase0, wrapped);
        assert_ne!(phase0, phase1);
        assert!(phase0.iter().zip(source).filter(|(a, b)| **a != *b).count() > 3_500);
    }

    #[test]
    fn double_buffer_upload_always_targets_the_inactive_tile() {
        for mask in 0..=u8::MAX {
            for index in 0..8 {
                let upload_hits_alternate = !alternate_tile_is_active(mask, index);
                // The tile being sampled and the tile being rewritten must
                // never be the same VRAM rectangle.
                assert_ne!(upload_hits_alternate, alternate_tile_is_active(mask, index));
                // After the commit the freshly written tile is the active one.
                let committed = commit_tile_upload(mask, index);
                assert_eq!(
                    alternate_tile_is_active(committed, index),
                    upload_hits_alternate
                );
                // Other liquids keep their active tile across the flip.
                for other in 0..8 {
                    if other != index {
                        assert_eq!(
                            alternate_tile_is_active(committed, other),
                            alternate_tile_is_active(mask, other)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn failed_uploads_retain_the_active_tile_until_a_commit_lands() {
        let mask = 0b0000_0100u8;
        assert!(alternate_tile_is_active(mask, 2));
        // A failed upload performs no commit: the active tile is unchanged
        // and the next attempt still targets the same inactive copy.
        assert!(alternate_tile_is_active(mask, 2));
        assert!(!alternate_tile_is_active(commit_tile_upload(mask, 2), 2));
    }

    #[test]
    fn tiles_alternate_deterministically_at_the_exact_quake_20hz_phase() {
        let mut mask = 0u8;
        let mut last_phase = phase_from_tick(0);
        let mut previous_active = alternate_tile_is_active(mask, 0);
        let mut commits = 0u32;
        for tick in 1..=600u32 {
            let phase = phase_from_tick(tick);
            if phase == last_phase {
                continue;
            }
            last_phase = phase;
            mask = commit_tile_upload(mask, 0);
            let active = alternate_tile_is_active(mask, 0);
            assert_ne!(active, previous_active, "tiles must strictly alternate");
            previous_active = active;
            commits += 1;
        }
        // 600 ticks at 60 Hz cover ten seconds: exactly 200 phase steps.
        assert_eq!(commits, 200);
    }

    #[test]
    fn optimized_table_addressing_matches_the_direct_formula_for_every_phase() {
        let mut source = [0; LIQUID_TILE_BYTES];
        for (index, pixel) in source.iter_mut().enumerate() {
            *pixel = index.wrapping_mul(73).wrapping_add(index >> 5) as u8;
        }
        let mut optimized = [0; LIQUID_TILE_BYTES];
        let mut reference = [0; LIQUID_TILE_BYTES];
        for phase in 0..LIQUID_CYCLE {
            assert!(warp_tile_64(&source, &mut optimized, phase as u8));
            for y in 0..LIQUID_TILE_SIDE {
                for x in 0..LIQUID_TILE_SIDE {
                    let source_x = (x + TURBULENCE[(phase + y) & (LIQUID_CYCLE - 1)] as usize)
                        & (LIQUID_TILE_SIDE - 1);
                    let source_y = (y + TURBULENCE[(phase + x) & (LIQUID_CYCLE - 1)] as usize)
                        & (LIQUID_TILE_SIDE - 1);
                    reference[y * LIQUID_TILE_SIDE + x] =
                        source[source_y * LIQUID_TILE_SIDE + source_x];
                }
            }
            assert_eq!(optimized, reference, "phase {phase}");
        }
    }
}
