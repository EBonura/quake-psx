//! The cooked map, for the controller's own line-of-sight checks.

use quake_core::collision::{
    trace_render_bsp_into, CompactPlane, PointTraceScratch, SharedCollisionHull, Trace,
};
use quake_formats::resident::ResidentMap;
use quake_formats::{SliceReader, Vec3I32};

pub struct World {
    map: ResidentMap,
    /// The map's planes as the SDK's render hulls read them.
    planes: Vec<CompactPlane>,
}

const Q12_ONE: i32 = 4096;

impl World {
    pub fn load(path: &str) -> World {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut reader = SliceReader::new(&bytes);
        let mut map = ResidentMap::new();
        map.load(1, &mut reader).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        let planes = map
            .planes()
            .iter()
            .map(|plane| CompactPlane {
                normal: plane.normal,
                kind: plane.kind as u8,
                sign_bits: 0,
                distance: plane.distance,
            })
            .collect();
        World { map, planes }
    }

    /// The authored bounds (whole units) of every brush entity, keyed by cooked source index.
    pub fn mover_bounds(&self) -> std::collections::HashMap<u16, ([i32; 3], [i32; 3])> {
        let mut out = std::collections::HashMap::new();
        for (index, entity) in self.map.entities().iter().enumerate() {
            if entity.model < 0 {
                if let Some(m) = self.map.brush_models().get((-entity.model) as usize) {
                    out.insert(
                        index as u16,
                        (
                            [i32::from(m.mins.x), i32::from(m.mins.y), i32::from(m.mins.z)],
                            [i32::from(m.maxs.x), i32::from(m.maxs.y), i32::from(m.maxs.z)],
                        ),
                    );
                }
            }
        }
        out
    }

    /// True when the static world does not block the segment (Q12 points).
    pub fn los(&self, from: [i32; 3], to: [i32; 3]) -> bool {
        let world = self.map.brush_models().get(0).expect("world model");
        let mut scratch = PointTraceScratch::default();
        let mut trace = Trace::default();
        let start = Vec3I32 { x: from[0], y: from[1], z: from[2] };
        let end = Vec3I32 { x: to[0], y: to[1], z: to[2] };
        // SAFETY: the map was loaded and validated by `ResidentMap::load`.
        let hull = unsafe {
            SharedCollisionHull::from_render_bsp(
                &self.planes,
                self.map.nodes(),
                self.map.leaves(),
                world.head_nodes[0],
            )
        };
        let ok = trace_render_bsp_into(
            &hull,
            &start,
            &end,
            &mut scratch,
            &mut trace,
        );
        ok && trace.fraction >= Q12_ONE
    }

    /// True when there is floor within `drop` units below the feet at `(x, y)` (world
    /// units), looking down from the player's origin height `z`.
    pub fn floor_below(&self, x: i32, y: i32, z: i32, drop: i32) -> bool {
        let from = [x << 12, y << 12, z << 12];
        let to = [x << 12, y << 12, (z - 24 - drop) << 12];
        !self.los(from, to)
    }
}
