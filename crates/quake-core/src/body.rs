//! Dynamic body blocking for Quake `SOLID_SLIDEBOX` entities.
//!
//! A moving body is traced against the other body's expanded box. Only live
//! bodies participate, ties keep authored source order, and an overlap at the
//! start is skipped so the movement code can separate the pair. Candidate
//! storage is fixed at [`MAX_BODY_CANDIDATES`].

use core::mem::MaybeUninit;

use quake_formats::{Vec3I16, Vec3I32};

use crate::collision::{Q12_ONE, TRACE_PLANE_EPSILON_Q12};
use crate::combat::segment_aabb_fraction;

/// Bodies considered by one trace. Episode 1's densest authored monster
/// clusters are far below this; the host capacity assertion proves it.
pub const MAX_BODY_CANDIDATES: usize = 16;

/// Source index reserved for the player body, which has no authored entity.
pub const PLAYER_BODY_SOURCE: u16 = u16::MAX;

const AXIS_NORMAL_Q12: i16 = Q12_ONE as i16;

/// Quake's three canonical clip hull extents, in whole units.
pub const fn hull_extents(hull_index: usize) -> (Vec3I16, Vec3I16) {
    let (mins, maxs) = match hull_index {
        1 => ([-16, -16, -24], [16, 16, 32]),
        2 => ([-32, -32, -24], [32, 32, 64]),
        _ => ([0, 0, 0], [0, 0, 0]),
    };
    (
        Vec3I16 {
            x: mins[0],
            y: mins[1],
            z: mins[2],
        },
        Vec3I16 {
            x: maxs[0],
            y: maxs[1],
            z: maxs[2],
        },
    )
}

/// One candidate blocker in absolute Q20.12 world bounds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Body {
    pub source_index: u16,
    pub mins: Vec3I32,
    pub maxs: Vec3I32,
    /// A dead monster is a corpse and never blocks.
    pub dead: bool,
}

/// The winning body block for one trace.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BodyImpact {
    pub source_index: u16,
    pub fraction: i32,
    pub end: Vec3I32,
    pub normal: Vec3I16,
}

/// Fixed-capacity candidate set for one trace.
///
/// Only the first `len` slots are ever initialized or read: `push` writes a
/// slot before advancing `len`, and `resolve` walks `..len`. Leaving the rest
/// uninitialized avoids zeroing 512 bytes on every player trace, which the
/// guest profile showed as most of its `memset` time.
#[derive(Copy, Clone)]
pub struct BodyBlockers {
    bodies: [MaybeUninit<Body>; MAX_BODY_CANDIDATES],
    len: u8,
    refused: u8,
}

impl BodyBlockers {
    pub const fn new() -> Self {
        Self {
            bodies: [const { MaybeUninit::uninit() }; MAX_BODY_CANDIDATES],
            len: 0,
            refused: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Candidates refused because the set was already full, saturating.
    pub const fn refused(&self) -> u8 {
        self.refused
    }

    /// Offer one candidate. Corpses are dropped silently; a live candidate
    /// offered to a full set is refused and counted.
    pub fn push(&mut self, body: Body) -> bool {
        if body.dead {
            return false;
        }
        if self.len as usize == MAX_BODY_CANDIDATES {
            self.refused = self.refused.saturating_add(1);
            return false;
        }
        self.bodies[self.len as usize].write(body);
        self.len += 1;
        true
    }

    /// Resolve the earliest body block for a mover of `hull_index` sweeping
    /// its origin from `start` to `end`.
    pub fn resolve(&self, start: Vec3I32, end: Vec3I32, hull_index: usize) -> Option<BodyImpact> {
        let (mover_mins, mover_maxs) = hull_extents(hull_index);
        let mut best: Option<BodyImpact> = None;
        let mut index = 0usize;
        while index < self.len as usize {
            // SAFETY: `push` initializes slot `len` before incrementing `len`,
            // and `len` only ever grows, so every slot below `len` is written.
            let body = unsafe { self.bodies[index].assume_init() };
            index += 1;
            let Some(impact) = body_impact(start, end, body, mover_mins, mover_maxs) else {
                continue;
            };
            let replace = match best {
                Some(current) => impact.fraction < current.fraction,
                None => true,
            };
            if replace {
                best = Some(impact);
            }
        }
        best
    }
}

impl Default for BodyBlockers {
    fn default() -> Self {
        Self::new()
    }
}

/// Broad-phase margin around a candidate's clip box, in whole units.
pub const BROAD_PHASE_MARGIN_UNITS: i32 = 32;

/// Broad-phase margin for a hull sweep. The largest Quake hull reaches 64 units
/// above its origin, so this covers every hull the game traces with.
pub const HULL_BROAD_PHASE_MARGIN_UNITS: i32 = 64;

/// One trace's swept extent reduced to whole units, so the per-candidate broad
/// phase compares against unshifted `i16` clip bounds instead of rebuilding
/// each candidate's Q12 box.
///
/// `overlaps` returns exactly what the Q12 test
/// `swept_max < (clip_min << 12) - margin || swept_min > (clip_max << 12) + margin`
/// returns, because for integer `k`: `x < k << 12` iff `x >> 12 < k`, and
/// `x > k << 12` iff `ceil(x / 4096) > k` (`swept_unit_box_matches_q12_test`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SweptUnitBox {
    /// `ceil(min / 4096)` per axis.
    min_units: [i32; 3],
    /// `floor(max / 4096)` per axis.
    max_units: [i32; 3],
}

impl SweptUnitBox {
    #[inline]
    pub fn new(start: Vec3I32, end: Vec3I32) -> Self {
        let start = [start.x, start.y, start.z];
        let end = [end.x, end.y, end.z];
        let mut min_units = [0i32; 3];
        let mut max_units = [0i32; 3];
        let mut axis = 0usize;
        while axis < 3 {
            // Saturating only matters within 4095 of `i32::MAX`, where both
            // roundings exceed every `i16` bound and compare identically.
            min_units[axis] = start[axis].min(end[axis]).saturating_add(4095) >> 12;
            max_units[axis] = start[axis].max(end[axis]) >> 12;
            axis += 1;
        }
        Self {
            min_units,
            max_units,
        }
    }

    #[inline]
    pub fn overlaps(&self, clip_mins: [i16; 3], clip_maxs: [i16; 3]) -> bool {
        self.overlaps_within(clip_mins, clip_maxs, BROAD_PHASE_MARGIN_UNITS)
    }

    /// As [`Self::overlaps`] with an explicit margin. A hull trace sweeps a box,
    /// not a point, so its broad phase has to grow by the hull's own extent;
    /// [`HULL_BROAD_PHASE_MARGIN_UNITS`] covers every Quake hull.
    #[inline]
    pub fn overlaps_within(
        &self,
        clip_mins: [i16; 3],
        clip_maxs: [i16; 3],
        margin_units: i32,
    ) -> bool {
        let mut axis = 0usize;
        while axis < 3 {
            if self.max_units[axis] < i32::from(clip_mins[axis]) - margin_units
                || self.min_units[axis] > i32::from(clip_maxs[axis]) + margin_units
            {
                return false;
            }
            axis += 1;
        }
        true
    }
}

/// A whole-unit region around one frame's player origin.
///
/// Every player trace of the frame is expected to sweep inside it. A candidate
/// whose margin-expanded clip box misses the region on some axis cannot
/// overlap any swept box contained in the region on that axis, so the
/// per-trace broad phase can skip it without changing its answer
/// (`region_prefilter_never_drops_an_overlap` pins the argument). A trace
/// that leaves the region falls back to the complete candidate list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BroadPhaseRegion {
    mins: [i32; 3],
    maxs: [i32; 3],
}

impl BroadPhaseRegion {
    pub fn around(anchor: Vec3I32, half_extent_units: i32) -> Self {
        let anchor = [anchor.x >> 12, anchor.y >> 12, anchor.z >> 12];
        let mut mins = [0i32; 3];
        let mut maxs = [0i32; 3];
        let mut axis = 0usize;
        while axis < 3 {
            mins[axis] = anchor[axis].saturating_sub(half_extent_units);
            maxs[axis] = anchor[axis].saturating_add(half_extent_units);
            axis += 1;
        }
        Self { mins, maxs }
    }

    /// Whether a candidate's expanded box reaches the region at all.
    #[inline]
    pub fn may_overlap(&self, clip_mins: [i16; 3], clip_maxs: [i16; 3]) -> bool {
        let mut axis = 0usize;
        while axis < 3 {
            if self.maxs[axis] < i32::from(clip_mins[axis]) - BROAD_PHASE_MARGIN_UNITS
                || self.mins[axis] > i32::from(clip_maxs[axis]) + BROAD_PHASE_MARGIN_UNITS
            {
                return false;
            }
            axis += 1;
        }
        true
    }

    /// Whether a swept box lies inside the region, in the same whole-unit
    /// rounding `SweptUnitBox::overlaps` compares with.
    #[inline]
    pub fn contains(&self, swept: &SweptUnitBox) -> bool {
        let mut axis = 0usize;
        while axis < 3 {
            if swept.min_units[axis] < self.mins[axis] || swept.max_units[axis] > self.maxs[axis] {
                return false;
            }
            axis += 1;
        }
        true
    }
}

/// Minkowski point trace of one mover hull against one body box.
pub fn body_impact(
    start: Vec3I32,
    end: Vec3I32,
    body: Body,
    mover_mins: Vec3I16,
    mover_maxs: Vec3I16,
) -> Option<BodyImpact> {
    if body.dead {
        return None;
    }
    let solid_mins = Vec3I32 {
        x: body.mins.x.saturating_sub(i32::from(mover_maxs.x) << 12),
        y: body.mins.y.saturating_sub(i32::from(mover_maxs.y) << 12),
        z: body.mins.z.saturating_sub(i32::from(mover_maxs.z) << 12),
    };
    let solid_maxs = Vec3I32 {
        x: body.maxs.x.saturating_sub(i32::from(mover_mins.x) << 12),
        y: body.maxs.y.saturating_sub(i32::from(mover_mins.y) << 12),
        z: body.maxs.z.saturating_sub(i32::from(mover_mins.z) << 12),
    };
    // Degenerate (gibbed) bodies have no volume to clip against.
    if solid_mins.x > solid_maxs.x || solid_mins.y > solid_maxs.y || solid_mins.z > solid_maxs.z {
        return None;
    }
    // An overlapped start is left to the next frame's separating motion.
    if segment_aabb_fraction(start, start, solid_mins, solid_maxs).is_some() {
        return None;
    }
    // Stop one plane epsilon short, exactly like the shared BSP tracer, so the
    // resting origin stays outside the body volume.
    let stop_mins = Vec3I32 {
        x: solid_mins.x.saturating_sub(TRACE_PLANE_EPSILON_Q12),
        y: solid_mins.y.saturating_sub(TRACE_PLANE_EPSILON_Q12),
        z: solid_mins.z.saturating_sub(TRACE_PLANE_EPSILON_Q12),
    };
    let stop_maxs = Vec3I32 {
        x: solid_maxs.x.saturating_add(TRACE_PLANE_EPSILON_Q12),
        y: solid_maxs.y.saturating_add(TRACE_PLANE_EPSILON_Q12),
        z: solid_maxs.z.saturating_add(TRACE_PLANE_EPSILON_Q12),
    };
    let fraction = segment_aabb_fraction(start, end, stop_mins, stop_maxs)?;
    if fraction >= Q12_ONE {
        return None;
    }
    let impact = interpolate(start, end, fraction);
    let axis = entry_axis(start, end, impact, stop_mins, stop_maxs)?;
    let mut normal = Vec3I16 { x: 0, y: 0, z: 0 };
    let component = |vector: Vec3I32| match axis {
        0 => vector.x,
        1 => vector.y,
        _ => vector.z,
    };
    let approach = component(end).saturating_sub(component(start));
    let sign = if approach > 0 {
        -AXIS_NORMAL_Q12
    } else {
        AXIS_NORMAL_Q12
    };
    match axis {
        0 => normal.x = sign,
        1 => normal.y = sign,
        _ => normal.z = sign,
    }
    Some(BodyImpact {
        source_index: body.source_index,
        fraction,
        end: impact,
        normal,
    })
}

/// Pick the slab face the segment entered through. The true entry face lies on
/// the impact point up to Q12 rounding, so the nearest facing plane is exact;
/// axis order breaks an exact tie. A segment that merely touches a face while
/// leaving fails the approach test and reports no entry, which keeps a mover
/// resting against a body free to walk away from it.
fn entry_axis(
    start: Vec3I32,
    end: Vec3I32,
    impact: Vec3I32,
    mins: Vec3I32,
    maxs: Vec3I32,
) -> Option<usize> {
    let starts = [start.x, start.y, start.z];
    let ends = [end.x, end.y, end.z];
    let impacts = [impact.x, impact.y, impact.z];
    let low = [mins.x, mins.y, mins.z];
    let high = [maxs.x, maxs.y, maxs.z];
    let mut best: Option<(usize, i32)> = None;
    let mut axis = 0usize;
    while axis < 3 {
        let delta = ends[axis].saturating_sub(starts[axis]);
        if delta != 0 {
            let (face, approaching) = if delta > 0 {
                (low[axis], starts[axis] <= low[axis])
            } else {
                (high[axis], starts[axis] >= high[axis])
            };
            if approaching {
                let gap = impacts[axis].saturating_sub(face).saturating_abs();
                if best.is_none_or(|(_, current)| gap < current) {
                    best = Some((axis, gap));
                }
            }
        }
        axis += 1;
    }
    best.map(|(axis, _)| axis)
}

fn interpolate(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    let component = |from: i32, to: i32| {
        from.saturating_add(psx_math::int32::mul_q12_i32(
            to.saturating_sub(from),
            fraction,
        ))
    };
    Vec3I32 {
        x: component(start.x, end.x),
        y: component(start.y, end.y),
        z: component(start.z, end.z),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(x: i32, y: i32, z: i32) -> Vec3I32 {
        Vec3I32 {
            x: x << 12,
            y: y << 12,
            z: z << 12,
        }
    }

    /// The Q12 broad phase the guest used before `SweptUnitBox`.
    fn q12_overlaps(start: Vec3I32, end: Vec3I32, mins: [i16; 3], maxs: [i16; 3]) -> bool {
        let margin = BROAD_PHASE_MARGIN_UNITS << 12;
        let start = [start.x, start.y, start.z];
        let end = [end.x, end.y, end.z];
        for axis in 0..3 {
            let swept_min = start[axis].min(end[axis]);
            let swept_max = start[axis].max(end[axis]);
            let entity_min = (i32::from(mins[axis]) << 12).saturating_sub(margin);
            let entity_max = (i32::from(maxs[axis]) << 12).saturating_add(margin);
            if swept_max < entity_min || swept_min > entity_max {
                return false;
            }
        }
        true
    }

    #[test]
    fn swept_unit_box_matches_q12_test() {
        // Every boundary the two forms could disagree on: exact multiples of
        // 4096 on either side of the margin edge, one Q12 step around them,
        // negatives, and the i32 extremes where the ceil saturates.
        let edges: [i32; 13] = [
            i32::MIN,
            i32::MIN + 4095,
            -(64 << 12) - 1,
            -(64 << 12),
            -(64 << 12) + 1,
            -1,
            0,
            1,
            (48 << 12) - 1,
            48 << 12,
            (48 << 12) + 1,
            i32::MAX - 4095,
            i32::MAX,
        ];
        let boxes: [([i16; 3], [i16; 3]); 4] = [
            ([-16, -16, -24], [16, 16, 32]),
            ([-32, -32, -32], [-32, -32, -32]),
            ([i16::MIN, 0, 0], [i16::MAX, 0, 0]),
            ([16, -100, 200], [16, 100, 300]),
        ];
        let mut checked = 0usize;
        for &a in &edges {
            for &b in &edges {
                let start = Vec3I32 { x: a, y: b, z: a };
                let end = Vec3I32 { x: b, y: a, z: b };
                let swept = SweptUnitBox::new(start, end);
                for &(mins, maxs) in &boxes {
                    assert_eq!(
                        swept.overlaps(mins, maxs),
                        q12_overlaps(start, end, mins, maxs),
                        "start={start:?} end={end:?} mins={mins:?} maxs={maxs:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 13 * 13 * 4);
        // A body straddling the margin edge on one axis only.
        let swept = SweptUnitBox::new(units(0, 0, 0), units(10, 0, 0));
        assert!(swept.overlaps([42, -1, -1], [50, 1, 1]));
        assert!(!swept.overlaps([43, -1, -1], [50, 1, 1]));
        assert!(swept.overlaps([-40, -1, -1], [-32, 1, 1]));
        assert!(!swept.overlaps([-40, -1, -1], [-33, 1, 1]));
    }

    #[test]
    fn region_prefilter_never_drops_an_overlap() {
        // Region [-64, 64] units around the origin. Sweep endpoints and boxes
        // over every whole-unit and one-Q12-step offset around the region and
        // margin edges: whenever a contained swept box overlaps a candidate,
        // the region must have kept that candidate.
        let region = BroadPhaseRegion::around(units(0, 0, 0), 64);
        const UNITS: [i32; 23] = [
            -100, -97, -96, -95, -65, -64, -63, -33, -32, -31, -1, 0, 1, 31, 32, 33, 63, 64, 65,
            95, 96, 97, 100,
        ];
        let mut coords = [0i32; UNITS.len() * 3];
        for (slot, u) in UNITS.iter().enumerate() {
            coords[slot * 3] = u << 12;
            coords[slot * 3 + 1] = (u << 12) - 1;
            coords[slot * 3 + 2] = (u << 12) + 1;
        }
        let mut contained = 0usize;
        let mut kept_overlaps = 0usize;
        for &a in &coords {
            for &b in &coords {
                let start = Vec3I32 { x: a, y: b, z: 0 };
                let end = Vec3I32 { x: b, y: a, z: 0 };
                let swept = SweptUnitBox::new(start, end);
                let inside = region.contains(&swept);
                contained += usize::from(inside);
                for &lo in &[
                    -130i16, -97, -96, -95, -64, -33, -32, -31, 0, 31, 32, 33, 64, 95, 96, 97, 130,
                ] {
                    for &size in &[0i16, 1, 16, 33, 64] {
                        let mins = [lo, -8, -8];
                        let maxs = [lo.saturating_add(size), 8, 8];
                        if inside && swept.overlaps(mins, maxs) {
                            kept_overlaps += 1;
                            assert!(
                                region.may_overlap(mins, maxs),
                                "start={start:?} end={end:?} mins={mins:?} maxs={maxs:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(contained > 0 && kept_overlaps > 0);
        // A candidate whose expanded box just misses the region is dropped;
        // one that just reaches it is kept.
        assert!(!region.may_overlap([97, 0, 0], [120, 0, 0]));
        assert!(region.may_overlap([96, 0, 0], [120, 0, 0]));
        assert!(!region.may_overlap([-120, 0, 0], [-97, 0, 0]));
        assert!(region.may_overlap([-120, 0, 0], [-96, 0, 0]));
        // Containment uses the same floor/ceil units as `overlaps`: a maximum
        // below 65 units floors inside, exactly 65 units does not; a minimum
        // above -65 units ceils inside, exactly -65 units does not.
        assert!(region.contains(&SweptUnitBox::new(units(-64, 0, 0), units(64, 0, 0))));
        assert!(region.contains(&SweptUnitBox::new(
            Vec3I32 {
                x: (-65 << 12) + 1,
                y: 0,
                z: 0
            },
            Vec3I32 {
                x: (65 << 12) - 1,
                y: 0,
                z: 0
            },
        )));
        assert!(!region.contains(&SweptUnitBox::new(units(-64, 0, 0), units(65, 0, 0))));
        assert!(!region.contains(&SweptUnitBox::new(units(-65, 0, 0), units(64, 0, 0))));
    }

    fn monster_body(source_index: u16, origin: Vec3I32) -> Body {
        // The standard 32x32x64 Quake monster box used by the soldier, knight,
        // wizard, and zombie.
        Body {
            source_index,
            mins: Vec3I32 {
                x: origin.x - (16 << 12),
                y: origin.y - (16 << 12),
                z: origin.z - (24 << 12),
            },
            maxs: Vec3I32 {
                x: origin.x + (16 << 12),
                y: origin.y + (16 << 12),
                z: origin.z + (40 << 12),
            },
            dead: false,
        }
    }

    #[test]
    fn hull_extents_match_quake_clip_hulls() {
        assert_eq!(
            hull_extents(0),
            (Vec3I16 { x: 0, y: 0, z: 0 }, Vec3I16 { x: 0, y: 0, z: 0 })
        );
        assert_eq!(
            hull_extents(1),
            (
                Vec3I16 {
                    x: -16,
                    y: -16,
                    z: -24
                },
                Vec3I16 {
                    x: 16,
                    y: 16,
                    z: 32
                }
            )
        );
        assert_eq!(
            hull_extents(2),
            (
                Vec3I16 {
                    x: -32,
                    y: -32,
                    z: -24
                },
                Vec3I16 {
                    x: 32,
                    y: 32,
                    z: 64
                }
            )
        );
    }

    #[test]
    fn player_is_blocked_by_a_live_monster_body() {
        let mut blockers = BodyBlockers::new();
        assert!(blockers.push(monster_body(21, units(96, 0, 0))));
        let impact = blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .expect("player hull meets the monster box");
        assert_eq!(impact.source_index, 21);
        assert!(impact.fraction > 0 && impact.fraction < Q12_ONE);
        // 96 - 16 (body) - 16 (player hull) = 64 units of free travel.
        let stop = impact.end.x >> 12;
        assert!((63..=64).contains(&stop), "stopped at {stop}");
        assert_eq!(impact.normal.x, -(Q12_ONE as i16));
        assert_eq!(impact.normal.y, 0);
        assert_eq!(impact.normal.z, 0);
    }

    #[test]
    fn monster_is_blocked_by_the_player_body() {
        let mut blockers = BodyBlockers::new();
        // The player body is a 32x32x56 box centred on its origin footprint.
        blockers.push(Body {
            source_index: PLAYER_BODY_SOURCE,
            mins: units(-16, 84, -24),
            maxs: units(16, 116, 32),
            dead: false,
        });
        let impact = blockers
            .resolve(units(0, 0, 0), units(0, 200, 0), 2)
            .expect("large monster hull meets the player box");
        assert_eq!(impact.source_index, PLAYER_BODY_SOURCE);
        // 84 - 32 (large hull) = 52 units of free travel.
        let stop = impact.end.y >> 12;
        assert!((51..=52).contains(&stop), "stopped at {stop}");
        assert_eq!(impact.normal.y, -(Q12_ONE as i16));
    }

    #[test]
    fn monster_is_blocked_by_another_monster_body() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(40, units(0, -120, 0)));
        let impact = blockers
            .resolve(units(0, 0, 0), units(0, -200, 0), 1)
            .expect("monster hull meets the other monster box");
        assert_eq!(impact.source_index, 40);
        assert_eq!(impact.normal.y, Q12_ONE as i16);
        let stop = impact.end.y >> 12;
        assert!((-89..=-88).contains(&stop), "stopped at {stop}");
    }

    #[test]
    fn a_corpse_never_blocks() {
        let mut blockers = BodyBlockers::new();
        let mut corpse = monster_body(21, units(96, 0, 0));
        corpse.dead = true;
        assert!(!blockers.push(corpse));
        assert!(blockers.is_empty());
        assert_eq!(blockers.refused(), 0);
        assert!(blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .is_none());
        assert!(body_impact(
            units(0, 0, 0),
            units(200, 0, 0),
            corpse,
            hull_extents(1).0,
            hull_extents(1).1
        )
        .is_none());
    }

    #[test]
    fn the_candidate_set_is_bounded_and_denies_on_full() {
        let mut blockers = BodyBlockers::new();
        for index in 0..MAX_BODY_CANDIDATES {
            assert!(blockers.push(monster_body(index as u16, units(96, 0, 0))));
        }
        assert_eq!(blockers.len(), MAX_BODY_CANDIDATES);
        // The ninth body is refused, counted, and cannot win the trace.
        assert!(!blockers.push(monster_body(900, units(40, 0, 0))));
        assert_eq!(blockers.refused(), 1);
        assert!(!blockers.push(monster_body(901, units(40, 0, 0))));
        assert_eq!(blockers.refused(), 2);
        assert_eq!(blockers.len(), MAX_BODY_CANDIDATES);
        let impact = blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .expect("an admitted body still blocks");
        assert_eq!(impact.source_index, 0);
    }

    #[test]
    fn equal_fractions_keep_the_lowest_source_index() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(7, units(96, 0, 0)));
        blockers.push(monster_body(9, units(96, 0, 0)));
        let impact = blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .expect("both bodies overlap the sweep");
        assert_eq!(impact.source_index, 7);
    }

    #[test]
    fn the_nearest_body_wins_regardless_of_push_order() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(3, units(160, 0, 0)));
        blockers.push(monster_body(5, units(96, 0, 0)));
        let impact = blockers
            .resolve(units(0, 0, 0), units(300, 0, 0), 1)
            .expect("both bodies overlap the sweep");
        assert_eq!(impact.source_index, 5);
    }

    #[test]
    fn a_resting_mover_can_still_walk_away_from_a_body() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(21, units(96, 0, 0)));
        let approach = blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .expect("approach blocks");
        // Retreating from the resting position must not report a block.
        let retreat = blockers.resolve(
            approach.end,
            Vec3I32 {
                x: approach.end.x - (40 << 12),
                ..approach.end
            },
            1,
        );
        assert!(retreat.is_none(), "retreat reported {retreat:?}");
        // Pressing back in still blocks, at the resting position.
        let again = blockers
            .resolve(
                approach.end,
                Vec3I32 {
                    x: approach.end.x + (40 << 12),
                    ..approach.end
                },
                1,
            )
            .expect("re-approach blocks");
        assert_eq!(again.fraction, 0);
        assert_eq!(again.normal.x, -(Q12_ONE as i16));
    }

    #[test]
    fn a_sweep_that_misses_the_body_is_not_blocked() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(21, units(0, 200, 0)));
        assert!(blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .is_none());
    }

    #[test]
    fn an_overlapped_start_is_skipped_rather_than_frozen() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(21, units(0, 0, 0)));
        assert!(blockers
            .resolve(units(0, 0, 0), units(200, 0, 0), 1)
            .is_none());
    }

    #[test]
    fn a_vertical_drop_onto_a_body_reports_a_floor_normal() {
        let mut blockers = BodyBlockers::new();
        blockers.push(monster_body(21, units(0, 0, 0)));
        let impact = blockers
            .resolve(units(0, 0, 160), units(0, 0, 0), 1)
            .expect("a drop onto the body blocks");
        assert_eq!(impact.normal.z, Q12_ONE as i16);
        // 40 (body maxs) + 24 (player hull mins) = 64 units above the origin.
        let stop = impact.end.z >> 12;
        assert!((64..=65).contains(&stop), "stopped at {stop}");
    }
}
