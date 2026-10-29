//! Waypoint following with the pad: turn toward the point, walk when roughly facing it.

use std::collections::HashSet;

use crate::obs::Obs;
use crate::pad::{button, Command};

/// One point on a route, world units.
#[derive(Clone, Debug)]
pub struct Wp {
    pub x: i32,
    pub y: i32,
    /// Required floor height (within 4 units) to count as reached.
    pub z: Option<i32>,
    pub radius: i32,
    pub jump: bool,
    /// Tap the use button while waiting here.
    pub use_btn: bool,
    /// A button reached by walking into it: skipped when its mover has already moved.
    pub touch: bool,
    /// Movers (cooked source index) that must have moved before the route goes on.
    pub req: Vec<u16>,
}

/// A condition the route can wait for.
#[derive(Clone, Debug)]
pub enum Cond {
    /// Mover (cooked source index) is in a state: 0 bottom, 1 top, 2 up, 3 down.
    Mover(u16, u8),
    /// The mover has left the bottom at some point on this map.
    Latched(u16),
    /// Carrying every key bit in the mask.
    Keys(u32),
    /// Carrying none of the key bits in the mask.
    NoKeys(u32),
    ZAtLeast(i32),
    ZAtMost(i32),
    /// The step has run this many frames.
    Frames(u32),
}

impl Cond {
    pub fn parse(t: &str) -> Cond {
        let f: Vec<&str> = t.split(':').collect();
        match f[0] {
            "mover" => Cond::Mover(f[1].parse().unwrap(), match f[2] {
                "bottom" => 0,
                "top" => 1,
                "up" => 2,
                "down" => 3,
                other => panic!("mover state {other}"),
            }),
            "latched" => Cond::Latched(f[1].parse().unwrap()),
            "key" => Cond::Keys(f[1].parse().unwrap()),
            "nokey" => Cond::NoKeys(f[1].parse().unwrap()),
            "zge" => Cond::ZAtLeast(f[1].parse().unwrap()),
            "zle" => Cond::ZAtMost(f[1].parse().unwrap()),
            "frames" => Cond::Frames(f[1].parse().unwrap()),
            other => panic!("condition {other}"),
        }
    }

    pub fn holds(&self, o: &Obs, ctx: &Ctx<'_>, frames: u32) -> bool {
        match self {
            Cond::Mover(i, st) => o.movers.iter().any(|m| m.index == *i && m.state == *st),
            Cond::Latched(i) => ctx.latched.contains(i),
            Cond::Keys(mask) => o.keys & mask == *mask,
            Cond::NoKeys(mask) => o.keys & mask == 0,
            Cond::ZAtLeast(z) => o.pos()[2] >= *z,
            Cond::ZAtMost(z) => o.pos()[2] <= *z,
            Cond::Frames(n) => frames >= *n,
        }
    }
}

/// One entry of a route: a point to walk to or something to do where the player stands.
#[derive(Clone, Debug)]
pub enum Item {
    Wp(Wp),
    /// Stand still until the condition holds.
    Wait(Cond),
    /// Hold weapon `weapon`, look at `target` (world units) and fire until the condition.
    Shoot { target: [i32; 3], weapon: u32, until: Cond },
    /// Tap use while facing `target` until the condition.
    UseAt { target: [i32; 3], until: Cond },
    /// Walk straight at `target` (no pathing, no stopping) until the condition.
    Push { target: [i32; 2], until: Cond },
    /// Walk to `(x, y)` along a planned dry path (every node kept), planned from wherever the
    /// player stands when the item is reached.
    Go { x: i32, y: i32, z: Option<i32>, radius: i32 },
}

/// Parse `x y [z=N] [r=N] [jump] [use] [req=a,b]` lines; `#` starts a comment.
pub fn parse_route(text: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if let Some(d) = f[0].strip_prefix('@') {
            let until = |f: &[&str]| -> Cond {
                f.iter()
                    .find_map(|t| t.strip_prefix("until="))
                    .map(Cond::parse)
                    .unwrap_or_else(|| panic!("directive needs until=: {line}"))
            };
            let num = |i: usize| -> i32 { f[i].parse().unwrap_or_else(|_| panic!("bad number in {line}")) };
            match d {
                "wait" => out.push(Item::Wait(until(&f))),
                "shoot" => out.push(Item::Shoot {
                    target: [num(1), num(2), num(3)],
                    weapon: f.iter().find_map(|t| t.strip_prefix("weapon=")).map_or(1, |v| v.parse().unwrap()),
                    until: until(&f),
                }),
                "use" => out.push(Item::UseAt { target: [num(1), num(2), num(3)], until: until(&f) }),
                "push" => out.push(Item::Push { target: [num(1), num(2)], until: until(&f) }),
                "go" => out.push(Item::Go {
                    x: num(1),
                    y: num(2),
                    z: f.iter().find_map(|t| t.strip_prefix("z=")).map(|v| v.parse().unwrap()),
                    radius: f.iter().find_map(|t| t.strip_prefix("r=")).map_or(16, |v| v.parse().unwrap()),
                }),
                other => panic!("unknown directive @{other}"),
            }
            continue;
        }
        let mut w = Wp {
            x: f[0].parse().unwrap_or_else(|_| panic!("bad route line {line}")),
            y: f[1].parse().unwrap_or_else(|_| panic!("bad route line {line}")),
            z: None,
            radius: 20,
            jump: false,
            use_btn: false,
            touch: false,
            req: Vec::new(),
        };
        for t in &f[2..] {
            if let Some(v) = t.strip_prefix("z=") {
                w.z = Some(v.parse().unwrap());
            } else if let Some(v) = t.strip_prefix("r=") {
                w.radius = v.parse().unwrap();
            } else if *t == "jump" {
                w.jump = true;
            } else if *t == "use" {
                w.use_btn = true;
                w.touch = true;
            } else if *t == "touch" {
                w.touch = true;
            } else if let Some(v) = t.strip_prefix("req=") {
                w.req = v.split(',').map(|n| n.parse().unwrap()).collect();
            } else if *t != "-" {
                panic!("bad route token {t} in {line}");
            }
        }
        out.push(Item::Wp(w));
    }
    out
}

pub fn wrap_q12(a: i32) -> i32 {
    let a = a & 4095;
    if a >= 2048 {
        a - 4096
    } else {
        a
    }
}

/// Yaw (Q12 turns) of the direction `(dx, dy)`.
pub fn heading(dx: i32, dy: i32) -> i32 {
    let a = (dy as f64).atan2(dx as f64) / (2.0 * std::f64::consts::PI) * 4096.0;
    a.round() as i32 & 4095
}

pub fn yaw_of(o: &Obs) -> i32 {
    o.angles[1] & 4095
}

pub fn dist2(o: &Obs, x: i32, y: i32) -> i32 {
    let p = o.pos();
    let (dx, dy) = ((x - p[0]) as f64, (y - p[1]) as f64);
    (dx * dx + dy * dy).sqrt() as i32
}

/// Turn toward `target_yaw`; returns the per-tick yaw command and the error.
pub fn turn_to(o: &Obs, target_yaw: i32) -> (i32, i32) {
    let err = wrap_q12(target_yaw - yaw_of(o));
    ((err / 3).clamp(-32, 32), err)
}

/// Pitch back toward `want` (Q12), positive down.
pub fn pitch_to(o: &Obs, want: i32) -> i32 {
    let cur = if o.angles[0] > 2048 { o.angles[0] - 4096 } else { o.angles[0] };
    ((want - cur) / 3).clamp(-24, 24)
}

/// What a walker needs from the run besides the frame: movers that have moved.
pub struct Ctx<'a> {
    pub latched: &'a HashSet<u16>,
}

/// Planning request: where the routesim binary and the cooked map are.
#[derive(Clone)]
pub struct Planner {
    pub routesim: String,
    pub psb: String,
}

impl Planner {
    /// Dry walkable path to `(x, y)` with every grid node kept (no straight-line shortcuts,
    /// so a narrow bridge stays a bridge). `radius` is the arrival radius of the last point.
    pub fn plan_dry(&self, from: [i32; 3], to: [i32; 2], to_z: Option<i32>, opened: &[u16], radius: i32) -> Option<Vec<Wp>> {
        let open: Vec<String> = opened.iter().map(|i| i.to_string()).collect();
        let z = to_z.map_or("any".to_string(), |z| z.to_string());
        let out = std::process::Command::new(&self.routesim)
            .args([&self.psb, "path", "--from", &from[0].to_string(), &from[1].to_string(), &from[2].to_string(), "--to", &to[0].to_string(), &to[1].to_string(), &z])
            .env("ROUTESIM_OPEN", open.join(","))
            .env("ROUTESIM_NO_SIMPLIFY", "1")
            .env("ROUTESIM_PATH_Z", "1")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("PATH STUCK") {
            return None;
        }
        let mut nodes: Vec<(i32, i32)> = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            nodes.push((f[0].parse().ok()?, f[1].parse().ok()?));
        }
        let last = nodes.len().checked_sub(1)?;
        // Every second node (32 units apart on the 16-unit grid), always the last.
        let route = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 1 || *i == last)
            .map(|(i, &(x, y))| Wp {
                x,
                y,
                z: None,
                radius: if i == last { radius } else { 16 },
                jump: false,
                use_btn: false,
                touch: false,
                req: Vec::new(),
            })
            .collect();
        Some(route)
    }

    /// Breadth-first walkable route from `from` to `(x, y)` at any height, as waypoints.
    pub fn plan(&self, from: [i32; 3], to: [i32; 2], to_z: Option<i32>, opened: &[u16]) -> Option<Vec<Wp>> {
        let open: Vec<String> = opened.iter().map(|i| i.to_string()).collect();
        let z = to_z.map_or("any".to_string(), |z| z.to_string());
        let out = std::process::Command::new(&self.routesim)
            .args([&self.psb, "path", "--from", &from[0].to_string(), &from[1].to_string(), &from[2].to_string(), "--to", &to[0].to_string(), &to[1].to_string(), &z, "--liquid"])
            .env("ROUTESIM_OPEN", open.join(","))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("PATH STUCK") {
            return None;
        }
        let mut route = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            route.push(Wp {
                x: f[0].parse().ok()?,
                y: f[1].parse().ok()?,
                z: None,
                radius: 24,
                jump: false,
                use_btn: false,
                touch: false,
                req: Vec::new(),
            });
        }
        Some(route)
    }
}

const DETOURS: [i32; 8] = [0, 512, 3584, 1024, 3072, 1536, 2560, 2048];

/// Steer the body straight at a world offset `(dx, dy)` whatever the view yaw is: split
/// it into forward and strafe and taper with distance so a small waypoint is not
/// overshot (the same taper the route gates use).
pub fn move_toward(o: &Obs, dx: i32, dy: i32, gain: i32) -> (i32, i32) {
    let yaw = yaw_of(o) as f64 / 4096.0 * std::f64::consts::TAU;
    let (c, s) = (yaw.cos(), yaw.sin());
    let (fx, fy) = (dx as f64, dy as f64);
    let forward = fx * c + fy * s;
    let left = -fx * s + fy * c;
    let scale = forward.abs().max(left.abs()).max(1.0);
    let limit = ((dx.abs().max(dy.abs()) * gain).clamp(16, 127)) as f64;
    (((forward * limit / scale) as i32).clamp(-127, 127), ((left * limit / scale) as i32).clamp(-127, 127))
}

pub struct Walker {
    pub route: Vec<Item>,
    pub next: usize,
    pub frames: u32,
    item_frames: u32,
    stall: u32,
    best: i32,
    detour: usize,
    pub waiting: bool,
    tap: u32,
    pub planner: Option<Planner>,
    pub replans: u32,
    since_best: u32,
    pub plan_note: Option<String>,
    last_cycle: u32,
}

impl Walker {
    pub fn new(route: Vec<Item>) -> Self {
        Self {
            route,
            next: 0,
            frames: 0,
            item_frames: 0,
            stall: 0,
            best: i32::MAX,
            detour: 0,
            waiting: false,
            tap: 0,
            planner: None,
            replans: 0,
            since_best: 0,
            plan_note: None,
            last_cycle: 0,
        }
    }

    pub fn done(&self) -> bool {
        self.next >= self.route.len()
    }

    fn advance(&mut self) {
        self.next += 1;
        self.best = i32::MAX;
        self.stall = 0;
        self.detour = 0;
        self.waiting = false;
        self.since_best = 0;
        self.item_frames = 0;
    }

    /// Look straight at a world point (units) and report whether the aim is on it.
    fn aim_at(o: &Obs, target: [i32; 3]) -> (Command, bool) {
        let p = o.pos();
        let (dx, dy) = ((target[0] - p[0]) as f64, (target[1] - p[1]) as f64);
        let horiz = (dx * dx + dy * dy).sqrt().max(1.0);
        let dz = (target[2] - (p[2] + 22)) as f64;
        let pitch_want = -((dz.atan2(horiz) / std::f64::consts::TAU * 4096.0).round() as i32);
        let (yaw, err) = turn_to(o, heading(target[0] - p[0], target[1] - p[1]));
        let cur = if o.angles[0] > 2048 { o.angles[0] - 4096 } else { o.angles[0] };
        let aligned = err.abs() < 40 && (pitch_want - cur).abs() < 60;
        (Command { yaw, pitch: pitch_to(o, pitch_want), ..Command::default() }, aligned)
    }

    /// One frame of the route. Returns `None` when it is finished.
    pub fn step(&mut self, o: &Obs, ctx: &Ctx<'_>) -> Option<Command> {
        self.frames += 1;
        self.item_frames += 1;
        let p = o.pos();
        loop {
            match self.route.get(self.next)? {
                Item::Wp(w) => {
                    let satisfied = w.req.iter().all(|r| ctx.latched.contains(r));
                    if w.touch && !w.req.is_empty() && satisfied {
                        self.advance();
                        continue;
                    }
                }
                Item::Go { x, y, z, radius } => {
                    let (x, y, z, radius) = (*x, *y, *z, *radius);
                    let mut path = match &self.planner {
                        Some(pl) => {
                            let opened: Vec<u16> = ctx.latched.iter().copied().collect();
                            pl.plan_dry(p, [x, y], z, &opened, radius)
                        }
                        None => None,
                    };
                    if path.is_none() {
                        self.plan_note = Some(format!("go FAILED at {:?} -> ({},{})", p, x, y));
                    }
                    let mut fallback = path.take().unwrap_or_else(|| {
                        vec![Wp { x, y, z: None, radius, jump: false, use_btn: false, touch: false, req: Vec::new() }]
                    });
                    if let Some(last) = fallback.last_mut() {
                        last.z = z;
                    }
                    self.route.remove(self.next);
                    for (k, w) in fallback.into_iter().enumerate() {
                        self.route.insert(self.next + k, Item::Wp(w));
                    }
                    self.best = i32::MAX;
                    continue;
                }
                Item::Wait(c) | Item::Shoot { until: c, .. } | Item::UseAt { until: c, .. } | Item::Push { until: c, .. } => {
                    if c.holds(o, ctx, self.item_frames) {
                        self.advance();
                        continue;
                    }
                }
            }
            break;
        }
        let item = self.route[self.next].clone();
        // Standing items start from rest: kill any slide first, with the body, so a ledge
        // stop does not become a fall.
        if !matches!(item, Item::Wp(_) | Item::Push { .. }) {
            let (vx, vy) = (o.velocity[0] >> 12, o.velocity[1] >> 12);
            if vx * vx + vy * vy > 80 * 80 && o.grounded {
                let (forward, strafe) = move_toward(o, -vx, -vy, 8);
                return Some(Command { forward, strafe, pitch: pitch_to(o, 0), ..Command::default() });
            }
        }
        let w = match item {
            Item::Wp(w) => w,
            Item::Go { .. } => unreachable!("expanded above"),
            Item::Wait(_) => {
                self.waiting = true;
                return Some(Command { pitch: pitch_to(o, 0), ..Command::default() });
            }
            Item::Shoot { target, weapon, .. } => {
                self.waiting = true;
                let (mut cmd, aligned) = Self::aim_at(o, target);
                if o.weapon != weapon {
                    if self.frames >= self.last_cycle + 8 {
                        self.last_cycle = self.frames;
                        cmd.buttons |= button::R1;
                    }
                } else if aligned {
                    cmd.buttons |= button::R2;
                }
                return Some(cmd);
            }
            Item::UseAt { target, .. } => {
                self.waiting = true;
                let (mut cmd, _) = Self::aim_at(o, target);
                self.tap += 1;
                if self.tap % 12 < 3 {
                    cmd.buttons |= button::SQUARE;
                }
                return Some(cmd);
            }
            Item::Push { target, .. } => {
                let (yaw, _) = turn_to(o, heading(target[0] - p[0], target[1] - p[1]));
                let (forward, strafe) = move_toward(o, target[0] - p[0], target[1] - p[1], 4);
                return Some(Command { forward, strafe, yaw, pitch: pitch_to(o, 0), ..Command::default() });
            }
        };
        let d = dist2(o, w.x, w.y);
        // A plain waypoint the player has slid past (a fight pushed it off the line, or
        // the corner was cut) counts as reached once it is behind and still close.
        if w.req.is_empty() && w.z.is_none() && !w.use_btn && w.radius >= 16 && d < 90 {
            if let Some(Item::Wp(n)) = self.route.get(self.next + 1) {
                let (lx, ly) = ((n.x - w.x) as i64, (n.y - w.y) as i64);
                let (px, py) = ((p[0] - w.x) as i64, (p[1] - w.y) as i64);
                if lx * px + ly * py > 0 {
                    self.advance();
                    return self.step(o, ctx);
                }
            }
        }
        let z_ok = w.z.map_or(true, |z| (p[2] - z).abs() <= 4);
        let satisfied = w.req.iter().all(|r| ctx.latched.contains(r));
        if d <= w.radius && z_ok {
            if satisfied {
                self.advance();
                return if self.done() { None } else { self.step(o, ctx) };
            }
            self.waiting = true;
            self.tap += 1;
            let press = w.use_btn && self.tap % 12 < 3;
            return Some(Command {
                buttons: if press { button::SQUARE } else { 0 },
                pitch: pitch_to(o, 0),
                ..Command::default()
            });
        }
        self.waiting = false;
        if d <= w.radius && !z_ok && w.z.is_some() {
            // Standing on the mark but at the wrong height (lift still moving): wait.
            return Some(Command { pitch: pitch_to(o, 0), ..Command::default() });
        }
        if d < self.best - 2 {
            self.best = d;
            self.stall = 0;
            self.detour = 0;
            self.since_best = 0;
        } else {
            self.since_best += 1;
            if self.since_best == 150 || self.since_best == 600 {
                if let Some(pl) = self.planner.clone() {
                    let opened: Vec<u16> = ctx.latched.iter().copied().collect();
                    match pl.plan(p, [w.x, w.y], w.z, &opened) {
                        Some(path) => {
                            self.replans += 1;
                            self.plan_note = Some(format!("replan at {:?} -> ({},{}): {} points", p, w.x, w.y, path.len()));
                            let at = self.next;
                            for (k, wpt) in path.into_iter().enumerate() {
                                self.route.insert(at + k, Item::Wp(wpt));
                            }
                            self.best = i32::MAX;
                            self.stall = 0;
                            self.detour = 0;
                        }
                        None => {
                            self.plan_note = Some(format!("replan FAILED at {:?} -> ({},{})", p, w.x, w.y));
                        }
                    }
                }
            }
            self.stall += 1;
            if self.stall > 45 {
                self.stall = 0;
                self.detour = (self.detour + 1) & 7;
            }
        }
        let target_yaw = (heading(w.x - p[0], w.y - p[1]) + DETOURS[self.detour]) & 4095;
        let (yaw, err) = turn_to(o, target_yaw);
        let aligned = err.abs();
        let (target_dx, target_dy) = if self.detour == 0 {
            (w.x - p[0], w.y - p[1])
        } else {
            let a = DETOURS[self.detour] as f64 / 4096.0 * std::f64::consts::TAU;
            let (dx, dy) = ((w.x - p[0]) as f64, (w.y - p[1]) as f64);
            ((dx * a.cos() - dy * a.sin()) as i32, (dx * a.sin() + dy * a.cos()) as i32)
        };
        // Taper hard into a point the route stops at (the next entry is not a plain waypoint,
        // or the mark is small): a full-speed run slides tens of units past a ledge.
        let stops_here = !matches!(self.route.get(self.next + 1), Some(Item::Wp(_))) || w.radius <= 12;
        let (forward, strafe) = move_toward(o, target_dx, target_dy, if stops_here { 2 } else { 4 });
        // Dunked: face the point, swim toward it holding jump to rise, and let the water
        // jump hop the ledge, as a person does.
        let in_liquid = o.water != 0;
        let pitch_want = if in_liquid {
            let up = w.z.map_or(false, |z| z > p[2] + 4);
            if up {
                let horiz = (d as f64).max(1.0);
                -(((w.z.unwrap() + 64 - p[2]) as f64).atan2(horiz) / std::f64::consts::TAU * 4096.0).round() as i32
            } else {
                0
            }
        } else {
            0
        };
        Some(Command {
            forward,
            strafe,
            yaw,
            pitch: pitch_to(o, pitch_want),
            buttons: if (w.jump && aligned < 150 && o.grounded && self.frames % 8 < 2) || in_liquid {
                button::CROSS
            } else {
                0
            },
        })
    }
}

impl Walker {
    /// True when the walker is heading for a plain waypoint (not waiting, shooting or pushing).
    pub fn at_plain_waypoint(&self) -> bool {
        !self.waiting && matches!(self.route.get(self.next), Some(Item::Wp(_)))
    }

    /// Walk to `to` first (a pickup), then carry on to the waypoint that was being
    /// approached, both along planned dry paths. Returns false when no path exists.
    pub fn detour(&mut self, from: [i32; 3], to: [i32; 2], opened: &[u16]) -> bool {
        let Some(pl) = self.planner.clone() else { return false };
        let Some(Item::Wp(back)) = self.route.get(self.next).cloned() else { return false };
        let Some(there) = pl.plan_dry(from, to, None, opened, 10) else { return false };
        let at = self.next;
        let mut items: Vec<Item> = there.into_iter().map(Item::Wp).collect();
        items.push(Item::Go { x: back.x, y: back.y, z: None, radius: back.radius.max(10) });
        for (k, it) in items.into_iter().enumerate() {
            self.route.insert(at + k, it);
        }
        self.best = i32::MAX;
        true
    }
}
