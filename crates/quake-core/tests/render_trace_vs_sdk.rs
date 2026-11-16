//! Quake's own render-BSP point trace against the SDK's `CollisionHull::from_render_bsp`,
//! over every cooked map, the world and every brush model.

use psx_bsp::CompactPlane;
use psx_bsp::collision::{CollisionHull, Trace as SdkTrace, TraceScratch};
use quake_core::collision::{trace_render_bsp_into, RenderTraceScratch, Trace};
use quake_formats::{resident::ResidentMap, SliceReader, Vec3I32};

#[test]
fn the_own_render_trace_and_the_sdks_agree() {
    let (mut checked, mut equal, mut ok_differs) = (0u64, 0u64, 0u64);
    let (mut open_checked, mut open_equal) = (0u64, 0u64);
    let (mut flags_differ, mut fraction_differs, mut max_fraction_delta, mut both_hit) = (0u64, 0u64, 0i32, 0u64);
    let mut first = None;
    for map in ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"] {
        let path = format!("{}/../../id1psx/maps/{map}.psb", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap();
        let mut reader = SliceReader::new(&bytes);
        let mut resident = ResidentMap::new();
        resident.load(0, &mut reader).unwrap();
        let planes: Vec<CompactPlane> = resident
            .planes()
            .iter()
            .map(|p| CompactPlane { normal: p.normal, kind: p.kind as u8, sign_bits: 0, distance: p.distance })
            .collect();
        let nodes = resident.nodes();
        let leaves = resident.leaves();
        let mut seed = 0x9e37_79b9u32 ^ map.len() as u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for model in resident.brush_models().iter() {
            let head = model.head_nodes[0];
            // SAFETY: the cooked lumps are validated at load; the indices come from them.
            let hull = unsafe { CollisionHull::from_render_bsp(&planes, nodes, leaves, head) };
            let span = |axis: usize| {
                let (lo, hi) = match axis {
                    0 => (model.mins.x, model.maxs.x),
                    1 => (model.mins.y, model.maxs.y),
                    _ => (model.mins.z, model.maxs.z),
                };
                (i32::from(lo) - 64, i32::from(hi) + 64)
            };
            for _ in 0..2000 {
                let mut point = || {
                    let mut v = [0i32; 3];
                    for (a, slot) in v.iter_mut().enumerate() {
                        let (lo, hi) = span(a);
                        let w = (hi - lo).max(1) as u32;
                        *slot = (lo + (next() % w) as i32) << 12 | (next() & 0xfff) as i32;
                    }
                    Vec3I32 { x: v[0], y: v[1], z: v[2] }
                };
                let (start, end) = (point(), point());
                let mut own = Trace::default();
                let mut sdk = SdkTrace::default();
                let own_ok = trace_render_bsp_into(
                    planes.as_slice(), nodes, leaves, head, &start, &end,
                    &mut RenderTraceScratch::default(), &mut own,
                );
                let sdk_ok = hull.trace_into(&start, &end, &mut TraceScratch::default(), &mut sdk);
                checked += 1;
                if own_ok && !own.start_solid {
                    open_checked += 1;
                    open_equal += u64::from(same(&own, &sdk));
                }
                if own_ok && sdk_ok {
                    if (own.all_solid, own.start_solid, own.in_open, own.in_water)
                        != (sdk.all_solid.is_set(), sdk.start_solid.is_set(), sdk.in_open.is_set(), sdk.in_water.is_set())
                    {
                        flags_differ += 1;
                    } else if own.fraction < 4096 && sdk.fraction < 4096 {
                        both_hit += 1;
                        if own.fraction != sdk.fraction {
                            fraction_differs += 1;
                            max_fraction_delta = max_fraction_delta.max((own.fraction - sdk.fraction).abs());
                        }
                    }
                }
                if own_ok != sdk_ok {
                    ok_differs += 1;
                } else if same(&own, &sdk) {
                    equal += 1;
                } else if first.is_none() {
                    first = Some((map, start, end, own, sdk));
                }
            }
        }
    }
    println!("flags differ {flags_differ}; flags equal and both hit: {both_hit}, fraction differs {fraction_differs} (max {max_fraction_delta} of 4096)");
    println!("checked {checked}, identical {equal}, success flag differs {ok_differs}; start in open space: {open_checked} checked, {open_equal} identical");
    if let Some((map, s, e, own, sdk)) = &first {
        println!("first difference {map} {s:?} -> {e:?}\n own {own:?}\n sdk {sdk:?}");
    }
}

fn same(own: &Trace, sdk: &SdkTrace) -> bool {
    own.all_solid == sdk.all_solid.is_set()
        && own.start_solid == sdk.start_solid.is_set()
        && own.in_open == sdk.in_open.is_set()
        && own.in_water == sdk.in_water.is_set()
        && own.fraction == sdk.fraction
        && (own.end.x, own.end.y, own.end.z) == (sdk.end.x, sdk.end.y, sdk.end.z)
        && (own.normal.x, own.normal.y, own.normal.z) == (sdk.normal.x, sdk.normal.y, sdk.normal.z)
        && own.plane_distance == sdk.plane_distance
}

#[test]
fn trace_speed_world_model() {
    let path = format!("{}/../../id1psx/maps/e1m1.psb", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap();
    let mut reader = SliceReader::new(&bytes);
    let mut resident = ResidentMap::new();
    resident.load(0, &mut reader).unwrap();
    let planes: Vec<CompactPlane> = resident
        .planes()
        .iter()
        .map(|p| CompactPlane { normal: p.normal, kind: p.kind as u8, sign_bits: 0, distance: p.distance })
        .collect();
    let (nodes, leaves) = (resident.nodes(), resident.leaves());
    let model = resident.brush_models().get(0).unwrap();
    let head = model.head_nodes[0];
    let hull = unsafe { CollisionHull::from_render_bsp(&planes, nodes, leaves, head) };
    let mut seed = 77u32;
    let mut next = move || { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed };
    let mut segs = Vec::new();
    for _ in 0..200_000 {
        let mut p = || {
            let mut v = [0i32; 3];
            for (a, slot) in v.iter_mut().enumerate() {
                let (lo, hi) = match a { 0 => (model.mins.x, model.maxs.x), 1 => (model.mins.y, model.maxs.y), _ => (model.mins.z, model.maxs.z) };
                let w = (i32::from(hi) - i32::from(lo)).max(1) as u32;
                *slot = (i32::from(lo) + (next() % w) as i32) << 12;
            }
            Vec3I32 { x: v[0], y: v[1], z: v[2] }
        };
        segs.push((p(), p()));
    }
    let t = std::time::Instant::now();
    let mut sink = 0i64;
    for (a, b) in &segs {
        let mut o = Trace::default();
        trace_render_bsp_into(planes.as_slice(), nodes, leaves, head, a, b, &mut RenderTraceScratch::default(), &mut o);
        sink += i64::from(o.fraction);
    }
    let own = t.elapsed();
    let t = std::time::Instant::now();
    for (a, b) in &segs {
        let mut o = SdkTrace::default();
        hull.trace_into(a, b, &mut TraceScratch::default(), &mut o);
        sink += i64::from(o.fraction);
    }
    let sdk = t.elapsed();
    println!("speed: own {:?}, sdk {:?} for 200000 traces (sink {sink})", own, sdk);
}
