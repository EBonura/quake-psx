//! Presentation-only pose smoothing for alias models.
//!
//! Quake thinks a monster ten times a second: its origin, yaw and animation
//! frame all change on that beat and nothing in between. The sim stays exactly
//! as it is. This module watches what the sim left in a model's pose, notices
//! when it changed, and tells the renderer how far along the glide from the
//! previous pose to the new one it should draw the model. The drawn pose
//! therefore trails the simulated one by at most one think, which is the price
//! every interpolating Quake engine pays, and the hitboxes, sight lines and
//! sounds still follow the real pose.
//!
//! Everything is 32-bit integer: weights are Q8 (`256` is the new pose), the
//! clock is the caller's 60 Hz tick count, and an animation frame is blended
//! by lerping the byte vertices of the two frames, which keeps the result on
//! the same grid as the authored frames so the existing alias submitter takes
//! it unchanged.

/// Quake's think period in 60 Hz ticks. A glide never takes longer than this.
pub const THINK_TICKS: u32 = 6;
/// A model unseen for this many ticks starts fresh instead of gliding from a
/// pose it left a long time ago.
pub const STALE_TICKS: u32 = 2 * THINK_TICKS;
/// Moves larger than this in any axis (whole units, Q12 below) are
/// teleports, knockback launches or level resets: draw them where they are.
pub const SNAP_UNITS: i32 = 128;
const SNAP_Q12: i32 = SNAP_UNITS << 12;

/// `256 / duration` for glide durations of one to six ticks.
const RECIPROCAL_Q8: [u32; THINK_TICKS as usize + 1] = [0, 256, 128, 85, 64, 51, 43];

/// What the sim currently says about one model.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pose {
    /// Q20.12 world origin.
    pub origin: [i32; 3],
    /// Quake angles, `65536` to the turn.
    pub angles: [i16; 3],
    /// Animation frame of `model`.
    pub frame: u16,
    /// Alias model id. A different model never glides: its frames mean
    /// something else.
    pub model: i16,
}

/// How to draw a model this frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Blend {
    /// The pose being glided from. The pose being glided to is the one the
    /// caller passed in.
    pub from: Pose,
    /// Progress from `from` (0) to the current pose (256).
    pub weight_q8: u32,
}

impl Blend {
    /// Nothing to blend: draw the current pose as the sim has it.
    pub const fn settled(&self) -> bool {
        self.weight_q8 >= 256
    }
}

#[derive(Copy, Clone)]
struct Track {
    key: u16,
    /// Ticks, modulo 2^16: only differences under `STALE_TICKS` are read.
    seen: u16,
    changed: u16,
    duration: u8,
    from: Pose,
    to: Pose,
}

const EMPTY_KEY: u16 = u16::MAX;

impl Track {
    const EMPTY: Self = Self {
        key: EMPTY_KEY,
        seen: 0,
        changed: 0,
        duration: THINK_TICKS as u8,
        from: Pose {
            origin: [0; 3],
            angles: [0; 3],
            frame: 0,
            model: 0,
        },
        to: Pose {
            origin: [0; 3],
            angles: [0; 3],
            frame: 0,
            model: 0,
        },
    };

    fn weight(&self, now: u16) -> u32 {
        let elapsed = u32::from(now.wrapping_sub(self.changed));
        (elapsed * RECIPROCAL_Q8[self.duration as usize]).min(256)
    }

    fn restart(&mut self, now: u16, to: Pose) {
        self.from = to;
        self.to = to;
        self.changed = now;
        self.duration = THINK_TICKS as u8;
    }
}

/// A small fixed table of the models currently being glided, keyed by the
/// caller's entity index.
///
/// Only the models the renderer draws are observed, so the table never needs
/// more entries than there can be visible monsters; when it is full the entry
/// unseen for longest is reused, which at worst makes that model start a fresh
/// glide.
pub struct PoseTracker<const SLOTS: usize> {
    tracks: [Track; SLOTS],
}

impl<const SLOTS: usize> PoseTracker<SLOTS> {
    pub const fn new() -> Self {
        Self {
            tracks: [Track::EMPTY; SLOTS],
        }
    }

    /// Forget every model, e.g. on a level load.
    pub fn clear(&mut self) {
        self.tracks = [Track::EMPTY; SLOTS];
    }

    /// Record the sim's current pose for `key` at tick `now` and return how
    /// to draw it.
    pub fn observe(&mut self, key: u16, now: u32, current: Pose) -> Blend {
        debug_assert!(key != EMPTY_KEY);
        let now = now as u16;
        let mut slot = SLOTS;
        let mut oldest = 0usize;
        let mut oldest_age = 0u32;
        let mut index = 0usize;
        while index < SLOTS {
            let track = &self.tracks[index];
            if track.key == key {
                slot = index;
                break;
            }
            let age = if track.key == EMPTY_KEY {
                u32::MAX
            } else {
                u32::from(now.wrapping_sub(track.seen))
            };
            if age >= oldest_age {
                oldest_age = age;
                oldest = index;
            }
            index += 1;
        }
        if slot == SLOTS {
            slot = oldest;
            let track = &mut self.tracks[slot];
            track.key = key;
            track.restart(now, current);
            track.seen = now;
            return Blend {
                from: current,
                weight_q8: 256,
            };
        }

        let track = &mut self.tracks[slot];
        let unseen = u32::from(now.wrapping_sub(track.seen));
        track.seen = now;
        if unseen > STALE_TICKS {
            track.restart(now, current);
            return Blend {
                from: current,
                weight_q8: 256,
            };
        }
        if track.to != current {
            if snaps(&track.to, &current) {
                track.restart(now, current);
                return Blend {
                    from: current,
                    weight_q8: 256,
                };
            }
            let weight = track.weight(now);
            let (origin, angles) = glide(&track.from, &track.to, weight);
            let frame = if weight >= 128 {
                track.to.frame
            } else {
                track.from.frame
            };
            let gap = u32::from(now.wrapping_sub(track.changed)).clamp(2, THINK_TICKS) as u8;
            track.from = Pose {
                origin,
                angles,
                frame,
                model: current.model,
            };
            track.to = current;
            track.changed = now;
            track.duration = gap;
        }
        Blend {
            from: track.from,
            weight_q8: track.weight(now),
        }
    }
}

fn snaps(old: &Pose, new: &Pose) -> bool {
    if old.model != new.model {
        return true;
    }
    let mut axis = 0;
    while axis < 3 {
        if (new.origin[axis].wrapping_sub(old.origin[axis])).abs() > SNAP_Q12 {
            return true;
        }
        axis += 1;
    }
    false
}

/// Origin and angles at `weight_q8` of the way from `from` to `to`.
///
/// Angles take the short way round the circle.
pub fn glide(from: &Pose, to: &Pose, weight_q8: u32) -> ([i32; 3], [i16; 3]) {
    let weight = weight_q8.min(256) as i32;
    let mut origin = [0i32; 3];
    let mut angles = [0i16; 3];
    let mut axis = 0;
    while axis < 3 {
        let delta = to.origin[axis].wrapping_sub(from.origin[axis]);
        origin[axis] = from.origin[axis].wrapping_add((delta * weight + 128) >> 8);
        let turn = to.angles[axis].wrapping_sub(from.angles[axis]) as i32;
        angles[axis] = from.angles[axis].wrapping_add(((turn * weight + 128) >> 8) as i16);
        axis += 1;
    }
    (origin, angles)
}

/// Blend two animation frames of one model into `out`, byte by byte:
/// `from + (to - from) * weight`, rounded to nearest, with the weight
/// quantized to sixty-fourths.
///
/// The frames are the cooked alias vertex bytes (three per vertex). The three
/// slices should have the same length; the shortest decides how many bytes are
/// written.
///
/// Four bytes are blended per step as two pairs of 16-bit lanes, one multiply
/// per pair: with `w` in `0..=64`, a lane of `(b - a + 256) * w` stays below
/// 2^16, and adding 32 and subtracting `256 * w` per lane leaves
/// `a * 64 + (b - a) * w + 32`, which is never negative, so no lane borrows
/// from its neighbour. That halves the multiplies (each stalls the pipeline
/// for six cycles) against blending byte by byte.
pub fn blend_frames(from: &[u8], to: &[u8], weight_q8: u32, out: &mut [u8]) {
    let weight = weight_q8.min(256) >> 2;
    let length = out.len().min(from.len()).min(to.len());
    let words = length / 4;
    let bias = 0x0100_0100u32;
    let lanes = 0x00ff_00ffu32;
    let offset = (weight * 256) * 0x0001_0001;
    let half = 0x0020_0020u32;
    let mut index = 0;
    while index < words {
        let at = index * 4;
        // SAFETY: `at + 4 <= length`, so every read and the write are inside
        // the three slices; the accesses are unaligned-safe.
        let (a, b) = unsafe {
            (
                core::ptr::read_unaligned(from.as_ptr().add(at).cast::<u32>()),
                core::ptr::read_unaligned(to.as_ptr().add(at).cast::<u32>()),
            )
        };
        let (a, b) = (u32::from_le(a), u32::from_le(b));
        let (a_even, a_odd) = (a & lanes, (a >> 8) & lanes);
        let (b_even, b_odd) = (b & lanes, (b >> 8) & lanes);
        let m_even = (b_even + bias - a_even) * weight;
        let m_odd = (b_odd + bias - a_odd) * weight;
        let even = (((a_even << 6) + m_even + half - offset) >> 6) & lanes;
        let odd = (((a_odd << 6) + m_odd + half - offset) >> 6) & lanes;
        let blended = (even | (odd << 8)).to_le();
        // SAFETY: as above.
        unsafe { core::ptr::write_unaligned(out.as_mut_ptr().add(at).cast::<u32>(), blended) };
        index += 1;
    }
    let mut at = words * 4;
    while at < length {
        let from = u32::from(from[at]);
        let to = u32::from(to[at]);
        out[at] = ((from * 64 + to * weight + 32 - from * weight) >> 6) as u8;
        at += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: i32, yaw: i16, frame: u16) -> Pose {
        Pose {
            origin: [x << 12, 0, 0],
            angles: [0, yaw, 0],
            frame,
            model: 7,
        }
    }

    #[test]
    fn first_sight_draws_the_sim_pose() {
        let mut tracker = PoseTracker::<4>::new();
        let blend = tracker.observe(3, 100, pose(10, 0, 5));
        assert!(blend.settled());
        assert_eq!(blend.from, pose(10, 0, 5));
    }

    #[test]
    fn a_think_starts_a_glide_from_the_previous_pose() {
        let mut tracker = PoseTracker::<4>::new();
        tracker.observe(3, 100, pose(10, 0, 5));
        // Steady for a think, then the sim steps.
        tracker.observe(3, 104, pose(10, 0, 5));
        let blend = tracker.observe(3, 106, pose(20, 0, 6));
        assert_eq!(blend.weight_q8, 0);
        assert_eq!(blend.from, pose(10, 0, 5));
        // Half a think later the glide is half way.
        let blend = tracker.observe(3, 109, pose(20, 0, 6));
        assert_eq!(blend.weight_q8, 3 * 43);
        let (origin, _) = glide(&blend.from, &pose(20, 0, 6), blend.weight_q8);
        assert!(origin[0] > 14 << 12 && origin[0] < 16 << 12);
        // And it lands on the new pose after a full think and holds there.
        assert!(tracker.observe(3, 112, pose(20, 0, 6)).settled());
        assert!(tracker.observe(3, 130, pose(20, 0, 6)).settled());
    }

    #[test]
    fn a_step_every_frame_glides_one_frame_behind_without_drifting() {
        let mut tracker = PoseTracker::<4>::new();
        let mut now = 0;
        let mut x = 0;
        tracker.observe(1, now, pose(x, 0, 0));
        for _ in 0..40 {
            now += 2;
            x += 4;
            let blend = tracker.observe(1, now, pose(x, 0, 0));
            // A model that moves on every observation starts each glide where
            // the last one was, so it trails by no more than one step.
            let (origin, _) = glide(&blend.from, &pose(x, 0, 0), blend.weight_q8);
            assert!(origin[0] >= (x - 4) << 12 && origin[0] <= x << 12);
        }
    }

    #[test]
    fn an_interrupted_glide_continues_from_where_it_was_drawn() {
        let mut tracker = PoseTracker::<4>::new();
        tracker.observe(1, 0, pose(0, 0, 0));
        tracker.observe(1, 6, pose(12, 0, 1));
        // Three ticks into the glide the sim steps again.
        let before = tracker.observe(1, 9, pose(12, 0, 1));
        let (shown, _) = glide(&before.from, &pose(12, 0, 1), before.weight_q8);
        let after = tracker.observe(1, 9, pose(24, 0, 2));
        assert_eq!(after.from.origin[0], shown[0]);
        assert_eq!(after.weight_q8, 0);
    }

    #[test]
    fn teleports_models_and_long_absences_snap() {
        let mut tracker = PoseTracker::<4>::new();
        tracker.observe(1, 0, pose(0, 0, 0));
        assert!(tracker.observe(1, 6, pose(500, 0, 0)).settled());
        let mut other = pose(500, 0, 3);
        other.model = 9;
        assert!(tracker.observe(1, 12, other).settled());
        assert!(tracker.observe(1, 200, pose(510, 0, 4)).settled());
    }

    #[test]
    fn yaw_takes_the_short_way_round() {
        let from = pose(0, -300, 0);
        let to = pose(0, 300, 0);
        let (_, angles) = glide(&from, &to, 128);
        assert_eq!(angles[1], 0);
        // Across the wrap: 65000 -> 600 is a 1136 turn, not a 64400 one.
        let from = pose(0, 65000u32 as u16 as i16, 0);
        let to = pose(0, 600, 0);
        let (_, angles) = glide(&from, &to, 128);
        let expected = (65000u32 as u16 as i16).wrapping_add(568);
        assert_eq!(angles[1], expected);
    }

    #[test]
    fn frame_blend_matches_its_endpoints_and_rounds() {
        let from = [0u8, 255, 10, 100];
        let to = [255u8, 0, 11, 100];
        let mut out = [0u8; 4];
        blend_frames(&from, &to, 0, &mut out);
        assert_eq!(out, from);
        blend_frames(&from, &to, 256, &mut out);
        assert_eq!(out, to);
        blend_frames(&from, &to, 128, &mut out);
        assert_eq!(out, [128, 128, 11, 100]);
    }

    #[test]
    fn swar_blend_matches_the_scalar_reference_for_every_pair_and_weight() {
        // Every (from, to) byte pair at every weight, laid out so each lane
        // position and the unaligned tail are exercised.
        for weight in (0..=256u32).step_by(7).chain([255, 256]) {
            let w = weight.min(256) >> 2;
            for from in 0..=255u32 {
                let from_bytes: [u8; 11] = core::array::from_fn(|i| (from as u8).wrapping_add(i as u8));
                for to in (0..=255u32).step_by(5).chain([255]) {
                    let to_bytes: [u8; 11] = core::array::from_fn(|i| (to as u8).wrapping_sub(i as u8 * 3));
                    let mut out = [0u8; 11];
                    blend_frames(&from_bytes, &to_bytes, weight, &mut out);
                    for i in 0..11 {
                        let a = u32::from(from_bytes[i]);
                        let b = u32::from(to_bytes[i]);
                        let expected = ((a * (64 - w) + b * w + 32) >> 6) as u8;
                        assert_eq!(out[i], expected, "w={weight} a={a} b={b} i={i}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_full_table_reuses_the_longest_unseen_entry() {
        let mut tracker = PoseTracker::<2>::new();
        tracker.observe(1, 0, pose(0, 0, 0));
        tracker.observe(2, 2, pose(0, 0, 0));
        // Key 3 evicts key 1 (unseen longest); key 2 keeps its glide state.
        tracker.observe(3, 4, pose(0, 0, 0));
        tracker.observe(2, 6, pose(0, 0, 0));
        let blend = tracker.observe(2, 8, pose(8, 0, 1));
        assert_eq!(blend.weight_q8, 0);
        assert_eq!(blend.from, pose(0, 0, 0));
    }
}
