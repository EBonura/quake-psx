//! The cooked map, for the controller's own line-of-sight checks.

use quake_core::collision::{trace_render_bsp_into, RenderTraceScratch, Trace};
use quake_formats::resident::ResidentMap;
use quake_formats::{SliceReader, Vec3I32};

pub struct World {
    map: ResidentMap,
}

const Q12_ONE: i32 = 4096;

impl World {
    pub fn load(path: &str) -> World {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut reader = SliceReader::new(&bytes);
        let mut map = ResidentMap::new();
        map.load(1, &mut reader).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        World { map }
    }

    /// True when the static world does not block the segment (Q12 points).
    pub fn los(&self, from: [i32; 3], to: [i32; 3]) -> bool {
        let world = self.map.brush_models().get(0).expect("world model");
        let mut scratch = RenderTraceScratch::default();
        let mut trace = Trace::default();
        let start = Vec3I32 { x: from[0], y: from[1], z: from[2] };
        let end = Vec3I32 { x: to[0], y: to[1], z: to[2] };
        let ok = trace_render_bsp_into(
            self.map.planes(),
            self.map.nodes(),
            self.map.leaves(),
            world.head_nodes[0],
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
