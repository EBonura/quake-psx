//! The byte format of a saved game.
//!
//! Plain little-endian scalars written field by field, never a copy of a
//! struct: the layout is a file format that has to outlive any padding or
//! field-order decision the compiler makes, and a card that holds a save from
//! another build must be refused, not misread.
//!
//! A save is `MAGIC`, `VERSION`, a payload and a trailing FNV-1a over
//! everything before it. The reader never panics: running off the end or
//! meeting a value no state machine can hold sets a sticky flag and every
//! later read returns zero, so a truncated or hand-edited save fails the final
//! [`Reader::finish`] instead of producing a plausible wrong world.

use quake_formats::{Vec3I16, Vec3I32};

/// `QPSV`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"QPSV");
/// Bumped whenever any serialised field changes.
pub const VERSION: u16 = 1;

/// Cursor writer over a fixed buffer. Writing past the end sets
/// [`Writer::overflowed`] and drops the bytes; [`Writer::finish`] reports it.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
    overflowed: bool,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            at: 0,
            overflowed: false,
        }
    }

    pub const fn len(&self) -> usize {
        self.at
    }

    pub const fn is_empty(&self) -> bool {
        self.at == 0
    }

    pub const fn overflowed(&self) -> bool {
        self.overflowed
    }

    #[inline(never)]
    pub fn u8(&mut self, value: u8) {
        match self.buf.get_mut(self.at) {
            Some(slot) => {
                *slot = value;
                self.at += 1;
            }
            None => self.overflowed = true,
        }
    }

    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    #[inline(never)]
    pub fn u16(&mut self, value: u16) {
        self.u8(value as u8);
        self.u8((value >> 8) as u8);
    }

    pub fn i16(&mut self, value: i16) {
        self.u16(value as u16);
    }

    #[inline(never)]
    pub fn u32(&mut self, value: u32) {
        self.u16(value as u16);
        self.u16((value >> 16) as u16);
    }

    pub fn i32(&mut self, value: i32) {
        self.u32(value as u32);
    }

    pub fn vec3(&mut self, value: Vec3I32) {
        self.i32(value.x);
        self.i32(value.y);
        self.i32(value.z);
    }

    pub fn vec3_16(&mut self, value: Vec3I16) {
        self.i16(value.x);
        self.i16(value.y);
        self.i16(value.z);
    }

    /// Write the trailing checksum and return the payload length, or `None`
    /// when anything was dropped.
    pub fn finish(mut self) -> Option<usize> {
        let sum = checksum(&self.buf[..self.at.min(self.buf.len())]);
        self.u32(sum);
        (!self.overflowed).then_some(self.at)
    }
}

/// Cursor reader. See the module comment for the failure model.
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
    bad: bool,
}

impl<'a> Reader<'a> {
    /// Verify the checksum and position the cursor after the header. `None`
    /// for a wrong magic, version, length or checksum.
    pub fn open(buf: &'a [u8]) -> Option<Self> {
        let body = buf.len().checked_sub(4)?;
        let stored = u32::from_le_bytes(buf[body..].try_into().ok()?);
        if stored != checksum(&buf[..body]) {
            return None;
        }
        let mut reader = Self {
            buf: &buf[..body],
            at: 0,
            bad: false,
        };
        if reader.u32() != MAGIC || reader.u16() != VERSION {
            return None;
        }
        Some(reader)
    }

    /// Reader over an unframed slice, for tests of one record.
    pub fn raw(buf: &'a [u8]) -> Self {
        Self {
            buf,
            at: 0,
            bad: false,
        }
    }

    /// Mark the save invalid: a value was read that no state can hold.
    pub fn reject(&mut self) {
        self.bad = true;
    }

    pub const fn is_bad(&self) -> bool {
        self.bad
    }

    /// True when every byte was consumed and no read failed.
    pub fn finish(&self) -> bool {
        !self.bad && self.at == self.buf.len()
    }

    #[inline(never)]
    pub fn u8(&mut self) -> u8 {
        match self.buf.get(self.at) {
            Some(&value) => {
                self.at += 1;
                value
            }
            None => {
                self.bad = true;
                0
            }
        }
    }

    pub fn bool(&mut self) -> bool {
        match self.u8() {
            0 => false,
            1 => true,
            _ => {
                self.bad = true;
                false
            }
        }
    }

    #[inline(never)]
    pub fn u16(&mut self) -> u16 {
        let low = u16::from(self.u8());
        low | (u16::from(self.u8()) << 8)
    }

    pub fn i16(&mut self) -> i16 {
        self.u16() as i16
    }

    #[inline(never)]
    pub fn u32(&mut self) -> u32 {
        let low = u32::from(self.u16());
        low | (u32::from(self.u16()) << 16)
    }

    pub fn i32(&mut self) -> i32 {
        self.u32() as i32
    }

    pub fn vec3(&mut self) -> Vec3I32 {
        Vec3I32 {
            x: self.i32(),
            y: self.i32(),
            z: self.i32(),
        }
    }

    pub fn vec3_16(&mut self) -> Vec3I16 {
        Vec3I16 {
            x: self.i16(),
            y: self.i16(),
            z: self.i16(),
        }
    }

    /// A value that must be below `limit` (an enum discriminant, an index).
    pub fn below(&mut self, limit: u8) -> u8 {
        let value = self.u8();
        if value >= limit {
            self.bad = true;
            return 0;
        }
        value
    }
}

/// FNV-1a over `bytes`, the project's one checksum idiom (the same hash the
/// Half-Life save uses). The memory card's own per-frame XOR catches media
/// damage; this catches a short or interleaved write and a layout change.
pub fn checksum(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &byte in bytes {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Write `values` as words with zero runs coded as a zero word and a run
/// length, so a mostly-zero table costs a few bytes.
pub fn write_words<T: Copy + PartialEq + Default>(
    w: &mut Writer,
    values: &[T],
    word: impl Fn(T) -> u16,
) {
    let mut at = 0;
    while at < values.len() {
        let value = values[at];
        if value == T::default() {
            let mut run = 1usize;
            while at + run < values.len() && values[at + run] == T::default() && run < 0xffff {
                run += 1;
            }
            w.u16(0);
            w.u16(run as u16);
            at += run;
        } else {
            w.u16(word(value));
            at += 1;
        }
    }
}

/// The inverse of [`write_words`] into a slice of known length. Anything that
/// does not fill the slice exactly rejects the save.
pub fn read_words<T: Copy + Default>(r: &mut Reader, out: &mut [T], value: impl Fn(u16) -> T) {
    let mut at = 0;
    while at < out.len() {
        let word = r.u16();
        if r.is_bad() {
            return;
        }
        if word == 0 {
            let run = usize::from(r.u16());
            if run == 0 || at + run > out.len() {
                r.reject();
                return;
            }
            out[at..at + run].fill(T::default());
            at += run;
        } else {
            out[at] = value(word);
            at += 1;
        }
    }
}

/// A type that can be written to and rebuilt from a save.
pub trait Snapshot: Sized {
    fn save(&self, w: &mut Writer);
    /// Rebuild from the reader. A value no valid state can hold must call
    /// [`Reader::reject`] and may return any placeholder.
    fn load(r: &mut Reader) -> Self;
}

/// Frame a payload written by `body`: magic, version, payload, checksum.
/// Returns the total length, or `None` if `buf` was too small.
pub fn frame(buf: &mut [u8], body: impl FnOnce(&mut Writer)) -> Option<usize> {
    let mut w = Writer::new(buf);
    w.u32(MAGIC);
    w.u16(VERSION);
    body(&mut w);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_round_trip_and_the_frame_checks_out() {
        let mut buf = [0u8; 64];
        let len = frame(&mut buf, |w| {
            w.u8(0xab);
            w.bool(true);
            w.i16(-1234);
            w.u32(0xdead_beef);
            w.vec3(Vec3I32 {
                x: -1,
                y: 2,
                z: -3 << 12,
            });
        })
        .expect("fits");
        let mut r = Reader::open(&buf[..len]).expect("valid frame");
        assert_eq!(r.u8(), 0xab);
        assert!(r.bool());
        assert_eq!(r.i16(), -1234);
        assert_eq!(r.u32(), 0xdead_beef);
        assert_eq!(
            r.vec3(),
            Vec3I32 {
                x: -1,
                y: 2,
                z: -3 << 12
            }
        );
        assert!(r.finish());
    }

    #[test]
    fn corruption_truncation_and_stale_headers_are_refused() {
        let mut buf = [0u8; 32];
        let len = frame(&mut buf, |w| w.u32(7)).unwrap();
        assert!(Reader::open(&buf[..len]).is_some());
        for index in 0..len {
            let mut damaged = buf;
            damaged[index] ^= 0x10;
            assert!(Reader::open(&damaged[..len]).is_none(), "byte {index}");
        }
        assert!(Reader::open(&buf[..len - 1]).is_none());
        assert!(Reader::open(&[]).is_none());
        // A valid frame under another version is not this save.
        let mut other = buf;
        other[4] = other[4].wrapping_add(1);
        let sum = checksum(&other[..len - 4]);
        other[len - 4..len].copy_from_slice(&sum.to_le_bytes());
        assert!(Reader::open(&other[..len]).is_none());
    }

    #[test]
    fn running_off_the_end_or_a_bad_flag_fails_the_save_without_panicking() {
        let mut r = Reader::raw(&[1, 2]);
        assert_eq!(r.u32(), 0x0201);
        assert!(r.is_bad());
        assert!(!r.finish());
        let mut r = Reader::raw(&[2]);
        assert!(!r.bool());
        assert!(r.is_bad());
        let mut r = Reader::raw(&[9]);
        assert_eq!(r.below(8), 0);
        assert!(r.is_bad());
    }

    #[test]
    fn a_buffer_that_is_too_small_is_reported() {
        let mut buf = [0u8; 8];
        assert!(frame(&mut buf, |w| w.u32(1)).is_none());
        let mut w = Writer::new(&mut buf[..2]);
        w.u32(1);
        assert!(w.overflowed());
    }
}
