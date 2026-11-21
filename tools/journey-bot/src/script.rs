//! The Episode 1 journey as a sequence of phases.

use std::collections::HashSet;

use emulator_core::input_tape::PadSample;

use crate::fight::Fighter;
use crate::obs::Obs;
use crate::pad::{self, button, Command};
use crate::walk::*;
use crate::world::World;

pub trait Script {
    /// Choose the pad for the frame about to be polled. `None` ends the run.
    fn step(&mut self, o: &Obs) -> Option<PadSample>;
    fn status(&self) -> String;
}

/// A map segment: the route to walk and the map the exit leads to.
struct Segment {
    map: u32,
    route: &'static str,
    next_map: u32,
}

const SEGMENTS: &[Segment] = &[
    Segment { map: 1, route: include_str!("../routes/e1m1.txt"), next_map: 2 },
    Segment { map: 2, route: include_str!("../routes/e1m2.txt"), next_map: 3 },
    Segment { map: 3, route: include_str!("../routes/e1m3.txt"), next_map: 4 },
    Segment { map: 4, route: include_str!("../routes/e1m4.txt"), next_map: 5 },
    Segment { map: 5, route: include_str!("../routes/e1m5.txt"), next_map: 6 },
    Segment { map: 6, route: include_str!("../routes/e1m6.txt"), next_map: 7 },
    // The end of the episode: the exit slipgate loads `start` after the finale text.
    Segment { map: 7, route: include_str!("../routes/e1m7.txt"), next_map: 0 },
    // The secret level (reached from E1M4's secret exit) leads back to E1M5, as in the original.
    Segment { map: 8, route: include_str!("../routes/e1m8.txt"), next_map: 5 },
];

enum Phase {
    Menu { pressed: u32 },
    ToSkill(Walker),
    ToPortal(Walker),
    Map { seg: usize, walker: Walker },
    /// Title menu: LOAD GAME, slot 1, from the memory card the run was started with.
    LoadMenu { frames: u32, opened: u32, map: u32 },
    /// Pause menu: SAVE GAME, slot 1, the first moments of the map the run saves at.
    SaveSeq { frames: u32 },
    /// Idle for a while on the next map before the run ends (`--settle`), so the closing
    /// checkpoint of a per-map journey is taken on a drawn frame, not the first frame of a load.
    Settle { left: u32 },
    /// The map the card loaded has just appeared: idle a few frames (`--load-settle`) so the
    /// opening checkpoint is a drawn frame, then start the route.
    LoadSettle { left: u32, seg: usize },
    Done,
}

pub struct Journey {
    phase: Phase,
    frame: u32,
    maps_dir: String,
    pub routesim: String,
    /// Attempt number: varies the dodge rhythm and delays the first step on each map.
    pub seed: u32,
    world: Option<(u32, World)>,
    pub fighter: Fighter,
    latched: HashSet<u16>,
    start_open: HashSet<u16>,
    restock_after: u32,
    /// Route text of the phases still to come on this map (the route file is cut at `@checkpoint`
    /// lines), the number of checkpoints passed, and a checkpoint reached this frame.
    cp_phases: std::collections::VecDeque<String>,
    pub cp_index: u32,
    pub pending_checkpoint: Option<u32>,
    last_map: u32,
    skip_tick: u32,
    foraged: HashSet<(u32, u16)>,
    progress: (u32, usize, u32),
    pub last: String,
    pub failure: Option<String>,
    pub segment_ends: Vec<(u32, u32)>,
    /// Save slot 1 through the pause menu when this map starts, then stop (`card_saved`).
    pub save_at: Option<u32>,
    pub card_saved: bool,
    /// Stop when the first map's exit leads to the next (a per-map journey).
    pub single: bool,
    pub settle: u32,
    pub load_settle: u32,
    /// The map the first segment's exit leads to, when not its usual one (`--exit-to`, E1M4's secret exit to E1M8).
    pub exit_to: Option<u32>,
}

const TO_EASY: &str = "544 544\n336 768\n272 1200\n256 1328\n232 1352 r=28\n";
const TO_PORTAL: &str = "352 1680\n304 1696\n176 1696\n-48 1648\n-64 1628\n";

const MAP_NAMES: [&str; 9] = ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"];

fn planner_for(routesim: &str, maps_dir: &str, map: u32) -> Option<Planner> {
    if routesim.is_empty() {
        return None;
    }
    Some(Planner { last_dz: Default::default(), drop: Default::default(), top: Default::default(), routesim: routesim.to_string(), psb: format!("{maps_dir}/{}.psb", MAP_NAMES[map as usize]) })
}

/// The route text of segment `seg`: the file named by `QUAKE_BOT_ROUTE` when set (route authoring), else the built-in one.
fn route_text(seg: usize) -> String {
    match std::env::var("QUAKE_BOT_ROUTE") {
        Ok(p) if !p.is_empty() => std::fs::read_to_string(&p).expect("QUAKE_BOT_ROUTE"),
        _ => SEGMENTS[seg].route.to_string(),
    }
}

/// The walker for the first phase of segment `seg`; the later phases wait in `cp_phases`.
fn segment_walker(
    routesim: &str,
    maps_dir: &str,
    seg: usize,
    cp_phases: &mut std::collections::VecDeque<String>,
    cp_index: &mut u32,
) -> Walker {
    let mut phases: std::collections::VecDeque<String> = route_phases(&route_text(seg)).into();
    let first = phases.pop_front().unwrap_or_default();
    *cp_phases = phases;
    *cp_index = 0;
    let mut walker = Walker::new(parse_route(&first));
    walker.planner = planner_for(routesim, maps_dir, SEGMENTS[seg].map);
    walker
}

/// The route text cut at `@checkpoint` lines.
fn route_phases(text: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for line in text.lines() {
        if line.trim() == "@checkpoint" {
            out.push(String::new());
        } else {
            let last = out.last_mut().unwrap();
            last.push_str(line);
            last.push('\n');
        }
    }
    out
}

/// Cooked indices of the doors authored with spawnflag 1 (start open), read from the cooked map by
/// the route simulator's `brushes` listing.
fn start_open_doors(routesim: &str, maps_dir: &str, map: u32) -> HashSet<u16> {
    let mut set = HashSet::new();
    if routesim.is_empty() || map as usize >= MAP_NAMES.len() {
        return set;
    }
    let psb = format!("{maps_dir}/{}.psb", MAP_NAMES[map as usize]);
    let Ok(out) = std::process::Command::new(routesim).args([&psb, "brushes"]).output() else {
        return set;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(rest) = line.strip_prefix('#') else { continue };
        let mut fields = rest.split_whitespace();
        let index = fields.next().and_then(|v| v.parse::<u16>().ok());
        let class = fields.next().and_then(|f| f.strip_prefix("class=0x"));
        let flags = fields.next().and_then(|f| f.strip_prefix("flags=")).and_then(|v| v.parse::<u32>().ok());
        if let (Some(index), Some("0c"), Some(flags)) = (index, class, flags) {
            if flags & 1 != 0 {
                set.insert(index);
            }
        }
    }
    set
}

impl Journey {
    /// The settle phase is on its last idle frame.
    pub fn settle_done(&self) -> bool {
        matches!(self.phase, Phase::Settle { left: 1 } | Phase::LoadSettle { left: 1, .. })
    }

    /// Text of what a checkpoint file must carry besides the machine: the phase, the movers seen
    /// open, the frame counter.
    pub fn state_text(&self) -> String {
        let mut latched: Vec<_> = self.latched.iter().copied().collect();
        latched.sort_unstable();
        format!(
            "cp={}\nframe={}\nrestock_after={}\nlatched={}\n",
            self.cp_index,
            self.frame,
            self.restock_after,
            latched.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
        )
    }

    /// Continue the map `map` at the checkpoint a `state_text` describes.
    pub fn restore_state(&mut self, text: &str, map: u32) {
        let get = |k: &str| -> String {
            text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").to_string()
        };
        let seg = SEGMENTS.iter().position(|s| s.map == map).expect("route");
        let cp: usize = get("cp").parse().unwrap_or(0);
        let mut phases: std::collections::VecDeque<String> = route_phases(&route_text(seg)).into();
        for _ in 0..cp {
            phases.pop_front();
        }
        let first = phases.pop_front().unwrap_or_default();
        self.cp_phases = phases;
        self.cp_index = cp as u32;
        let mut walker = Walker::new(parse_route(&first));
        walker.planner = planner_for(&self.routesim, &self.maps_dir, map);
        self.phase = Phase::Map { seg, walker };
        self.frame = get("frame").parse().unwrap_or(0);
        self.restock_after = get("restock_after").parse().unwrap_or(0);
        self.latched = get("latched").split(',').filter_map(|v| v.parse().ok()).collect();
        self.last_map = map;
        self.start_open = start_open_doors(&self.routesim, &self.maps_dir, map);
        self.progress = (self.frame, 0, map);
    }

    /// Continue on map `map` as if the journey had just arrived there.
    pub fn resumed(maps_dir: &str, map: u32) -> Self {
        let mut j = Self::new(maps_dir);
        let seg = SEGMENTS.iter().position(|s| s.map == map).expect("no route for that map");
        j.last_map = map;
        let walker = segment_walker("", maps_dir, seg, &mut j.cp_phases, &mut j.cp_index);
        j.phase = Phase::Map { seg, walker };
        j
    }

    pub fn new(maps_dir: &str) -> Self {
        Self {
            phase: Phase::Menu { pressed: 0 },
            frame: 0,
            maps_dir: maps_dir.to_string(),
            routesim: String::new(),
            seed: 0,
            world: None,
            fighter: Fighter::new(),
            latched: HashSet::new(),
            start_open: HashSet::new(),
            restock_after: 0,
            cp_phases: Default::default(),
            cp_index: 0,
            pending_checkpoint: None,
            last_map: 0,
            skip_tick: 0,
            foraged: HashSet::new(),
            progress: (0, 0, 0),
            last: String::new(),
            failure: None,
            segment_ends: Vec::new(),
            save_at: None,
            card_saved: false,
            single: false,
            settle: 0,
            load_settle: 0,
            exit_to: None,
        }
    }

    /// Replace the route of the map being played (a resumed run that continues from the middle).
    pub fn set_route(&mut self, text: &str) {
        if let Phase::Map { walker, .. } = &mut self.phase {
            *walker = Walker::new(parse_route(text));
        }
    }

    /// Start from a memory card: load slot 1 from the title menu, then play map `map`.
    pub fn loading(maps_dir: &str, map: u32) -> Self {
        let mut j = Self::new(maps_dir);
        assert!(SEGMENTS.iter().any(|s| s.map == map), "no route for map {map}");
        j.phase = Phase::LoadMenu { frames: 0, opened: 0, map };
        j
    }

    fn ensure_world(&mut self, map: u32) {
        if self.world.as_ref().map_or(true, |(m, _)| *m != map) {
            let path = format!("{}/{}.psb", self.maps_dir, MAP_NAMES[map as usize]);
            self.world = Some((map, World::load(&path)));
        }
    }
}

impl Script for Journey {
    fn status(&self) -> String {
        match &self.phase {
            Phase::Menu { .. } => "menu".into(),
            Phase::ToSkill(w) => format!("to-skill wp{}/{}", w.next, w.route.len()),
            Phase::ToPortal(w) => format!("to-portal wp{}/{}", w.next, w.route.len()),
            Phase::Map { seg, walker } => format!(
                "e1m{} wp{}/{}{} fight={:?} latched={} replans={} {}",
                SEGMENTS[*seg].map,
                walker.next,
                walker.route.len(),
                if walker.waiting { " waiting" } else { "" },
                self.fighter.target,
                { let mut v: Vec<_> = self.latched.iter().copied().collect(); v.sort(); format!("{v:?}") },
                walker.replans,
                walker.plan_note.clone().unwrap_or_default()
            ),
            Phase::LoadMenu { frames, .. } => format!("load-menu {frames}"),
            Phase::SaveSeq { frames } => format!("save-seq {frames}"),
            Phase::Settle { left } => format!("settle {left}"),
            Phase::LoadSettle { left, .. } => format!("load-settle {left}"),
            Phase::Done => "done".into(),
        }
    }

    fn step(&mut self, o: &Obs) -> Option<PadSample> {
        self.frame += 1;
        if o.map != self.last_map || self.frame == 1 {
            self.latched.clear();
            self.last_map = o.map;
            // A door authored with the start-open flag rests open and closes when triggered, so
            // it is never "opened" by anyone (the planner would close it).
            self.start_open = start_open_doors(&self.routesim, &self.maps_dir, o.map);
        }
        for m in &o.movers {
            if m.state != 0 && !self.start_open.contains(&m.index) {
                self.latched.insert(m.index);
            }
        }
        // No progress for a long time: end the run with a diagnosis instead of wandering.
        let key = match &self.phase {
            Phase::Map { walker, .. } => walker.next,
            _ => 0,
        };
        if self.progress.1 != key || self.progress.2 != o.map {
            self.progress = (self.frame, key, o.map);
        } else if self.frame - self.progress.0 > 2400 && o.mode & 2 == 0 {
            self.failure = Some(format!(
                "no progress for 2400 frames on map {} at waypoint {} pos {:?}",
                o.map, key, o.pos()
            ));
            return None;
        }
        if o.mode & 4 != 0 && o.map != 0 {
            self.failure = Some(format!(
                "player died on map {} at {:?} frame {} (health {} velocity {:?} mode {} water {})",
                o.map,
                o.pos(),
                self.frame,
                o.health,
                o.velocity,
                o.mode,
                o.water
            ));
            for m in &o.movers {
                eprintln!("  mover {} class {:#x} state {} disp {:?}", m.index, m.class, m.state, [m.origin[0] >> 12, m.origin[1] >> 12, m.origin[2] >> 12]);
            }
            let p = o.pos();
            for m in o.monsters.iter().filter(|m| m.alive()) {
                let (dx, dy) = ((m.origin[0] >> 12) - p[0], (m.origin[1] >> 12) - p[1]);
                let d = ((dx * dx + dy * dy) as f64).sqrt() as i32;
                if d < 1200 {
                    eprintln!("  near: m{} kind{} state{} hp{} dist{} z{}", m.index, m.kind, m.state, m.health, d, m.origin[2] >> 12);
                }
            }
            return None;
        }
        // The end-of-level panel waits for a button after two seconds.
        if o.mode & 2 != 0 {
            self.skip_tick += 1;
            let press = o.intermission_elapsed >= 130 && self.skip_tick % 10 < 3;
            return Some(pad::sample(Command {
                buttons: if press { button::CROSS } else { 0 },
                ..Command::default()
            }));
        }
        if let Some(m) = self.save_at {
            if o.map == m && o.mode & 1 == 0 && !self.card_saved && !matches!(self.phase, Phase::SaveSeq { .. }) {
                self.phase = Phase::SaveSeq { frames: 0 };
            }
        }
        if (1..=8).contains(&o.map) {
            self.ensure_world(o.map);
        }
        if let Phase::Map { seg, walker } = &mut self.phase {
            if walker.planner.is_none() {
                walker.planner = planner_for(&self.routesim, &self.maps_dir, SEGMENTS[*seg].map);
            }
        }
        let ctx_latched = self.latched.clone();
        let no_bounds = std::collections::HashMap::new();
        let bounds = self.world.as_ref().map_or_else(|| no_bounds, |(_, w)| w.mover_bounds());
        let ctx = Ctx { latched: &ctx_latched, bounds: &bounds, world: self.world.as_ref().map(|(_, w)| w) };
        let mut next_phase: Option<Phase> = None;
        let cmd = match &mut self.phase {
            Phase::Menu { pressed } => {
                if o.mode & 1 != 0 {
                    *pressed += 1;
                    Command {
                        buttons: if (*pressed % 8) < 3 { button::CROSS } else { 0 },
                        ..Command::default()
                    }
                } else {
                    next_phase = Some(Phase::ToSkill(Walker::new(parse_route(TO_EASY))));
                    Command::default()
                }
            }
            Phase::ToSkill(w) => match w.step(o, &ctx) {
                Some(c) => c,
                None => {
                    next_phase = Some(Phase::ToPortal(Walker::new(parse_route(TO_PORTAL))));
                    Command::default()
                }
            },
            Phase::ToPortal(w) => {
                if o.map == 1 {
                    let walker = segment_walker(&self.routesim, &self.maps_dir, 0, &mut self.cp_phases, &mut self.cp_index);
                    next_phase = Some(Phase::Map { seg: 0, walker });
                    Command::default()
                } else {
                    w.step(o, &ctx).unwrap_or_default()
                }
            }
            Phase::Map { seg, walker } => {
                let s = &SEGMENTS[*seg];
                if o.map == s.map && walker.done() && !self.cp_phases.is_empty() {
                    // The phase's route is walked: the next phase starts here, and the driver
                    // saves a checkpoint to retry the following phase from.
                    let text = self.cp_phases.pop_front().unwrap_or_default();
                    let planner = walker.planner.clone();
                    *walker = Walker::new(parse_route(&text));
                    walker.planner = planner;
                    self.cp_index += 1;
                    self.pending_checkpoint = Some(self.cp_index);
                }
                let next_map = self.exit_to.unwrap_or(s.next_map);
                if o.map == next_map {
                    self.segment_ends.push((s.map, self.frame));
                    if !self.single && *seg + 1 < SEGMENTS.len() {
                        let walker = segment_walker(&self.routesim, &self.maps_dir, *seg + 1, &mut self.cp_phases, &mut self.cp_index);
                        next_phase = Some(Phase::Map { seg: *seg + 1, walker });
                    } else if self.single && self.settle > 0 {
                        next_phase = Some(Phase::Settle { left: self.settle });
                    } else {
                        next_phase = Some(Phase::Done);
                    }
                    Command::default()
                } else if o.map != s.map {
                    self.failure = Some(format!("unexpected map {} (wanted {} or {next_map})", o.map, s.map));
                    return None;
                } else {
                    let world = &self.world.as_ref().expect("world loaded").1;
                    // Swimming: no fight, the air is short (the route is a long dive).
                    let engaged = if o.water >= 2 || walker.no_fight { None } else { self.fighter.step(o, world) };
                    match engaged {
                        Some(c) => {
                            self.last = format!("FIGHT f{} s{} y{} b{:x}", c.forward, c.strafe, c.yaw, c.buttons);
                            c
                        }
                        None => {
                            // Pick up what a player would walk a few steps for.
                            if walker.at_plain_waypoint() && o.grounded && o.water == 0 {
                                let p = o.pos();
                                let mut best: Option<(i32, u16, [i32; 3])> = None;
                                for k in &o.pickups {
                                    if self.foraged.contains(&(o.map, k.index)) || !wanted(o, k.code) {
                                        continue;
                                    }
                                    let q = [k.origin[0] >> 12, k.origin[1] >> 12, k.origin[2] >> 12];
                                    let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
                                    let d = ((dx * dx + dy * dy) as f64).sqrt() as i32;
                                    let reach = match (k.code >> 16, o.health) {
                                        (1, h) if h < 50 => 700,
                                        (1, h) if h < 75 => 400,
                                        (2, _) => 400,
                                        _ => 260,
                                    };
                                    if d <= reach && (q[2] - p[2]).abs() <= 48 && best.map_or(true, |b| d < b.0) {
                                        best = Some((d, k.index, q));
                                    }
                                }
                                if let Some((_, index, q)) = best {
                                    self.foraged.insert((o.map, index));
                                    let opened: Vec<u16> = ctx_latched.iter().copied().collect();
                                    walker.detour(p, [q[0], q[1]], &opened);
                                }
                            }
                            // Hurt and not fighting: a player walks to the nearest health box.
                            if o.health < 50
                                && o.grounded
                                && o.water == 0
                                && self.frame >= self.restock_after
                                && !walker.no_restock
                                && walker.at_plain_waypoint()
                            {
                                self.restock_after = self.frame + 240;
                                walker.route.insert(walker.next, Item::Restock { min: 80 });
                            }
                            let c = walker.step(o, &ctx).unwrap_or_default();
                            self.last = format!("WALK wp{} {} f{} s{} y{} b{:x}", walker.next, walker.cur(), c.forward, c.strafe, c.yaw, c.buttons);
                            c
                        }
                    }
                }
            }
            Phase::LoadMenu { frames, opened, map } => {
                *frames += 1;
                if o.map == *map && o.mode & 1 == 0 {
                    let seg = SEGMENTS.iter().position(|s| s.map == *map).expect("route");
                    if self.load_settle > 0 {
                        next_phase = Some(Phase::LoadSettle { left: self.load_settle, seg });
                    } else {
                        let walker = segment_walker(&self.routesim, &self.maps_dir, seg, &mut self.cp_phases, &mut self.cp_index);
                        next_phase = Some(Phase::Map { seg, walker });
                    }
                    Command::default()
                } else if *frames > 900 {
                    self.failure = Some(format!("the card did not load map {map} (map {} mode {})", o.map, o.mode));
                    return None;
                } else {
                    let mut buttons = 0;
                    if o.mode & 1 != 0 {
                        *opened += 1;
                        buttons = match *opened {
                            10..=12 | 22..=24 => button::DOWN,
                            34..=36 | 50..=52 => button::CROSS,
                            _ => 0,
                        };
                    }
                    Command { buttons, ..Command::default() }
                }
            }
            Phase::SaveSeq { frames } => {
                *frames += 1;
                if *frames >= 300 {
                    self.card_saved = true;
                    return None;
                }
                let buttons = match *frames {
                    15..=17 => button::START,
                    30..=32 | 42..=44 | 54..=56 => button::DOWN,
                    66..=68 | 80..=82 => button::CROSS,
                    _ => 0,
                };
                Command { buttons, ..Command::default() }
            }
            Phase::Settle { left } => {
                *left -= 1;
                if *left == 0 {
                    return None;
                }
                Command::default()
            }
            Phase::LoadSettle { left, seg } => {
                *left -= 1;
                if *left == 0 {
                    let seg = *seg;
                    let walker = segment_walker(&self.routesim, &self.maps_dir, seg, &mut self.cp_phases, &mut self.cp_index);
                    next_phase = Some(Phase::Map { seg, walker });
                }
                Command::default()
            }
            Phase::Done => return None,
        };
        let cmd = match (&self.phase, &self.world) {
            (Phase::Map { .. }, Some((_, world))) if next_phase.is_none() && o.water == 0 => repel(o, world, cmd),
            _ => cmd,
        };
        if let Some(p) = next_phase {
            self.phase = p;
        }
        Some(pad::sample(cmd))
    }
}

/// Whether a player in this state would go out of the way for a pickup.
fn wanted(o: &Obs, code: i32) -> bool {
    let (kind, sub, amount) = (code >> 16, (code >> 8) & 255, code & 255);
    let owns = |w: u32| o.weapons & (1 << w) != 0;
    match kind {
        1 => o.health + amount <= 125 && o.health <= 80,
        2 => o.armor < 60,
        3 => match sub {
            0 => o.ammo[0] < 60,
            1 => (owns(3) || owns(4)) && o.ammo[1] < 100,
            2 => (owns(5) || owns(6)) && o.ammo[2] < 30,
            _ => false,
        },
        4 => !owns(sub as u32),
        _ => false,
    }
}

/// Step away from a zombie within arm's reach: standing to aim or walk while one claws is how
/// the player dies, and zombies are slow enough to leave behind.
fn repel(o: &Obs, world: &World, c: Command) -> Command {
    const REACH: f64 = 75.0;
    let p = o.pos();
    let (mut ax, mut ay, mut near) = (0.0f64, 0.0f64, false);
    for m in &o.monsters {
        if m.kind != 3 || !m.alive() || matches!(m.state, 13 | 14 | 15 | 17) {
            continue;
        }
        let (mx, my, mz) = (m.origin[0] >> 12, m.origin[1] >> 12, m.origin[2] >> 12);
        if (mz - p[2]).abs() > 64 {
            continue;
        }
        let (dx, dy) = ((p[0] - mx) as f64, (p[1] - my) as f64);
        let d = (dx * dx + dy * dy).sqrt().max(1.0);
        if d < REACH {
            ax += dx / d * (REACH - d + 10.0);
            ay += dy / d * (REACH - d + 10.0);
            near = true;
        }
    }
    if !near {
        return c;
    }
    let (forward, strafe) = move_toward(o, ax as i32, ay as i32, 4);
    let (forward, strafe) = Fighter::keep_footing(o, world, forward, strafe);
    Command { forward, strafe, ..c }
}
