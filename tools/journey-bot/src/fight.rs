//! A fighting policy that reads the same things a player sees: where the monsters are,
//! what is in hand, what is in the shells box.

use crate::obs::{Monster, Obs};
use crate::pad::{button, Command};
use crate::walk::{heading, pitch_to, turn_to, wrap_q12};
use crate::world::World;

pub const AXE: u32 = 0;
pub const SHOTGUN: u32 = 1;
pub const SUPER_SHOTGUN: u32 = 2;
pub const NAILGUN: u32 = 3;
pub const SUPER_NAILGUN: u32 = 4;
pub const GRENADE: u32 = 5;
pub const ROCKET: u32 = 6;

/// Monster states that mean it is no longer a threat.
fn threat(m: &Monster) -> bool {
    m.alive() && !matches!(m.state, 13 | 14 | 15 | 17)
}

pub struct Fighter {
    frame: u32,
    last_cycle: u32,
    pub engaged_frames: u32,
    pub target: Option<u16>,
    strafe_sign: i32,
    flip_at: u32,
}

impl Fighter {
    pub fn new() -> Self {
        Self { frame: 0, last_cycle: 0, engaged_frames: 0, target: None, strafe_sign: 1, flip_at: 0 }
    }

    fn desired_weapon(o: &Obs, dist: i32) -> u32 {
        let owns = |w: u32| o.weapons & (1 << w) != 0;
        let (shells, nails, rockets) = (o.ammo[0], o.ammo[1], o.ammo[2]);
        if owns(ROCKET) && rockets > 0 && dist > 220 && o.health > 40 && dist < 700 {
            return ROCKET;
        }
        if owns(SUPER_SHOTGUN) && shells >= 2 {
            return SUPER_SHOTGUN;
        }
        if owns(SUPER_NAILGUN) && nails >= 2 {
            return SUPER_NAILGUN;
        }
        if owns(NAILGUN) && nails >= 1 {
            return NAILGUN;
        }
        if shells >= 1 && owns(SHOTGUN) {
            return SHOTGUN;
        }
        AXE
    }

    fn max_range(weapon: u32) -> i32 {
        match weapon {
            AXE => 64,
            SHOTGUN | SUPER_SHOTGUN => 450,
            _ => 900,
        }
    }

    /// Pick the target this frame: nearest threat with a clear line, in range.
    fn pick<'a>(&self, o: &'a Obs, world: &World) -> Option<(&'a Monster, i32)> {
        let p = o.pos();
        let eye = [o.origin[0], o.origin[1], o.origin[2] + (22 << 12)];
        let mut best: Option<(&Monster, i32)> = None;
        for m in &o.monsters {
            if !threat(m) {
                continue;
            }
            let mp = [m.origin[0] >> 12, m.origin[1] >> 12, m.origin[2] >> 12];
            let dx = (mp[0] - p[0]) as f64;
            let dy = (mp[1] - p[1]) as f64;
            let dist = (dx * dx + dy * dy).sqrt() as i32;
            let dz = (mp[2] - p[2]).abs();
            // Sleepers are only fought when close; hunters from farther away.
            let reach = if m.active() { 800 } else { 250 };
            if dist > reach || dz > 200 {
                continue;
            }
            let aim = [m.origin[0], m.origin[1], m.origin[2] + (8 << 12)];
            if !world.los(eye, aim) {
                continue;
            }
            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((m, dist));
            }
        }
        best
    }

    /// Drop the part of a move that would carry the player off a ledge.
    fn keep_footing(o: &Obs, world: &World, forward: i32, strafe: i32) -> (i32, i32) {
        if forward == 0 && strafe == 0 {
            return (forward, strafe);
        }
        let p = o.pos();
        let yaw = (o.angles[1] & 4095) as f64 / 4096.0 * std::f64::consts::TAU;
        let (c, s) = (yaw.cos(), yaw.sin());
        // Left strafe is positive; the left vector of yaw is (-sin, cos).
        let (wx, wy) = (forward as f64 * c - strafe as f64 * s, forward as f64 * s + strafe as f64 * c);
        let len = (wx * wx + wy * wy).sqrt().max(1.0);
        let (ux, uy) = (wx / len, wy / len);
        for reach in [24.0, 48.0, 72.0, 100.0] {
            let x = p[0] + (ux * reach) as i32;
            let y = p[1] + (uy * reach) as i32;
            if !world.floor_below(x, y, p[2], 40) {
                return (0, 0);
            }
        }
        (forward, strafe)
    }

    /// `Some(command)` when a fight takes over the frame.
    pub fn step(&mut self, o: &Obs, world: &World) -> Option<Command> {
        self.frame += 1;
        let Some((m, dist)) = self.pick(o, world) else {
            self.target = None;
            self.engaged_frames = 0;
            return None;
        };
        self.target = Some(m.index);
        self.engaged_frames += 1;
        let p = o.pos();
        let mp = [m.origin[0] >> 12, m.origin[1] >> 12, m.origin[2] >> 12];
        let want_weapon = Self::desired_weapon(o, dist);
        let mut buttons = 0u16;
        if o.weapon != want_weapon && self.frame >= self.last_cycle + 8 {
            self.last_cycle = self.frame;
            buttons |= button::R1;
        }
        let (yaw_cmd, err) = turn_to(o, heading(mp[0] - p[0], mp[1] - p[1]));
        let horiz = {
            let dx = (mp[0] - p[0]) as f64;
            let dy = (mp[1] - p[1]) as f64;
            (dx * dx + dy * dy).sqrt()
        };
        let dz = (mp[2] + 8 - (p[2] + 22)) as f64;
        let pitch_want = -((dz.atan2(horiz.max(1.0)) / (2.0 * std::f64::consts::PI) * 4096.0).round() as i32);
        let tolerance = (40 * 650 / dist.max(1)).clamp(30, 260);
        let aligned = err.abs() <= tolerance;
        // Never fire with a barrel close enough to take the player with it, nor with one
        // standing between the muzzle and the target.
        let near_barrel = o.props.iter().any(|b| {
            let bp = [b.origin[0] >> 12, b.origin[1] >> 12];
            let (dx, dy) = ((bp[0] - p[0]) as f64, (bp[1] - p[1]) as f64);
            (dx * dx + dy * dy).sqrt() < 140.0 && ((b.origin[2] >> 12) - p[2]).abs() < 120
        });
        if o.weapon == want_weapon && aligned && dist <= Self::max_range(want_weapon) && !near_barrel {
            buttons |= button::R2;
        }
        if self.frame >= self.flip_at {
            self.strafe_sign = -self.strafe_sign;
            self.flip_at = self.frame + 25 + (m.index as u32 % 20);
        }
        let weapon = o.weapon;
        let (forward, strafe) = if err.abs() > 700 {
            (0, 0)
        } else if weapon == AXE {
            (if dist > 56 { 127 } else { 0 }, 0)
        } else if matches!(weapon, SHOTGUN | SUPER_SHOTGUN) {
            if dist > 280 {
                (127, 40 * self.strafe_sign)
            } else if dist < 110 {
                (-127, 90 * self.strafe_sign)
            } else {
                (20, 127 * self.strafe_sign)
            }
        } else if dist > 500 {
            (80, 60 * self.strafe_sign)
        } else {
            (0, 127 * self.strafe_sign)
        };
        let _ = wrap_q12(0);
        let (forward, strafe) = Self::keep_footing(o, world, forward, strafe);
        Some(Command {
            forward,
            strafe,
            yaw: yaw_cmd,
            pitch: pitch_to(o, pitch_want),
            buttons,
        })
    }
}
