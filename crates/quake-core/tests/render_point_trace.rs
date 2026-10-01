//! Cooked E1M1/E1M2 proof that point rays use render BSPs, not clipnodes.

use quake_core::collision::{
    trace_render_bsp_into, trace_translated_render_bsp_into, CollisionHull, RenderTraceScratch,
    Trace, TraceScratch, Q12_ONE,
};
use quake_formats::{resident::ResidentMap, SliceReader, Vec3I32};

#[test]
fn translated_brush_render_hull_blocks_the_shifted_ray() {
    let path = format!("{}/../../id1psx/maps/e1m1.psb", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut reader = SliceReader::new(&bytes);
    let mut resident = ResidentMap::new();
    resident.load(0, &mut reader).expect("resident E1M1");
    let planes = resident.planes();
    let nodes = resident.nodes();
    let leaves = resident.leaves();
    let clip_nodes = resident.clip_nodes();
    let models = resident.brush_models();
    let entities = resident.entities();
    let door_entity = entities
        .iter()
        .filter(|entity| entity.class_name == 0x0c && entity.model < 0)
        .nth(1)
        .expect("second half of E1M1's first linked func_door");
    let door = models
        .get(door_entity.model.saturating_neg() as usize)
        .expect("E1M1 door model");
    let origin = q12(128, -64, 32);
    let local_center = Vec3I32 {
        x: (i32::from(door.mins.x) + i32::from(door.maxs.x)) / 2 << 12,
        y: (i32::from(door.mins.y) + i32::from(door.maxs.y)) / 2 << 12,
        z: (i32::from(door.mins.z) + i32::from(door.maxs.z)) / 2 << 12,
    };
    let spans = [
        i32::from(door.maxs.x) - i32::from(door.mins.x),
        i32::from(door.maxs.y) - i32::from(door.mins.y),
        i32::from(door.maxs.z) - i32::from(door.mins.z),
    ];
    let thin_axis = (0..3).min_by_key(|&axis| spans[axis]).unwrap();
    let translated_center = Vec3I32 {
        x: local_center.x.saturating_add(origin.x),
        y: local_center.y.saturating_add(origin.y),
        z: local_center.z.saturating_add(origin.z),
    };
    let mut start = translated_center;
    let mut end = translated_center;
    // Cross the model through its thinnest dimension, which is the authored
    // door slab rather than an arbitrary decorative submodel axis.
    match thin_axis {
        0 => {
            start.x = start.x.saturating_sub(256 << 12);
            end.x = end.x.saturating_add(256 << 12);
        }
        1 => {
            start.y = start.y.saturating_sub(256 << 12);
            end.y = end.y.saturating_add(256 << 12);
        }
        _ => {
            start.z = start.z.saturating_sub(256 << 12);
            end.z = end.z.saturating_add(256 << 12);
        }
    }
    let mut trace = Trace::default();
    assert!(trace_translated_render_bsp_into(
        planes,
        nodes,
        leaves,
        door.head_nodes[0],
        origin,
        &start,
        &end,
        &mut RenderTraceScratch::default(),
        &mut trace,
    ));
    assert!(
        trace.fraction < Q12_ONE,
        "translated closed linked door must block the ray"
    );
    assert!(trace.end.x >= start.x && trace.end.x <= end.x);

    // This reproduces the shipping defect: the render-node head was treated
    // as a clipnode index and the same closed door became transparent.
    let legacy = CollisionHull::new(planes, clip_nodes, door.head_nodes[0]).unwrap();
    let mut wrong = Trace::default();
    assert!(legacy.trace_into(&start, &end, &mut TraceScratch::default(), &mut wrong,));
    assert_eq!(wrong.fraction, Q12_ONE);
}

fn q12(x: i32, y: i32, z: i32) -> Vec3I32 {
    Vec3I32 {
        x: x << 12,
        y: y << 12,
        z: z << 12,
    }
}

#[test]
fn e1m2_shootable_button_ray_is_clear_but_a_static_wall_still_blocks() {
    let path = format!("{}/../../id1psx/maps/e1m2.psb", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut reader = SliceReader::new(&bytes);
    let mut resident = ResidentMap::new();
    resident.load(0, &mut reader).expect("resident E1M2");
    let planes = resident.planes();
    let nodes = resident.nodes();
    let leaves = resident.leaves();
    let clip_nodes = resident.clip_nodes();
    let models = resident.brush_models();
    let world = models.get(0).expect("world model");

    // Ordinary reachable firing position to the center of E1M2 button #243.
    let start = q12(1460, -460, 334);
    let button = q12(1546, -552, 328);
    let mut trace = Trace::default();
    assert!(trace_render_bsp_into(
        planes,
        nodes,
        leaves,
        world.head_nodes[0],
        &start,
        &button,
        &mut RenderTraceScratch::default(),
        &mut trace,
    ));
    assert_eq!(trace.fraction, Q12_ONE, "button #243 must be shootable");

    // This is the exact old bug: render head zero was interpreted as a
    // clipnode index. It reports a wall before the visible button and would
    // discard every shipping shotgun pellet.
    let legacy = CollisionHull::new(planes, clip_nodes, world.head_nodes[0]).unwrap();
    let mut wrong = Trace::default();
    assert!(legacy.trace_into(&start, &button, &mut TraceScratch::default(), &mut wrong,));
    assert!(
        wrong.fraction < Q12_ONE,
        "old path must reproduce the defect"
    );

    // Correct point tracing is not wall forgiveness: the north wall from the
    // same authored firing position still clips this longer ray.
    let behind_wall = q12(1460, 0, 334);
    assert!(trace_render_bsp_into(
        planes,
        nodes,
        leaves,
        world.head_nodes[0],
        &start,
        &behind_wall,
        &mut RenderTraceScratch::default(),
        &mut trace,
    ));
    assert!(
        trace.fraction < Q12_ONE,
        "static wall must remain occluding"
    );
}

/// The game traces through the resident map's aligned node and plane views;
/// they must answer exactly as the cooked records do, for the world and every
/// brush model, on every map.
#[test]
fn native_render_views_trace_exactly_like_the_cooked_records() {
    use psx_bsp::{CompactNode, CompactPlane};
    let mut checked = 0u32;
    let mut blocked = 0u32;
    for map in [
        "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
    ] {
        let path = format!("{}/../../id1psx/maps/{map}.psb", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let mut reader = SliceReader::new(&bytes);
        let mut resident = ResidentMap::new();
        resident.load(0, &mut reader).expect("resident map");
        let planes = resident.planes();
        let nodes = resident.nodes();
        let leaves = resident.leaves();
        // The same conversion the game's resident map makes at load.
        let compact_planes: Vec<CompactPlane> = planes
            .iter()
            .map(|plane| CompactPlane {
                normal: plane.normal,
                kind: plane.kind as u8,
                sign_bits: 0,
                distance: plane.distance,
            })
            .collect();
        let compact_nodes: &[CompactNode] = nodes
            .as_native_compact_nodes()
            .expect("aligned render nodes");
        let mut seed = 0x1234_5678u32 ^ map.len() as u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for model in resident.brush_models().iter() {
            let span = |axis: usize| {
                let (low, high) = match axis {
                    0 => (model.mins.x, model.maxs.x),
                    1 => (model.mins.y, model.maxs.y),
                    _ => (model.mins.z, model.maxs.z),
                };
                (i32::from(low) - 64, i32::from(high) + 64)
            };
            for _ in 0..200 {
                let mut point = || {
                    let mut value = [0i32; 3];
                    for (axis, slot) in value.iter_mut().enumerate() {
                        let (low, high) = span(axis);
                        let width = (high - low).max(1) as u32;
                        *slot = (low + (next() % width) as i32) << 12 | (next() & 0xfff) as i32;
                    }
                    Vec3I32 {
                        x: value[0],
                        y: value[1],
                        z: value[2],
                    }
                };
                let (start, end) = (point(), point());
                let origin = Vec3I32 {
                    x: (next() % 129) as i32 - 64 << 12,
                    y: (next() % 129) as i32 - 64 << 12,
                    z: 0,
                };
                let mut cooked = Trace::default();
                let mut native = Trace::default();
                let cooked_ok = trace_translated_render_bsp_into(
                    planes,
                    nodes,
                    leaves,
                    model.head_nodes[0],
                    origin,
                    &start,
                    &end,
                    &mut RenderTraceScratch::default(),
                    &mut cooked,
                );
                let native_ok = trace_translated_render_bsp_into(
                    compact_planes.as_slice(),
                    compact_nodes,
                    leaves,
                    model.head_nodes[0],
                    origin,
                    &start,
                    &end,
                    &mut RenderTraceScratch::default(),
                    &mut native,
                );
                assert_eq!(cooked_ok, native_ok, "{map}");
                assert_eq!(cooked, native, "{map}");
                checked += 1;
                blocked += u32::from(cooked_ok && cooked.fraction < Q12_ONE);
            }
        }
    }
    assert!(checked > 10_000);
    assert!(
        blocked > checked / 10,
        "{blocked} of {checked} rays blocked"
    );
}
