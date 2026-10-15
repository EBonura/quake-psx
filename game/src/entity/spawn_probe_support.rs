//! Read-only scene accessors for `crate::spawn_probe`.
//!
//! A child module of `entity`, so it reaches the private scene without
//! widening the scene's public surface. Nothing here changes game state.

use quake_core::collision::{Trace, TraceScratch, Q12_ONE};
use quake_core::monster::MonsterState;
use quake_formats::Vec3I32;

use super::EntityScene;
use crate::asset::ResidentMap;

/// One monster as the probe reports it.
pub struct ProbeRow {
    pub source_index: u16,
    pub origin: Vec3I32,
    pub yaw: i16,
    pub state: MonsterState,
    pub active: bool,
    pub health: i16,
    /// Walking monster on the ground (not a flyer, boss, hanging zombie or a
    /// monster mid-leap).
    pub walks: bool,
    pub hull: usize,
}

impl EntityScene {
    /// One row per monster, in scene order.
    pub fn spawn_probe_rows(&self) -> impl Iterator<Item = ProbeRow> + '_ {
        self.entities.iter().filter_map(|entity| {
            let monster = entity.monster?;
            let kind = monster.kind();
            Some(ProbeRow {
                source_index: entity.source_index,
                origin: entity.origin,
                yaw: entity.angles.y,
                state: monster.state(),
                active: monster.active(),
                health: entity.health,
                // A leaping dog or fiend is in the air by design, not hanging.
                walks: !kind.flies()
                    && !kind.is_boss()
                    && !monster.crucified()
                    && !monster.leaping(),
                hull: kind.collision_hull(),
            })
        })
    }

    /// `SV_CheckBottom`'s full check, written straight from the engine
    /// source and independent of the movement code's quick pass: the box
    /// midpoint must find floor within two steps (36 units) below the box,
    /// and so must each bottom corner, none more than one step (18) lower than
    /// the midpoint's floor. A monster that fails this is hanging off an edge.
    pub fn spawn_probe_supported(
        &self,
        map: &ResidentMap,
        origin: Vec3I32,
        hull_index: usize,
    ) -> bool {
        let half = if hull_index == 2 { 32 << 12 } else { 16 << 12 };
        let bottom = origin.z - (24 << 12);
        let reach = 36 << 12;
        let mut scratch = TraceScratch::default();
        let mut trace = Trace::default();
        let mut probe = |x: i32, y: i32| -> Option<i32> {
            let start = Vec3I32 { x, y, z: bottom };
            let stop = Vec3I32 {
                z: bottom - reach,
                ..start
            };
            if !self.trace_point(map, &start, &stop, &mut scratch, &mut trace) {
                return None;
            }
            (trace.fraction != Q12_ONE).then_some(trace.end.z)
        };
        let Some(mid) = probe(origin.x, origin.y) else {
            return false;
        };
        for (x, y) in [
            (origin.x - half, origin.y - half),
            (origin.x + half, origin.y - half),
            (origin.x - half, origin.y + half),
            (origin.x + half, origin.y + half),
        ] {
            match probe(x, y) {
                Some(floor) if mid - floor <= 18 << 12 => {}
                _ => return false,
            }
        }
        true
    }
}
