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
];

enum Phase {
    Menu { pressed: u32 },
    ToSkill(Walker),
    ToPortal(Walker),
    Map { seg: usize, walker: Walker },
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
    last_map: u32,
    skip_tick: u32,
    foraged: HashSet<(u32, u16)>,
    progress: (u32, usize, u32),
    pub last: String,
    pub failure: Option<String>,
    pub segment_ends: Vec<(u32, u32)>,
}

const TO_EASY: &str = "544 544\n336 768\n272 1200\n256 1328\n232 1352\n";
const TO_PORTAL: &str = "352 1680\n304 1696\n176 1696\n-48 1648\n-64 1628\n";

const MAP_NAMES: [&str; 9] = ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"];

fn planner_for(routesim: &str, maps_dir: &str, map: u32) -> Option<Planner> {
    if routesim.is_empty() {
        return None;
    }
    Some(Planner { last_dz: Default::default(), routesim: routesim.to_string(), psb: format!("{maps_dir}/{}.psb", MAP_NAMES[map as usize]) })
}

impl Journey {
    /// Continue on map `map` as if the journey had just arrived there.
    pub fn resumed(maps_dir: &str, map: u32) -> Self {
        let mut j = Self::new(maps_dir);
        let seg = SEGMENTS.iter().position(|s| s.map == map).expect("no route for that map");
        j.last_map = map;
        j.phase = Phase::Map { seg, walker: Walker::new(parse_route(SEGMENTS[seg].route)) };
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
            last_map: 0,
            skip_tick: 0,
            foraged: HashSet::new(),
            progress: (0, 0, 0),
            last: String::new(),
            failure: None,
            segment_ends: Vec::new(),
        }
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
                self.latched.len(),
                walker.replans,
                walker.plan_note.clone().unwrap_or_default()
            ),
            Phase::Done => "done".into(),
        }
    }

    fn step(&mut self, o: &Obs) -> Option<PadSample> {
        self.frame += 1;
        if o.map != self.last_map {
            self.latched.clear();
            self.last_map = o.map;
        }
        for m in &o.movers {
            if m.state != 0 {
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
            self.failure = Some(format!("player died on map {} at {:?} frame {}", o.map, o.pos(), self.frame));
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
        if (1..=8).contains(&o.map) {
            self.ensure_world(o.map);
        }
        if let Phase::Map { seg, walker } = &mut self.phase {
            if walker.planner.is_none() {
                walker.planner = planner_for(&self.routesim, &self.maps_dir, SEGMENTS[*seg].map);
            }
        }
        let ctx_latched = self.latched.clone();
        let ctx = Ctx { latched: &ctx_latched };
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
                    let mut walker = Walker::new(parse_route(SEGMENTS[0].route));
                    walker.planner = planner_for(&self.routesim, &self.maps_dir, 1);
                    next_phase = Some(Phase::Map { seg: 0, walker });
                    Command::default()
                } else {
                    w.step(o, &ctx).unwrap_or_default()
                }
            }
            Phase::Map { seg, walker } => {
                let s = &SEGMENTS[*seg];
                if o.map == s.next_map {
                    self.segment_ends.push((s.map, self.frame));
                    if *seg + 1 < SEGMENTS.len() {
                        let mut walker = Walker::new(parse_route(SEGMENTS[*seg + 1].route));
                        walker.planner = planner_for(&self.routesim, &self.maps_dir, SEGMENTS[*seg + 1].map);
                        next_phase = Some(Phase::Map { seg: *seg + 1, walker });
                    } else {
                        next_phase = Some(Phase::Done);
                    }
                    Command::default()
                } else if o.map != s.map {
                    self.failure = Some(format!("unexpected map {} (wanted {})", o.map, s.map));
                    return None;
                } else {
                    let world = &self.world.as_ref().expect("world loaded").1;
                    // Swimming: no fight, the air is short (the route is a long dive).
                    let engaged = if o.water >= 2 { None } else { self.fighter.step(o, world) };
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
                            let c = walker.step(o, &ctx).unwrap_or_default();
                            self.last = format!("WALK wp{} {} f{} s{} y{} b{:x}", walker.next, walker.cur(), c.forward, c.strafe, c.yaw, c.buttons);
                            c
                        }
                    }
                }
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
