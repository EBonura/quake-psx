//! Host route-authoring aid for the headless route gates.
//!
//! The route gates (`start-route-regress`, `e1m1-chain-regress`,
//! `systems-regress`, `bestiary-regress`) drive the player with ordinary pad
//! input over real cooked map data, so authoring or debugging one of their
//! waypoint lists normally costs a guest build plus a full emulator run. This
//! tool links the same `quake-core` movement and collision the guest links and
//! runs them over the same cooked `.psb`, so a candidate route is answered in
//! milliseconds.
//!
//! It is an aid, not an oracle. It reproduces the world hull, the solid brush
//! submodels, and the skill and episode-rune spawn rules, but it does not run
//! monsters, movers, triggers, or the guest's frame pacing. A route that fails
//! here will fail on the guest; a route that passes here still has to be proved
//! by the gate.
//!
//! ```text
//! cargo run --release --manifest-path tools/routesim/Cargo.toml -- \
//!     id1psx/maps/start.psb solids
//!
//! printf '%s\n' '-64 1740 - 24' | cargo run --release \
//!     --manifest-path tools/routesim/Cargo.toml -- \
//!     id1psx/maps/start.psb route --from 544 1536 43 --vel 0 300 0 --yaw 1024
//! ```
//!
//! Modes:
//! * `dump`     - every cooked entity with its class, flags, model and origin.
//! * `solids`   - the solid brush submodels this map presents to the player.
//! * `probe`    - an ASCII slice of hull-1 solidity over a world rectangle.
//! * `contents` - an ASCII slice of leaf contents over a world rectangle.
//! * `floors`   - the hull-1 floor height under every cell of a rectangle.
//! * `trace`    - one segment through the world and every solid submodel.
//! * `brushes`  - every brush entity with its authored volume and fields.
//! * `reach`    - flood the walkable set from a point as an ASCII height map.
//! * `path`     - breadth-first walkable route between authored points.
//! * `route`    - walk a waypoint list from stdin with ordinary movement input.
//!
//! Movers are placed where they SPAWN, not where their brush was authored,
//! because those are different places for two of them. An untargeted
//! `func_plat` spawns at its low position. A `DOOR_START_OPEN` door spawns one
//! whole travel away from its brush and comes back when it is triggered, so
//! its authored volume is geometry the map only has AFTER the trigger: E1M7's
//! lava bridge is two such doors, and modelling them at their brush made the
//! bridge look permanent and the exit chamber below look like an unreachable
//! 78-cell island.
//!
//! `trigger_teleport` is traversed, not walked around. The flood, the path
//! search and the route walker all carry the player through an open teleport
//! volume to its authored `info_teleport_destination`, using the same rules
//! `quake-core::teleport` gives the guest: the 27-unit destination rise, the
//! 300-unit exit push along the destination angles, and the gate that keeps a
//! targetnamed teleporter shut until something fires it. Without this the
//! flood stopped dead at every teleporter, which is what kept E1M2, E1M3,
//! E1M5, E1M6 and E1M8 unauthorable.
//!
//! Environment: `ROUTESIM_SKILL` (0 easy, default), `ROUTESIM_RUNES` (the
//! `serverflags` rune mask, default 0), `ROUTESIM_TICKS` (movement ticks per
//! route frame, default 4), `ROUTESIM_GRAVITY` (800 normally, 100 for E1M8),
//! `ROUTESIM_VERBOSE`, `ROUTESIM_OPEN` (a comma
//! separated list of cooked entity indices the route has already triggered, so
//! a leg behind a door that opens earlier in the route can still be authored;
//! an ordinary mover is dropped, a `DOOR_START_OPEN` door is instead put back
//! at its brush), `ROUTESIM_OPEN_MOVERS` (drop every door, secret door and
//! lift at once, for a first look at what a map holds), `ROUTESIM_PLAT_TOP`
//! (keep lifts raised, for authoring the leg that starts on top of one),
//! `ROUTESIM_ENABLE_TELEPORTS` (a comma separated list of targetnamed
//! `trigger_teleport` indices the route has already opened; untargeted ones
//! are always open), `ROUTESIM_NO_TELEPORTS` (walk around them instead, the
//! old behaviour).

use psx_math::int32::mul_q12_i32;
use quake_core::collision::{
    trace_render_bsp_into, trace_translated_render_bsp_into, BrushTransform, CollisionHull,
    RenderTraceScratch, Trace, TraceScratch,
};
use quake_core::movement::{
    MovementInput, MovementScratch, MovementState, MovementTrace, MovementTraceResult,
};
use quake_formats::resident::ResidentMap;
use quake_formats::{SliceReader, Vec3I16, Vec3I32};

/// Cooked class ids from `tools/cfg/id1/entmap.txt`.
const CLASS_FUNC_BOSSGATE: u8 = 0x0a;
const CLASS_FUNC_EPISODEGATE: u8 = 0x0e;
const CLASS_FUNC_BUTTON: u8 = 0x0b;
const CLASS_FUNC_DOOR: u8 = 0x0c;
const CLASS_FUNC_DOOR_SECRET: u8 = 0x0d;
const CLASS_FUNC_PLAT: u8 = 0x10;
const CLASS_FUNC_TRAIN: u8 = 0x11;
const CLASS_FUNC_WALL: u8 = 0x12;

/// Every episode rune bit, `serverflags & 15` in the original.
const RUNE_MASK: u8 = 0x0f;

fn brush_model_is_solid(class_name: u8) -> bool {
    matches!(
        class_name,
        CLASS_FUNC_BOSSGATE
            | CLASS_FUNC_EPISODEGATE
            | CLASS_FUNC_BUTTON
            | CLASS_FUNC_DOOR
            | CLASS_FUNC_DOOR_SECRET
            | CLASS_FUNC_PLAT
            | CLASS_FUNC_TRAIN
            | CLASS_FUNC_WALL
    )
}

/// `func_plat`'s Q20.12 travel: the authored `height`, or the deck's own
/// thickness less eight units when the map authors none.
fn plat_travel(height: i16, maxs_z: i16, mins_z: i16) -> i32 {
    if height != 0 {
        return i32::from(height) << 12;
    }
    ((i32::from(maxs_z) - i32::from(mins_z)) << 12)
        .saturating_sub(8 << 12)
        .max(1 << 12)
}

/// `func_door`'s `DOOR_START_OPEN`. Quake spawns such a door already displaced
/// by its whole travel and then moves it BACK to the position the brush was
/// authored in when it is triggered, so its authored volume is the shape the
/// map presents AFTER the trigger, not before it. `QuakeMover::from_entity`
/// implements this by swapping the endpoints; modelling every door at its
/// authored position instead made E1M7's lava bridge look permanent and its
/// exit chamber look like an unreachable island.
const DOOR_START_OPEN: u16 = 1;

/// The Q20.12 offset a `func_door`/`func_door_secret` travels, straight off
/// `QuakeMover::from_entity`: the authored move direction, the authored `lip`
/// (`count`, eight when the map authors none), and the brush's own size.
fn door_open_offset(angles: Vec3I16, count: i16, mins: Vec3I16, maxs: Vec3I16) -> Vec3I32 {
    let direction = quake_core::mover::move_direction(angles);
    let size = Vec3I32 {
        x: (i32::from(maxs.x) - i32::from(mins.x)) << 12,
        y: (i32::from(maxs.y) - i32::from(mins.y)) << 12,
        z: (i32::from(maxs.z) - i32::from(mins.z)) << 12,
    };
    let lip = i32::from(if count == 0 { 8 } else { count }) << 12;
    let dot = ((i64::from(direction.x) * i64::from(size.x)
        + i64::from(direction.y) * i64::from(size.y)
        + i64::from(direction.z) * i64::from(size.z))
        >> 12) as i32;
    let distance = dot.saturating_abs().saturating_sub(lip);
    Vec3I32 {
        x: ((i64::from(direction.x) * i64::from(distance)) >> 12) as i32,
        y: ((i64::from(direction.y) * i64::from(distance)) >> 12) as i32,
        z: ((i64::from(direction.z) * i64::from(distance)) >> 12) as i32,
    }
}

/// `SPAWNFLAG_NOT_EASY/MEDIUM/HARD`.
fn excluded_for_skill(spawn_flags: u16, skill: u8) -> bool {
    let bit = match skill {
        0 => 0x0100,
        1 => 0x0200,
        _ => 0x0400,
    };
    spawn_flags & bit != 0
}

struct Solid {
    source_index: usize,
    class_name: u8,
    model_index: usize,
    origin: Vec3I32,
}

/// One open `trigger_teleport` volume and where it puts the player.
struct Teleport {
    source_index: usize,
    /// Q20.12 world bounds of the trigger brush.
    mins: Vec3I32,
    maxs: Vec3I32,
    /// Q20.12 arrival origin, already raised the authored 27 units.
    destination: Vec3I32,
    /// Q20.12 exit push the arrival carries.
    exit_velocity: Vec3I32,
    destination_yaw: u16,
}

struct Scene {
    map: ResidentMap,
    solids: Vec<Solid>,
    teleports: Vec<Teleport>,
}

impl Scene {
    fn load(path: &str, skill: u8, runes: u8) -> Self {
        // Named entities a route has already opened by the time it needs the
        // geometry behind them. The guest still has to open them for real.
        let opened: Vec<usize> = std::env::var("ROUTESIM_OPEN")
            .unwrap_or_default()
            .split(',')
            .filter_map(|entry| entry.trim().parse().ok())
            .collect();
        // Movers whose fully travelled brush remains part of the route. This
        // differs from `ROUTESIM_OPEN`, which removes an opened blocker: E1M3
        // builds its staircase from five downward-moving doors, so dropping
        // those brushes would erase the floor the guest actually walks on.
        let moved_to_top: Vec<usize> = std::env::var("ROUTESIM_TOP")
            .unwrap_or_default()
            .split(',')
            .filter_map(|entry| entry.trim().parse().ok())
            .collect();
        // The blunt version of the same thing, for a first look at a map.
        let open = std::env::var("ROUTESIM_OPEN_MOVERS").is_ok();
        // `ROUTESIM_PLAT_TOP` keeps every lift at the position it was authored
        // in, which is where it stands once a rider has ridden it up. Authoring
        // the leg that starts on a raised lift needs that geometry.
        let plats_raised = std::env::var("ROUTESIM_PLAT_TOP").is_ok();
        let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let mut reader = SliceReader::new(&bytes);
        let mut map = ResidentMap::new();
        map.load(1, &mut reader)
            .unwrap_or_else(|error| panic!("{path}: {error:?}"));
        let mut solids = Vec::new();
        // The guest's own spawn rules: skill filtering, then the rune gates.
        // `func_episodegate` only exists once its episode is finished and
        // `func_bossgate` disappears once all four runes are in hand.
        for (source_index, entity) in map.entities().iter().enumerate().skip(2) {
            if excluded_for_skill(entity.spawn_flags, skill) {
                continue;
            }
            // A `START_OPEN` door is the one mover whose triggered state is
            // MORE solid than its spawn state, so `ROUTESIM_OPEN` cannot mean
            // "drop it" there: triggering it puts the brush back where it was
            // authored. Every other mover keeps the existing approximation of
            // a fully travelled mover, which is that it is out of the way.
            let start_open = matches!(entity.class_name, CLASS_FUNC_DOOR | CLASS_FUNC_DOOR_SECRET)
                && entity.spawn_flags & DOOR_START_OPEN != 0;
            let triggered = opened.contains(&source_index) || moved_to_top.contains(&source_index);
            if triggered && !start_open {
                if !moved_to_top.contains(&source_index) {
                    continue;
                }
            }
            if entity.class_name == CLASS_FUNC_EPISODEGATE
                && runes & (entity.spawn_flags as u8 & RUNE_MASK) == 0
            {
                continue;
            }
            if entity.class_name == CLASS_FUNC_BOSSGATE && runes & RUNE_MASK == RUNE_MASK {
                continue;
            }
            if open
                && matches!(
                    entity.class_name,
                    CLASS_FUNC_DOOR | CLASS_FUNC_DOOR_SECRET | CLASS_FUNC_PLAT
                )
            {
                continue;
            }
            if entity.model < 0 && brush_model_is_solid(entity.class_name) {
                let model_index = (-entity.model) as usize;
                let mut origin = entity.origin;
                // `func_plat` without a `targetname` is moved to its low
                // position at spawn, so the deck a route steps onto is not
                // where the brush was authored. The guest does this too;
                // leaving it out made the lift look like a solid ledge.
                if entity.class_name == CLASS_FUNC_PLAT && entity.target_name == 0 && !plats_raised
                {
                    if let Some(model) = map.brush_models().get(model_index) {
                        origin.z -= plat_travel(entity.height, model.maxs.z, model.mins.z);
                    }
                }
                // A `START_OPEN` door spawns one whole travel away from its
                // authored brush and comes back on the trigger, so its
                // untriggered volume is the displaced one.
                if start_open && !triggered {
                    if let Some(model) = map.brush_models().get(model_index) {
                        let offset =
                            door_open_offset(entity.angles, entity.count, model.mins, model.maxs);
                        origin.x += offset.x;
                        origin.y += offset.y;
                        origin.z += offset.z;
                    }
                } else if moved_to_top.contains(&source_index)
                    && matches!(entity.class_name, CLASS_FUNC_DOOR | CLASS_FUNC_BUTTON)
                {
                    if let Some(model) = map.brush_models().get(model_index) {
                        let offset =
                            door_open_offset(entity.angles, entity.count, model.mins, model.maxs);
                        origin.x += offset.x;
                        origin.y += offset.y;
                        origin.z += offset.z;
                    }
                }
                solids.push(Solid {
                    source_index,
                    class_name: entity.class_name,
                    model_index,
                    origin,
                });
            }
        }
        let teleports = collect_teleports(&map, skill);
        if std::env::var("ROUTESIM_VERBOSE").is_ok() {
            for teleport in &teleports {
                eprintln!(
                    "teleporter #{} volume ({},{},{})..({},{},{}) -> ({},{},{})",
                    teleport.source_index,
                    units(teleport.mins.x),
                    units(teleport.mins.y),
                    units(teleport.mins.z),
                    units(teleport.maxs.x),
                    units(teleport.maxs.y),
                    units(teleport.maxs.z),
                    units(teleport.destination.x),
                    units(teleport.destination.y),
                    units(teleport.destination.z),
                );
            }
        }
        Self {
            map,
            solids,
            teleports,
        }
    }

    /// The teleporter whose volume the standing player box at `origin`
    /// overlaps, if any. Q20.12 in, cooked entity index out.
    fn teleport_at(&self, origin: Vec3I32) -> Option<&Teleport> {
        let mins = Vec3I32 {
            x: origin.x - (PLAYER_HALF_WIDTH << 12),
            y: origin.y - (PLAYER_HALF_WIDTH << 12),
            z: origin.z - (PLAYER_DOWN << 12),
        };
        let maxs = Vec3I32 {
            x: origin.x + (PLAYER_HALF_WIDTH << 12),
            y: origin.y + (PLAYER_HALF_WIDTH << 12),
            z: origin.z + (PLAYER_UP << 12),
        };
        self.teleports.iter().find(|teleport| {
            maxs.x >= teleport.mins.x
                && mins.x <= teleport.maxs.x
                && maxs.y >= teleport.mins.y
                && mins.y <= teleport.maxs.y
                && maxs.z >= teleport.mins.z
                && mins.z <= teleport.maxs.z
        })
    }

    fn world_hull(&self) -> CollisionHull<'_> {
        let world = self.map.brush_models().get(0).expect("world brush model");
        CollisionHull::new(
            self.map.planes(),
            self.map.clip_nodes(),
            world.head_nodes[1],
        )
        .expect("world hull")
    }

    fn solid_hull(&self, solid: &Solid) -> Option<CollisionHull<'_>> {
        let model = self.map.brush_models().get(solid.model_index)?;
        CollisionHull::new(
            self.map.planes(),
            self.map.clip_nodes(),
            model.head_nodes[1],
        )
    }
}

/// Quake's player hull, the same box `pickup_touch_bounds` and the guest's
/// trigger overlap use.
const PLAYER_HALF_WIDTH: i32 = 16;
const PLAYER_DOWN: i32 = 24;
const PLAYER_UP: i32 = 32;

/// Cooked `trigger_teleport` and `info_teleport_destination`.
const CLASS_TRIGGER_TELEPORT: u8 = 0x52;

/// Every teleporter the route may pass through, resolved to its authored
/// destination exactly like `quake_core::teleport::resolve_destination`.
///
/// A teleporter with no `targetname` is permanently open. A targetnamed one is
/// shut until something fires it, so it only appears here when the caller
/// names it in `ROUTESIM_ENABLE_TELEPORTS`, which is the authoring claim "the
/// route has already opened this".
fn collect_teleports(map: &ResidentMap, skill: u8) -> Vec<Teleport> {
    if std::env::var("ROUTESIM_NO_TELEPORTS").is_ok() {
        return Vec::new();
    }
    let enabled: Vec<usize> = std::env::var("ROUTESIM_ENABLE_TELEPORTS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.trim().parse().ok())
        .collect();
    let mut teleports = Vec::new();
    for (source_index, entity) in map.entities().iter().enumerate() {
        if entity.class_name != CLASS_TRIGGER_TELEPORT || entity.model >= 0 {
            continue;
        }
        if excluded_for_skill(entity.spawn_flags, skill) {
            continue;
        }
        if entity.target_name != 0 && !enabled.contains(&source_index) {
            continue;
        }
        let model_index = (-entity.model) as usize;
        let Some(model) = map.brush_models().get(model_index) else {
            continue;
        };
        let entities = map.entities();
        let Some(target) = quake_core::teleport::resolve_destination(&entities, entity, |_| true)
        else {
            continue;
        };
        teleports.push(Teleport {
            source_index,
            mins: Vec3I32 {
                x: (i32::from(model.mins.x) << 12) + entity.origin.x,
                y: (i32::from(model.mins.y) << 12) + entity.origin.y,
                z: (i32::from(model.mins.z) << 12) + entity.origin.z,
            },
            maxs: Vec3I32 {
                x: (i32::from(model.maxs.x) << 12) + entity.origin.x,
                y: (i32::from(model.maxs.y) << 12) + entity.origin.y,
                z: (i32::from(model.maxs.z) << 12) + entity.origin.z,
            },
            destination: target.origin,
            exit_velocity: target.exit_velocity,
            destination_yaw: target.angles.y as u16,
        });
    }
    teleports
}

/// The guest's `SceneCollision`: the world hull plus every solid translated
/// submodel, keeping the earliest hit.
struct SceneTrace<'a> {
    scene: &'a Scene,
    blocker: std::cell::Cell<Option<usize>>,
}

impl MovementTrace for SceneTrace<'_> {
    fn trace(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut MovementTraceResult,
    ) -> bool {
        let mut best = Trace::default();
        if !self
            .scene
            .world_hull()
            .trace_into(start, end, scratch, &mut best)
        {
            return false;
        }
        for solid in &self.scene.solids {
            let Some(hull) = self.scene.solid_hull(solid) else {
                continue;
            };
            let mut candidate = Trace::default();
            if !hull
                .transformed(BrushTransform::translated(solid.origin))
                .trace_into(start, end, scratch, &mut candidate)
            {
                continue;
            }
            if candidate.fraction < best.fraction
                || (candidate.start_solid && !best.start_solid)
                || (candidate.all_solid && !best.all_solid)
            {
                best = candidate;
                self.blocker.set(Some(solid.source_index));
            }
        }
        *output = MovementTraceResult {
            all_solid: best.all_solid,
            start_solid: best.start_solid,
            fraction: best.fraction,
            end: best.end,
            normal: best.normal,
            blocking_body: None,
        };
        true
    }
}

/// The taper both authored route followers use: steer toward a world point and
/// return the pad's forward/strafe intent for the current view yaw.
fn movement_input(yaw: u16, dx: i32, dy: i32) -> [i16; 2] {
    let yaw = yaw & 0x0fff;
    let cos = psx_math::cos_q12(yaw);
    let sin = psx_math::sin_q12(yaw);
    let forward = mul_q12_i32(cos, dx).saturating_add(mul_q12_i32(sin, dy));
    let strafe = mul_q12_i32(-sin, dx).saturating_add(mul_q12_i32(cos, dy));
    let scale = forward.abs().max(strafe.abs()).max(1);
    let limit = dx.abs().max(dy.abs()).saturating_mul(6).clamp(16, 127);
    [
        (forward.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
        (strafe.saturating_mul(limit) / scale).clamp(-127, 127) as i16,
    ]
}

fn env_num(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn units(value: i32) -> i32 {
    value >> 12
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(map_path) = args.get(1) else {
        eprintln!(
            "usage: quake-routesim <map.psb> [dump|solids|probe|trace|route] ...\n\
             see the module documentation in tools/routesim/src/main.rs"
        );
        std::process::exit(2);
    };
    let mode = args.get(2).map(String::as_str).unwrap_or("route");
    let scene = Scene::load(
        map_path,
        env_num("ROUTESIM_SKILL", 0) as u8,
        env_num("ROUTESIM_RUNES", 0) as u8,
    );

    match mode {
        "dump" => dump_entities(&scene),
        "solids" => dump_solids(&scene),
        "brushes" => dump_brushes(&scene),
        "probe" => probe_slice(&scene, &args),
        "contents" => contents_slice(&scene, &args),
        "floors" => floor_slice(&scene, &args),
        "trace" => trace_segment(&scene, &args),
        "path" => find_path(&scene, &args),
        "reach" => flood_reach(&scene, &args),
        "route" => run_route(&scene, &args),
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}

/// Every brush entity with its authored volume. Buttons, doors, lifts,
/// teleport volumes and `trigger_changelevel` carry no origin, so their
/// waypoint coordinates can only come from the brush model bounds.
fn dump_brushes(scene: &Scene) {
    for (index, entity) in scene.map.entities().iter().enumerate() {
        if entity.model >= 0 {
            continue;
        }
        let model_index = (-entity.model) as usize;
        let Some(model) = scene.map.brush_models().get(model_index) else {
            continue;
        };
        println!(
            "#{index} class={:#04x} flags={} model=*{model_index} target={} targetname={} \
             health={} speed={} wait={} lip={} height={} dmg={} angles=({},{},{}) \
             mins=({},{},{}) maxs=({},{},{}) center=({},{},{})",
            entity.class_name,
            entity.spawn_flags,
            entity.target,
            entity.target_name,
            entity.health,
            entity.speed,
            entity.wait,
            entity.count,
            entity.height,
            entity.damage,
            entity.angles.x,
            entity.angles.y,
            entity.angles.z,
            model.mins.x,
            model.mins.y,
            model.mins.z,
            model.maxs.x,
            model.maxs.y,
            model.maxs.z,
            (model.mins.x + model.maxs.x) / 2,
            (model.mins.y + model.maxs.y) / 2,
            (model.mins.z + model.maxs.z) / 2,
        );
    }
}

fn dump_entities(scene: &Scene) {
    for (index, entity) in scene.map.entities().iter().enumerate() {
        if index == 0 {
            let message = scene
                .map
                .string_at(entity.string)
                .and_then(|bytes| core::str::from_utf8(bytes).ok())
                .unwrap_or("");
            println!("#0 worldspawn message={message:?}");
            continue;
        }
        if entity.class_name == 0 {
            continue;
        }
        println!(
            "#{index} class={:#04x} flags={} model={} target={} killtarget={} targetname={} \
             angles=({},{},{}) speed={} lip={} origin=({},{},{})",
            entity.class_name,
            entity.spawn_flags,
            entity.model,
            entity.target,
            entity.kill_target,
            entity.target_name,
            entity.angles.x,
            entity.angles.y,
            entity.angles.z,
            entity.speed,
            entity.count,
            units(entity.origin.x),
            units(entity.origin.y),
            units(entity.origin.z),
        );
    }
}

fn dump_solids(scene: &Scene) {
    // Every brush entity, solid or not: trigger volumes decide routes too.
    for (index, entity) in scene.map.entities().iter().enumerate() {
        if entity.model >= 0 {
            continue;
        }
        if let Some(model) = scene.map.brush_models().get((-entity.model) as usize) {
            println!(
                "brush #{index} class={:#04x} model=*{} target={} targetname={} \
                 mins=({},{},{}) maxs=({},{},{})",
                entity.class_name,
                -entity.model,
                entity.target,
                entity.target_name,
                model.mins.x,
                model.mins.y,
                model.mins.z,
                model.maxs.x,
                model.maxs.y,
                model.maxs.z,
            );
        }
    }
    for solid in &scene.solids {
        let model = scene
            .map
            .brush_models()
            .get(solid.model_index)
            .expect("solid model");
        println!(
            "#{} class={:#04x} model=*{} mins=({},{},{}) maxs=({},{},{}) origin=({},{},{})",
            solid.source_index,
            solid.class_name,
            solid.model_index,
            model.mins.x,
            model.mins.y,
            model.mins.z,
            model.maxs.x,
            model.maxs.y,
            model.maxs.z,
            units(solid.origin.x),
            units(solid.origin.y),
            units(solid.origin.z),
        );
    }
}

/// `probe x0 x1 y0 y1 z [step]`: `#` world solid, `L`/`S`/`~` lava, slime and
/// water, a letter for a brush entity, `.` free, all in hull 1 at the given
/// height.
fn probe_slice(scene: &Scene, args: &[String]) {
    let value = |index: usize, default: i32| -> i32 {
        args.get(index)
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default)
    };
    let (x0, x1) = (value(3, 0), value(4, 0));
    let (y0, y1) = (value(5, 0), value(6, 0));
    let z = value(7, 0);
    let step = value(8, 16).max(1);
    let mut scratch = TraceScratch::default();
    let mut y = y0;
    while y <= y1 {
        let mut line = String::new();
        let mut x = x0;
        while x <= x1 {
            let point = Vec3I32 {
                x: x << 12,
                y: y << 12,
                z: z << 12,
            };
            line.push(point_glyph(scene, &point, &mut scratch));
            x += step;
        }
        println!("y={y:6} {line}");
        y += step;
    }
}

/// `floors x0 x1 y0 y1 z_top [step]`: the height a player box dropped from
/// `z_top` comes to rest at, one field per cell, `-----` where it never lands.
/// Authoring a fall-damage station needs the drop between two adjacent cells,
/// which no solidity slice at a single height can show.
fn floor_slice(scene: &Scene, args: &[String]) {
    let value = |index: usize, default: i32| -> i32 {
        args.get(index)
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default)
    };
    let (x0, x1) = (value(3, 0), value(4, 0));
    let (y0, y1) = (value(5, 0), value(6, 0));
    let top = value(7, 1_000);
    let step = value(8, 32).max(1);
    let bottom = value(9, -2_000);
    let collision = SceneTrace {
        scene,
        blocker: std::cell::Cell::new(None),
    };
    let mut scratch = TraceScratch::default();
    let mut trace = MovementTraceResult::unobstructed(Vec3I32 { x: 0, y: 0, z: 0 });
    let mut y = y0;
    while y <= y1 {
        let mut line = String::new();
        let mut x = x0;
        while x <= x1 {
            let start = Vec3I32 {
                x: x << 12,
                y: y << 12,
                z: top << 12,
            };
            let end = Vec3I32 {
                x: x << 12,
                y: y << 12,
                z: bottom << 12,
            };
            let landed = collision.trace(&start, &end, &mut scratch, &mut trace)
                && trace.fraction < 4096
                && !trace.all_solid
                && !trace.start_solid;
            // The combined trace takes the nearest hit wholesale, so a brush
            // submodel standing in front of world solid can hand back a rest
            // point that is inside the world. Re-ask at the rest point: a spot
            // the player cannot occupy is not a floor.
            let rest = trace.end;
            let standable = landed
                && collision.trace(&rest, &rest, &mut scratch, &mut trace)
                && !trace.all_solid
                && !trace.start_solid;
            if standable {
                line.push_str(&format!("{:6}", units(rest.z)));
            } else {
                line.push_str("     -");
            }
            x += step;
        }
        println!("y={y:6}{line}");
        y += step;
    }
}

/// `contents x0 x1 y0 y1 z [step]`: the cooked leaf contents at each point,
/// `#` solid, `~` water, `S` slime, `L` lava, `.` empty. Liquids are invisible
/// to the hull-1 `probe` slice, so hazard and swim routes need this one.
fn contents_slice(scene: &Scene, args: &[String]) {
    let value = |index: usize, default: i32| -> i32 {
        args.get(index)
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default)
    };
    let (x0, x1) = (value(3, 0), value(4, 0));
    let (y0, y1) = (value(5, 0), value(6, 0));
    let z = value(7, 0);
    let step = value(8, 16).max(1);
    let leaves = scene.map.leaves();
    let mut y = y0;
    while y <= y1 {
        let mut line = String::new();
        let mut x = x0;
        while x <= x1 {
            let point = Vec3I32 {
                x: x << 12,
                y: y << 12,
                z: z << 12,
            };
            let contents = scene
                .map
                .point_leaf_index(point)
                .and_then(|leaf| leaves.get(leaf))
                .map(|leaf| leaf.contents);
            line.push(match contents {
                Some(-1) => '.',
                Some(-2) => '#',
                Some(-3) => '~',
                Some(-4) => 'S',
                Some(-5) => 'L',
                Some(_) => '?',
                None => '#',
            });
            x += step;
        }
        println!("y={y:6} {line}");
        y += step;
    }
}

fn point_glyph(scene: &Scene, point: &Vec3I32, scratch: &mut TraceScratch) -> char {
    let mut trace = Trace::default();
    if scene
        .world_hull()
        .trace_into(point, point, scratch, &mut trace)
        && (trace.start_solid || trace.all_solid)
    {
        return '#';
    }
    match point_contents(scene, *point) {
        quake_core::collision::CONTENTS_LAVA => return 'L',
        quake_core::collision::CONTENTS_SLIME => return 'S',
        quake_core::collision::CONTENTS_WATER => return '~',
        quake_core::collision::CONTENTS_SKY => return '^',
        _ => {}
    }
    for solid in &scene.solids {
        let Some(hull) = scene.solid_hull(solid) else {
            continue;
        };
        let mut trace = Trace::default();
        if !hull
            .transformed(BrushTransform::translated(solid.origin))
            .trace_into(point, point, scratch, &mut trace)
        {
            continue;
        }
        if trace.start_solid || trace.all_solid {
            return match solid.class_name {
                CLASS_FUNC_BOSSGATE => 'B',
                CLASS_FUNC_EPISODEGATE => 'E',
                CLASS_FUNC_DOOR | CLASS_FUNC_DOOR_SECRET => 'D',
                CLASS_FUNC_WALL => 'W',
                _ => 'X',
            };
        }
    }
    '.'
}

/// `trace sx sy sz ex ey ez` in whole world units.
fn trace_segment(scene: &Scene, args: &[String]) {
    let value = |index: usize| -> i32 {
        args.get(index)
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0)
    };
    let start = Vec3I32 {
        x: value(3) << 12,
        y: value(4) << 12,
        z: value(5) << 12,
    };
    let end = Vec3I32 {
        x: value(6) << 12,
        y: value(7) << 12,
        z: value(8) << 12,
    };
    let mut scratch = TraceScratch::default();
    let mut trace = Trace::default();
    let ok = scene
        .world_hull()
        .trace_into(&start, &end, &mut scratch, &mut trace);
    println!(
        "world ok={ok} fraction={} start_solid={} all_solid={} normal=({},{},{})",
        trace.fraction,
        trace.start_solid,
        trace.all_solid,
        trace.normal.x,
        trace.normal.y,
        trace.normal.z
    );
    let world = scene.map.brush_models().get(0).expect("world brush model");
    let mut render_scratch = RenderTraceScratch::default();
    let mut render_trace = Trace::default();
    let render_ok = trace_render_bsp_into(
        scene.map.planes(),
        scene.map.nodes(),
        scene.map.leaves(),
        world.head_nodes[0],
        &start,
        &end,
        &mut render_scratch,
        &mut render_trace,
    );
    println!(
        "render world ok={render_ok} fraction={} start_solid={} all_solid={} normal=({},{},{})",
        render_trace.fraction,
        render_trace.start_solid,
        render_trace.all_solid,
        render_trace.normal.x,
        render_trace.normal.y,
        render_trace.normal.z
    );
    for solid in &scene.solids {
        let Some(hull) = scene.solid_hull(solid) else {
            continue;
        };
        let mut trace = Trace::default();
        if !hull
            .transformed(BrushTransform::translated(solid.origin))
            .trace_into(&start, &end, &mut scratch, &mut trace)
        {
            continue;
        }
        if trace.fraction < 4096 || trace.start_solid || trace.all_solid {
            println!(
                "  #{} class={:#04x} fraction={} start_solid={} all_solid={} normal=({},{},{})",
                solid.source_index,
                solid.class_name,
                trace.fraction,
                trace.start_solid,
                trace.all_solid,
                trace.normal.x,
                trace.normal.y,
                trace.normal.z
            );
        }
        let Some(model) = scene.map.brush_models().get(solid.model_index) else {
            continue;
        };
        let mut render_trace = Trace::default();
        if !trace_translated_render_bsp_into(
            scene.map.planes(),
            scene.map.nodes(),
            scene.map.leaves(),
            model.head_nodes[0],
            solid.origin,
            &start,
            &end,
            &mut render_scratch,
            &mut render_trace,
        ) {
            continue;
        }
        if render_trace.fraction < 4096 || render_trace.start_solid || render_trace.all_solid {
            println!(
                "  render #{} class={:#04x} fraction={} start_solid={} all_solid={} normal=({},{},{})",
                solid.source_index,
                solid.class_name,
                render_trace.fraction,
                render_trace.start_solid,
                render_trace.all_solid,
                render_trace.normal.x,
                render_trace.normal.y,
                render_trace.normal.z
            );
        }
    }
}

/// Quake's `STEPSIZE`.
const STEP_HEIGHT: i32 = 18;

/// One hull-1 trace against the world plus every solid brush entity whose
/// authored volume overlaps the segment. The overlap reject keeps the
/// pathfinder's millions of traces close to world-hull cost.
fn scene_trace(scene: &Scene, start: Vec3I32, end: Vec3I32, scratch: &mut TraceScratch) -> Trace {
    let mut best = Trace::default();
    if !scene
        .world_hull()
        .trace_into(&start, &end, scratch, &mut best)
    {
        best.all_solid = true;
        best.start_solid = true;
        best.fraction = 0;
        return best;
    }
    let low = Vec3I32 {
        x: start.x.min(end.x),
        y: start.y.min(end.y),
        z: start.z.min(end.z),
    };
    let high = Vec3I32 {
        x: start.x.max(end.x),
        y: start.y.max(end.y),
        z: start.z.max(end.z),
    };
    for solid in &scene.solids {
        let Some(model) = scene.map.brush_models().get(solid.model_index) else {
            continue;
        };
        // Hull 1 grows the brush by the player box; 64 units of slack covers it.
        const SLACK: i32 = 64 << 12;
        let mins = Vec3I32 {
            x: (i32::from(model.mins.x) << 12) + solid.origin.x - SLACK,
            y: (i32::from(model.mins.y) << 12) + solid.origin.y - SLACK,
            z: (i32::from(model.mins.z) << 12) + solid.origin.z - SLACK,
        };
        let maxs = Vec3I32 {
            x: (i32::from(model.maxs.x) << 12) + solid.origin.x + SLACK,
            y: (i32::from(model.maxs.y) << 12) + solid.origin.y + SLACK,
            z: (i32::from(model.maxs.z) << 12) + solid.origin.z + SLACK,
        };
        if high.x < mins.x
            || low.x > maxs.x
            || high.y < mins.y
            || low.y > maxs.y
            || high.z < mins.z
            || low.z > maxs.z
        {
            continue;
        }
        let Some(hull) = scene.solid_hull(solid) else {
            continue;
        };
        let mut candidate = Trace::default();
        if !hull
            .transformed(BrushTransform::translated(solid.origin))
            .trace_into(&start, &end, scratch, &mut candidate)
        {
            continue;
        }
        if candidate.fraction < best.fraction
            || (candidate.start_solid && !best.start_solid)
            || (candidate.all_solid && !best.all_solid)
        {
            best = candidate;
        }
    }
    best
}

fn point_contents(scene: &Scene, point: Vec3I32) -> i16 {
    scene
        .map
        .point_leaf_index(point)
        .and_then(|leaf| scene.map.leaves().get(leaf))
        .map(|leaf| leaf.contents)
        .unwrap_or(quake_core::collision::CONTENTS_SOLID)
}

/// Drop a standing player from `z` and report the floor height it lands on.
fn floor_below(
    scene: &Scene,
    x: i32,
    y: i32,
    z: i32,
    drop: i32,
    scratch: &mut TraceScratch,
) -> Option<i32> {
    let start = Vec3I32 {
        x: x << 12,
        y: y << 12,
        z: z << 12,
    };
    let end = Vec3I32 {
        x: x << 12,
        y: y << 12,
        z: (z - drop) << 12,
    };
    let trace = scene_trace(scene, start, end, scratch);
    if trace.start_solid || trace.all_solid {
        return None;
    }
    if trace.fraction >= quake_core::collision::Q12_ONE {
        return None;
    }
    // A wall the drop grazed is not a floor.
    if trace.normal.z < 1_800 {
        return None;
    }
    Some((trace.end.z >> 12) + 1)
}

/// `path --from x y z [--to x y z]... [--step n] [--drop n] [--radius n] [--liquid]`
///
/// Breadth-first walk over hull-1 free space using the same collision the
/// guest links, so every emitted waypoint is one the real player hull can
/// stand on. Output is the `route` stdin format, so a candidate list can be
/// piped straight back in for validation.
fn find_path(scene: &Scene, args: &[String]) {
    let mut goals: Vec<[i32; 3]> = Vec::new();
    let mut from = [i32::MIN; 3];
    let mut step = 16i32;
    let mut drop = 64i32;
    let mut radius = 24i32;
    let mut allow_liquid = false;
    let mut index = 3usize;
    while index < args.len() {
        let number = |offset: usize| -> i32 {
            args.get(index + offset)
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(0)
        };
        match args[index].as_str() {
            "--from" => {
                from = [number(1), number(2), number(3)];
                index += 4;
            }
            "--to" => {
                goals.push([number(1), number(2), number(3)]);
                index += 4;
            }
            "--step" => {
                step = number(1).max(4);
                index += 2;
            }
            "--drop" => {
                drop = number(1).max(1);
                index += 2;
            }
            "--radius" => {
                radius = number(1).max(1);
                index += 2;
            }
            "--liquid" => {
                allow_liquid = true;
                index += 1;
            }
            other => {
                eprintln!("unknown path argument {other}");
                std::process::exit(2);
            }
        }
    }
    if from[0] == i32::MIN {
        let start = scene
            .map
            .entities()
            .get(1)
            .expect("cooked info_player_start");
        from = [
            units(start.origin.x),
            units(start.origin.y),
            units(start.origin.z),
        ];
    }
    if goals.is_empty() {
        eprintln!("path needs at least one --to");
        std::process::exit(2);
    }

    let mut scratch = TraceScratch::default();
    let mut cursor = from;
    // Waypoints in emission order, each carrying the teleporter that has to
    // fire before the NEXT one is reachable.
    let mut emitted: Vec<([i32; 3], Option<usize>)> = Vec::new();
    let mut grid_nodes = 0usize;
    let simplify_paths = std::env::var("ROUTESIM_NO_SIMPLIFY").is_err();
    for goal in goals {
        let leg = walk_to(
            scene,
            cursor,
            goal,
            step,
            drop,
            radius,
            allow_liquid,
            &mut scratch,
        );
        let Some(leg) = leg else {
            println!(
                "PATH STUCK from ({},{},{}) to ({},{},{})",
                cursor[0], cursor[1], cursor[2], goal[0], goal[1], goal[2]
            );
            std::process::exit(1);
        };
        let nodes: Vec<[i32; 3]> = leg.iter().skip(1).copied().collect();
        grid_nodes += nodes.len();
        let teleports = leg_teleports(&nodes, step);
        for step_kind in leg_steps(&teleports) {
            match step_kind {
                LegStep::Walk { start, end } => {
                    // Each leg is simplified on its own so an intermediate
                    // goal, which is the whole point of ordering the legs, is
                    // never merged away.
                    let simplified = if simplify_paths {
                        simplify(scene, cursor, &nodes[start..end], drop, &mut scratch)
                    } else {
                        nodes[start..end].to_vec()
                    };
                    for point in simplified {
                        emitted.push((point, None));
                    }
                    cursor = nodes[end - 1];
                }
                LegStep::Teleport {
                    node,
                    source_index: arrival,
                } => {
                    if let Some(last) = emitted.last_mut() {
                        last.1 = Some(arrival);
                    }
                    emitted.push((nodes[node], None));
                    cursor = nodes[node];
                }
            }
        }
    }
    println!("# path from ({},{},{})", from[0], from[1], from[2]);
    let emit_height = std::env::var("ROUTESIM_PATH_Z").is_ok();
    for (point, teleport) in &emitted {
        if emit_height {
            println!("{} {} {} {}", point[0], point[1], point[2], radius);
        } else {
            println!("{} {} - {}", point[0], point[1], radius);
        }
        if let Some(source_index) = teleport {
            println!("# teleporter #{source_index} carries the route from here");
        }
    }
    println!(
        "# {grid_nodes} grid nodes reduced to {} waypoints",
        emitted.len()
    );
}

/// `reach --from x y z [--step n] [--drop n] [--liquid]`: flood the walkable
/// set from a point and print it as an ASCII map with the floor height under
/// every cell. This is how a map's real reachable region is found before any
/// waypoint is written.
fn flood_reach(scene: &Scene, args: &[String]) {
    let mut from = [i32::MIN; 3];
    let mut step = 32i32;
    let mut drop = 64i32;
    let mut allow_liquid = false;
    let mut index = 3usize;
    while index < args.len() {
        let number = |offset: usize| -> i32 {
            args.get(index + offset)
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(0)
        };
        match args[index].as_str() {
            "--from" => {
                from = [number(1), number(2), number(3)];
                index += 4;
            }
            "--step" => {
                step = number(1).max(4);
                index += 2;
            }
            "--drop" => {
                drop = number(1).max(1);
                index += 2;
            }
            "--liquid" => {
                allow_liquid = true;
                index += 1;
            }
            other => {
                eprintln!("unknown reach argument {other}");
                std::process::exit(2);
            }
        }
    }
    if from[0] == i32::MIN {
        let start = scene
            .map
            .entities()
            .get(1)
            .expect("cooked info_player_start");
        from = [
            units(start.origin.x),
            units(start.origin.y),
            units(start.origin.z),
        ];
    }
    let mut scratch = TraceScratch::default();
    let visited = walk_to(
        scene,
        from,
        [i32::MAX / 4, i32::MAX / 4, i32::MAX / 4],
        step,
        drop,
        1,
        allow_liquid,
        &mut scratch,
    );
    let cells = LAST_VISITED.with(|cells| cells.borrow().clone());
    let _ = visited;
    if cells.is_empty() {
        println!("no reachable cells");
        return;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for point in &cells {
        x0 = x0.min(point[0]);
        x1 = x1.max(point[0]);
        y0 = y0.min(point[1]);
        y1 = y1.max(point[1]);
    }
    println!(
        "reachable cells={} x=[{x0},{x1}] y=[{y0},{y1}] step={step}",
        cells.len()
    );
    if std::env::var("ROUTESIM_VERBOSE").is_ok() {
        let mut exact = cells.clone();
        exact.sort_unstable_by_key(|point| (point[2], point[1], point[0]));
        for [x, y, z] in exact {
            println!("cell {x} {y} {z}");
        }
    }
    let mut y = y0;
    while y <= y1 {
        let mut line = String::new();
        let mut x = x0;
        while x <= x1 {
            let height = cells
                .iter()
                .filter(|point| point[0] == x && point[1] == y)
                .map(|point| point[2])
                .max();
            line.push(match height {
                None => ' ',
                Some(z) => {
                    let bucket = (z.div_euclid(64)).rem_euclid(36);
                    b"0123456789abcdefghijklmnopqrstuvwxyz"[bucket as usize] as char
                }
            });
            x += step;
        }
        println!("y={y:6} {line}");
        y += step;
    }
}

thread_local! {
    static LAST_VISITED: std::cell::RefCell<Vec<[i32; 3]>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Nodes the flood ARRIVED at through a teleporter, keyed the same way the
    /// search keys its parents, valued with the cooked teleporter index. The
    /// path emitter splits its legs here so a waypoint list never merges a
    /// straight walk across a teleport hop.
    static LAST_TELEPORTED: std::cell::RefCell<std::collections::HashMap<(i32, i32, i32), usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[allow(clippy::too_many_arguments)]
fn walk_to(
    scene: &Scene,
    from: [i32; 3],
    goal: [i32; 3],
    step: i32,
    drop: i32,
    radius: i32,
    allow_liquid: bool,
    scratch: &mut TraceScratch,
) -> Option<Vec<[i32; 3]>> {
    use std::collections::{HashMap, VecDeque};

    LAST_VISITED.with(|cells| cells.borrow_mut().clear());
    reset_teleport_marks();
    let key = |point: [i32; 3]| -> (i32, i32, i32) {
        (
            point[0].div_euclid(step),
            point[1].div_euclid(step),
            point[2].div_euclid(16),
        )
    };
    let start_floor = floor_below(
        scene,
        from[0],
        from[1],
        from[2] + STEP_HEIGHT,
        drop + 64,
        scratch,
    )
    .unwrap_or(from[2]);
    let start = [from[0], from[1], start_floor];
    let mut parents: HashMap<(i32, i32, i32), Option<[i32; 3]>> = HashMap::new();
    let mut queue = VecDeque::new();
    parents.insert(key(start), None);
    queue.push_back(start);
    let mut best: Option<[i32; 3]> = None;
    while let Some(current) = queue.pop_front() {
        if (current[0] - goal[0]).abs() <= radius
            && (current[1] - goal[1]).abs() <= radius
            && (current[2] - goal[2]).abs() <= 56
        {
            best = Some(current);
            break;
        }
        if parents.len() > 400_000 {
            break;
        }
        for (dx, dy) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let next_x = current[0] + dx * step;
            let next_y = current[1] + dy * step;
            let stand = Vec3I32 {
                x: current[0] << 12,
                y: current[1] << 12,
                z: current[2] << 12,
            };
            let lifted = Vec3I32 {
                z: (current[2] + STEP_HEIGHT) << 12,
                ..stand
            };
            let up = scene_trace(scene, stand, lifted, scratch);
            if up.start_solid || up.all_solid {
                continue;
            }
            let head = up.end;
            let across = scene_trace(
                scene,
                head,
                Vec3I32 {
                    x: next_x << 12,
                    y: next_y << 12,
                    z: head.z,
                },
                scratch,
            );
            // A teleporter fires the moment the player box touches it, so the
            // step is tested where the hull actually STOPPED, not where it was
            // aimed. That matters because the flood cannot climb a ramp taller
            // than one step, and Start's slipgates sit at the top of one: the
            // step short of the volume still reaches into it, exactly as the
            // real walk-in does.
            if let Some(teleport) = scene.teleport_at(across.end) {
                let arrival = [
                    units(teleport.destination.x),
                    units(teleport.destination.y),
                    units(teleport.destination.z),
                ];
                let landed = floor_below(
                    scene,
                    arrival[0],
                    arrival[1],
                    arrival[2] + STEP_HEIGHT,
                    drop + 128,
                    scratch,
                )
                .unwrap_or(arrival[2]);
                let next = [arrival[0], arrival[1], landed];
                if parents.contains_key(&key(next)) {
                    continue;
                }
                if std::env::var("ROUTESIM_VERBOSE").is_ok() {
                    eprintln!(
                        "flood entered teleporter #{} -> ({},{},{})",
                        teleport.source_index, next[0], next[1], next[2]
                    );
                }
                parents.insert(key(next), Some(current));
                mark_teleport(key(next), teleport.source_index);
                LAST_VISITED.with(|cells| cells.borrow_mut().push(next));
                queue.push_back(next);
                continue;
            }
            if across.fraction < quake_core::collision::Q12_ONE || across.start_solid {
                continue;
            }
            let Some(floor) = floor_below(
                scene,
                next_x,
                next_y,
                head.z >> 12,
                STEP_HEIGHT + drop,
                scratch,
            ) else {
                continue;
            };
            let contents = point_contents(
                scene,
                Vec3I32 {
                    x: next_x << 12,
                    y: next_y << 12,
                    z: (floor + 2) << 12,
                },
            );
            if contents == quake_core::collision::CONTENTS_LAVA
                || contents == quake_core::collision::CONTENTS_SLIME
                || (!allow_liquid && contents == quake_core::collision::CONTENTS_WATER)
            {
                continue;
            }
            let next = [next_x, next_y, floor];
            if parents.contains_key(&key(next)) {
                continue;
            }
            parents.insert(key(next), Some(current));
            LAST_VISITED.with(|cells| cells.borrow_mut().push(next));
            queue.push_back(next);
        }
    }

    if std::env::var("ROUTESIM_VERBOSE").is_ok() {
        eprintln!(
            "bfs nodes={} goal=({},{},{}) best={:?}",
            parents.len(),
            goal[0],
            goal[1],
            goal[2],
            best
        );
    }
    let mut node = best?;
    let mut path = vec![node];
    while let Some(Some(parent)) = parents.get(&key(node)).copied() {
        path.push(parent);
        node = parent;
    }
    path.reverse();
    Some(path)
}

/// Drop every teleport mark. Called at the top of each `walk_to` flood: a
/// multi-goal `path` walks one leg per goal through that same function, so an
/// arrival left over from an earlier leg would otherwise still answer
/// [`leg_teleports`] and cut a later leg at a node it never teleported into.
fn reset_teleport_marks() {
    LAST_TELEPORTED.with(|cells| cells.borrow_mut().clear());
}

/// Record that this flood arrived at `key` through cooked teleporter
/// `source_index`.
fn mark_teleport(key: (i32, i32, i32), source_index: usize) {
    LAST_TELEPORTED.with(|cells| {
        cells.borrow_mut().insert(key, source_index);
    });
}

/// For every node on a leg, the teleporter the flood arrived through, if any.
fn leg_teleports(nodes: &[[i32; 3]], step: i32) -> Vec<Option<usize>> {
    LAST_TELEPORTED.with(|cells| {
        let cells = cells.borrow();
        nodes
            .iter()
            .map(|point| {
                cells
                    .get(&(
                        point[0].div_euclid(step),
                        point[1].div_euclid(step),
                        point[2].div_euclid(16),
                    ))
                    .copied()
            })
            .collect()
    })
}

/// One emission step over a leg's nodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LegStep {
    /// `nodes[start..end]` is a plain walk and may be simplified as one piece.
    Walk { start: usize, end: usize },
    /// `nodes[node]` is where the flood came OUT of a teleporter, so the walk
    /// stops short of it and a fresh waypoint starts there.
    Teleport { node: usize, source_index: usize },
}

/// Cut a leg into walks and teleport hops.
///
/// A teleport hop is not a walk, so the leg is cut either side of one: the
/// walk up to the volume is simplified on its own and the arrival starts a
/// fresh segment. Without the cut the greedy visibility merge would happily
/// draw a straight line across the whole map.
fn leg_steps(teleports: &[Option<usize>]) -> Vec<LegStep> {
    let mut steps = Vec::new();
    let mut segment_start = 0usize;
    for index in 0..teleports.len() {
        let arrival = teleports[index];
        if arrival.is_none() && index + 1 != teleports.len() {
            continue;
        }
        let end = if arrival.is_some() { index } else { index + 1 };
        if end > segment_start {
            steps.push(LegStep::Walk {
                start: segment_start,
                end,
            });
        }
        if let Some(source_index) = arrival {
            steps.push(LegStep::Teleport {
                node: index,
                source_index,
            });
        }
        segment_start = index + 1;
    }
    steps
}

/// Greedy visibility merge. Two nodes collapse when the walking segment
/// between them is clear at step height and every sample along it still has a
/// floor under it, so a straight authored waypoint cannot walk off a ledge the
/// grid path went around.
fn simplify(
    scene: &Scene,
    from: [i32; 3],
    path: &[[i32; 3]],
    drop: i32,
    scratch: &mut TraceScratch,
) -> Vec<[i32; 3]> {
    let mut output = Vec::new();
    let mut current = from;
    let mut index = 0usize;
    while index < path.len() {
        let mut chosen = index;
        for candidate in (index..path.len()).rev() {
            if walkable_segment(scene, current, path[candidate], drop, scratch) {
                chosen = candidate;
                break;
            }
        }
        output.push(path[chosen]);
        current = path[chosen];
        index = chosen + 1;
    }
    output
}

fn walkable_segment(
    scene: &Scene,
    from: [i32; 3],
    to: [i32; 3],
    drop: i32,
    scratch: &mut TraceScratch,
) -> bool {
    let height = from[2].max(to[2]) + STEP_HEIGHT;
    let start = Vec3I32 {
        x: from[0] << 12,
        y: from[1] << 12,
        z: height << 12,
    };
    let end = Vec3I32 {
        x: to[0] << 12,
        y: to[1] << 12,
        z: height << 12,
    };
    let trace = scene_trace(scene, start, end, scratch);
    if trace.fraction < quake_core::collision::Q12_ONE || trace.start_solid || trace.all_solid {
        return false;
    }
    let span = (to[0] - from[0]).abs().max((to[1] - from[1]).abs());
    let samples = (span / 8).clamp(1, 96);
    let mut previous = from[2];
    for sample in 1..=samples {
        let x = from[0] + (to[0] - from[0]) * sample / samples;
        let y = from[1] + (to[1] - from[1]) * sample / samples;
        let Some(floor) = floor_below(scene, x, y, height, STEP_HEIGHT + drop, scratch) else {
            return false;
        };
        if (floor - previous).abs() > STEP_HEIGHT + 8 {
            return false;
        }
        previous = floor;
    }
    true
}

struct Waypoint {
    x: i32,
    y: i32,
    z: Option<i32>,
    radius: i32,
    jump: bool,
}

/// `route [--from x y z] [--vel x y z] [--yaw n] [--frames n]`, waypoints on
/// stdin as `x y [z|-] [radius] [jump]`. The final marker holds jump while
/// approaching that waypoint; the movement motor's latch still permits only
/// one launch until a non-jump waypoint releases it.
fn run_route(scene: &Scene, args: &[String]) {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).expect("waypoints on stdin");
    let mut route = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            eprintln!("waypoint needs at least x and y: {line}");
            std::process::exit(2);
        }
        route.push(Waypoint {
            x: parts[0].parse().expect("waypoint x"),
            y: parts[1].parse().expect("waypoint y"),
            z: parts
                .get(2)
                .filter(|raw| **raw != "-")
                .map(|raw| raw.parse().expect("waypoint z")),
            radius: parts
                .get(3)
                .map(|raw| raw.parse().expect("waypoint radius"))
                .unwrap_or(20),
            jump: parts
                .get(4)
                .is_some_and(|raw| *raw == "jump" || *raw == "1"),
        });
    }
    if route.is_empty() {
        eprintln!("no waypoints on stdin");
        std::process::exit(2);
    }

    let start = scene
        .map
        .entities()
        .get(1)
        .expect("cooked info_player_start");
    let mut origin = start.origin;
    let mut velocity = Vec3I32 { x: 0, y: 0, z: 0 };
    let mut yaw = start.angles.y as u16;
    let mut max_frames = 1_200u32;
    let mut index = 3usize;
    while index < args.len() {
        let number = |offset: usize| -> i32 {
            args.get(index + offset)
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(0)
        };
        match args[index].as_str() {
            "--from" => {
                origin = Vec3I32 {
                    x: number(1) << 12,
                    y: number(2) << 12,
                    z: number(3) << 12,
                };
                index += 4;
            }
            "--vel" => {
                velocity = Vec3I32 {
                    x: number(1) << 12,
                    y: number(2) << 12,
                    z: number(3) << 12,
                };
                index += 4;
            }
            "--yaw" => {
                yaw = number(1) as u16;
                index += 2;
            }
            "--frames" => {
                max_frames = number(1).max(1) as u32;
                index += 2;
            }
            other => {
                eprintln!("unknown route argument {other}");
                std::process::exit(2);
            }
        }
    }

    let mut state = MovementState::new(origin);
    state.teleport_with_velocity(origin, velocity);
    let mut scratch = MovementScratch::default();
    let collision = SceneTrace {
        scene,
        blocker: std::cell::Cell::new(None),
    };
    let leaves = scene.map.leaves();
    let map = &scene.map;
    let ticks = env_num("ROUTESIM_TICKS", 4).clamp(1, 4) as u16;
    let verbose = std::env::var("ROUTESIM_VERBOSE").is_ok();

    let mut waypoint = 0usize;
    let mut frame = 0u32;
    let mut last = String::new();
    while waypoint < route.len() && frame < max_frames {
        frame += 1;
        // `teleport_touch` runs before the route reads the position, exactly
        // like the guest's own touch pass, so a waypoint inside a teleport
        // volume is retired at the destination and not at the pad.
        if let Some(teleport) = scene.teleport_at(state.origin()) {
            let destination = teleport.destination;
            let velocity = teleport.exit_velocity;
            yaw = teleport.destination_yaw;
            println!(
                "teleporter #{} at frame {frame} -> ({},{},{})",
                teleport.source_index,
                units(destination.x),
                units(destination.y),
                units(destination.z),
            );
            state.teleport_with_velocity(destination, velocity);
        }
        let origin = state.origin();
        let (x, y, z) = (units(origin.x), units(origin.y), units(origin.z));
        let target = &route[waypoint];
        let dx = target.x - x;
        let dy = target.y - y;
        let wrong_height = target.z.is_some_and(|height| (height - z).abs() > 8);
        if dx.abs() <= target.radius && dy.abs() <= target.radius && !wrong_height {
            println!(
                "waypoint {waypoint} ({},{}) reached at frame {frame} ({x},{y},{z})",
                target.x, target.y
            );
            waypoint += 1;
            continue;
        }
        collision.blocker.set(None);
        let movement = movement_input(yaw, dx, dy);
        state.update_ticks_with_gravity(
            &collision,
            &mut scratch,
            MovementInput {
                forward: movement[0],
                strafe: movement[1],
                yaw: yaw & 0x0fff,
                pitch: 0,
                jump: target.jump,
            },
            ticks,
            env_num("ROUTESIM_GRAVITY", 800).clamp(0, u16::MAX as i32) as u16,
            |point| {
                let leaf = map.point_leaf_index(*point)?;
                Some(leaves.get(leaf)?.contents)
            },
        );
        last = format!(
            "frame {frame} waypoint {waypoint} at ({x},{y},{z}) target ({},{}) blocker {:?}",
            target.x,
            target.y,
            collision.blocker.get()
        );
        if verbose {
            println!("  {last}");
        }
    }

    let origin = state.origin();
    if waypoint == route.len() {
        println!(
            "ROUTE COMPLETE in {frame} frames at ({},{},{})",
            units(origin.x),
            units(origin.y),
            units(origin.z)
        );
    } else {
        println!("ROUTE STUCK: {last}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this covers: `walk_to` used to clear `LAST_VISITED` but not
    /// `LAST_TELEPORTED`, so a multi-goal `path` carried leg one's arrivals
    /// into leg two and cut it at a node it had walked to on foot.
    #[test]
    fn a_teleport_mark_does_not_survive_into_the_next_leg() {
        let step = 32i32;
        let arrival = [64i32, 128, 16];
        let key = (
            arrival[0].div_euclid(step),
            arrival[1].div_euclid(step),
            arrival[2].div_euclid(16),
        );

        reset_teleport_marks();
        mark_teleport(key, 9);
        assert_eq!(leg_teleports(&[arrival], step), vec![Some(9)]);

        // What the next leg's flood does before it walks anywhere.
        reset_teleport_marks();
        assert_eq!(leg_teleports(&[arrival], step), vec![None]);
    }

    #[test]
    fn a_leg_with_no_teleport_stays_one_walk() {
        assert_eq!(
            leg_steps(&[None, None, None]),
            vec![LegStep::Walk { start: 0, end: 3 }]
        );
        assert!(leg_steps(&[]).is_empty());
    }

    /// The walk stops at the node BEFORE the arrival and the arrival opens a
    /// fresh segment, so `simplify` never sees the two sides of a hop in one
    /// slice and cannot merge a straight line across the discontinuity.
    #[test]
    fn a_teleport_cuts_the_leg_either_side_of_the_hop() {
        let steps = leg_steps(&[None, None, Some(4), None, None]);
        assert_eq!(
            steps,
            vec![
                LegStep::Walk { start: 0, end: 2 },
                LegStep::Teleport {
                    node: 2,
                    source_index: 4,
                },
                LegStep::Walk { start: 3, end: 5 },
            ]
        );
        // No walk slice spans the hop.
        for step in &steps {
            if let LegStep::Walk { start, end } = step {
                assert!(
                    *end <= 2 || *start >= 3,
                    "walk {start}..{end} crosses the hop"
                );
            }
        }
    }

    #[test]
    fn an_arrival_on_the_first_node_emits_no_leading_walk() {
        assert_eq!(
            leg_steps(&[Some(1), None]),
            vec![
                LegStep::Teleport {
                    node: 0,
                    source_index: 1,
                },
                LegStep::Walk { start: 1, end: 2 },
            ]
        );
    }

    #[test]
    fn an_arrival_on_the_last_node_ends_the_leg_on_the_hop() {
        assert_eq!(
            leg_steps(&[None, Some(2)]),
            vec![
                LegStep::Walk { start: 0, end: 1 },
                LegStep::Teleport {
                    node: 1,
                    source_index: 2,
                },
            ]
        );
    }

    #[test]
    fn back_to_back_teleports_each_get_their_own_hop() {
        assert_eq!(
            leg_steps(&[Some(1), Some(2)]),
            vec![
                LegStep::Teleport {
                    node: 0,
                    source_index: 1,
                },
                LegStep::Teleport {
                    node: 1,
                    source_index: 2,
                },
            ]
        );
    }
}
