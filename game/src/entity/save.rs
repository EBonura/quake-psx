//! The scene's half of a saved game: everything that moves or changes after a
//! map loads, and nothing that the map itself rebuilds.
//!
//! A save is read back into a scene freshly loaded from the same map, so only
//! the difference matters: which entities are still standing, where each
//! monster is and what it is doing, how far every door, lift and train has
//! travelled, which triggers have fired, what the target graph is counting.
//! Projectiles in flight, particles, lights' animation phase and sounds are
//! not saved; they are a fraction of a second old and restart with the map.
//! The original saves those too. That is a recorded limit (LIMITS.md).

use quake_core::save::{Reader, Snapshot, Writer};
use quake_core::targets::{load_activator, save_activator};

use super::*;

fn save_bits(w: &mut Writer, count: usize, mut bit: impl FnMut(usize) -> bool) {
    let mut index = 0;
    while index < count {
        let mut byte = 0u8;
        for offset in 0..8 {
            if index + offset < count && bit(index + offset) {
                byte |= 1 << offset;
            }
        }
        w.u8(byte);
        index += 8;
    }
}

fn load_bits(r: &mut Reader, count: usize, mut set: impl FnMut(usize, bool)) {
    let mut index = 0;
    while index < count {
        let byte = r.u8();
        for offset in 0..8 {
            if index + offset < count {
                set(index + offset, byte & (1 << offset) != 0);
            }
        }
        index += 8;
    }
}

impl EntityScene {
    /// Write the scene's moving parts. See the module comment.
    #[inline(never)]
    #[optimize(size)]
    pub fn save_state(&self, w: &mut Writer) {
        w.u32(self.frame);
        w.u8(self.animation_subtick);
        w.u32(self.chase_random);
        w.u16(self.sight_index);
        w.u16(self.sight_alert_ticks);
        for word in self.light_off {
            w.u32(word);
        }

        let count = self.entities.len();
        w.u16(count as u16);
        save_bits(w, count, |i| self.entities[i].visible);
        save_bits(w, count, |i| self.entities[i].solid);
        save_bits(w, count, |i| self.entities[i].damageable);
        save_bits(w, count, |i| self.entities[i].pickup.is_some());
        // Health of everything that has any to lose: barrels and shootable
        // brushes as well as monsters.
        w.u16(self.entities.iter().filter(|e| e.max_health > 0).count() as u16);
        for (index, entity) in self.entities.iter().enumerate() {
            if entity.max_health > 0 {
                w.u16(index as u16);
                w.i16(entity.health);
            }
        }
        w.u16(self.entities.iter().filter(|e| e.monster.is_some()).count() as u16);
        for (index, entity) in self.entities.iter().enumerate() {
            let Some(monster) = entity.monster else {
                continue;
            };
            w.u16(index as u16);
            w.vec3(entity.origin);
            w.i16(entity.angles.y);
            w.u16(entity.frame);
            monster.save(w);
            w.u16(entity.patrol);
            w.i16(entity.ideal_yaw);
            w.bool(entity.partial_ground);
        }

        w.u16(self.movers.len() as u16);
        for mover in &self.movers {
            save_activator(w, mover.activator);
            w.u16(mover.key_cooldown);
            w.bool(mover.key_spent);
            mover.crush.save_dynamic(w);
            w.i16(mover.health);
            w.bool(mover.shot_open);
            mover.policy.save_dynamic(w);
        }
        w.u16(self.door_fields.len() as u16);
        for field in &self.door_fields {
            field.save_dynamic(w);
        }
        w.u16(self.trains.len() as u16);
        for train in &self.trains {
            train.crush.save_dynamic(w);
            train.policy.save_dynamic(w);
        }
        w.u16(self.triggers.len() as u16);
        for trigger in &self.triggers {
            w.bool(trigger.armed);
            w.u16(trigger.cooldown);
            trigger.multi.save_dynamic(w);
        }
        w.u16(self.teleports.len() as u16);
        for teleport in &self.teleports {
            teleport.gate.save_dynamic(w);
            w.u16(teleport.cooldown);
        }
        self.targets.save_dynamic(w);
        self.secrets.save_dynamic(w);
        self.monsters.save_dynamic(w);
        self.event_lightning.save_dynamic(w);
        match self.lightning_pair {
            None => w.bool(false),
            Some([a, b]) => {
                w.bool(true);
                w.u16(a as u16);
                w.u16(b as u16);
            }
        }
    }

    /// Put a scene freshly loaded from the saved map back where
    /// [`Self::save_state`] found it. Any value that cannot belong to this
    /// map rejects the reader; the caller then discards the whole load.
    #[inline(never)]
    #[optimize(size)]
    pub fn load_state(&mut self, map: &ResidentMap, r: &mut Reader) {
        self.frame = r.u32();
        self.animation_subtick = r.u8();
        self.chase_random = r.u32();
        self.sight_index = r.u16();
        self.sight_alert_ticks = r.u16();
        for word in self.light_off.iter_mut() {
            *word = r.u32();
        }
        // The switchable styles follow the lights that own them.
        let sources = map.entities();
        for (index, source) in sources.iter().enumerate() {
            if !quake_core::targets::is_light_class(source.class_name) {
                continue;
            }
            let style = source.count;
            if style < lightstyle::FIRST_SWITCHABLE_STYLE as i16
                || style as usize >= lightstyle::STYLE_COUNT
            {
                continue;
            }
            self.light_styles[style as usize] =
                lightstyle::switched_value(!self.light_is_off(index as u16));
        }
        self.light_style_tick = u32::MAX;
        self.light_style_next_tick = u32::MAX;

        let count = self.entities.len();
        if usize::from(r.u16()) != count {
            r.reject();
            return;
        }
        let entities = &mut self.entities;
        load_bits(r, count, |i, on| entities[i].visible = on);
        load_bits(r, count, |i, on| entities[i].solid = on);
        load_bits(r, count, |i, on| entities[i].damageable = on);
        load_bits(r, count, |i, on| {
            if !on {
                entities[i].pickup = None;
            }
        });
        for _ in 0..r.u16() {
            let index = usize::from(r.u16());
            let health = r.i16();
            match self.entities.get_mut(index) {
                Some(entity) if entity.max_health > 0 && health <= entity.max_health => {
                    entity.health = health;
                }
                _ => r.reject(),
            }
        }
        let styles = self.light_styles;
        for _ in 0..r.u16() {
            let index = usize::from(r.u16());
            let origin = r.vec3();
            let yaw = r.i16();
            let frame = r.u16();
            let monster = <MonsterRuntime as quake_core::save::Snapshot>::load(r);
            let patrol = r.u16();
            let ideal_yaw = r.i16();
            let partial_ground = r.bool();
            let Some(entity) = self.entities.get_mut(index) else {
                r.reject();
                return;
            };
            // Only a monster the map placed here can be restored into this
            // slot, and only as the same kind.
            match entity.monster {
                Some(fresh) if fresh.kind() == monster.kind() => {}
                _ => {
                    r.reject();
                    return;
                }
            }
            entity.monster = Some(monster);
            entity.angles.y = yaw;
            entity.frame = frame;
            entity.patrol = patrol;
            entity.ideal_yaw = ideal_yaw;
            entity.partial_ground = partial_ground;
            if !update_moving_alias_origin(map, entity, origin, &styles) {
                r.reject();
                return;
            }
        }

        if usize::from(r.u16()) != self.movers.len() {
            r.reject();
            return;
        }
        for index in 0..self.movers.len() {
            let mover = &mut self.movers[index];
            mover.activator = load_activator(r);
            mover.key_cooldown = r.u16();
            mover.key_spent = r.bool();
            mover.crush.load_dynamic(r);
            mover.health = r.i16();
            mover.shot_open = r.bool();
            mover.policy.load_dynamic(r);
            let render_index = self.movers[index].render_index as usize;
            self.place_mover(map, render_index, index);
        }
        if usize::from(r.u16()) != self.door_fields.len() {
            r.reject();
            return;
        }
        for field in self.door_fields.iter_mut() {
            field.load_dynamic(r);
        }
        if usize::from(r.u16()) != self.trains.len() {
            r.reject();
            return;
        }
        for index in 0..self.trains.len() {
            self.trains[index].crush.load_dynamic(r);
            self.trains[index].policy.load_dynamic(r);
            let render_index = self.trains[index].render_index as usize;
            self.place_train(map, render_index, index);
        }
        if usize::from(r.u16()) != self.triggers.len() {
            r.reject();
            return;
        }
        for trigger in self.triggers.iter_mut() {
            trigger.armed = r.bool();
            trigger.cooldown = r.u16();
            trigger.multi.load_dynamic(r);
        }
        if usize::from(r.u16()) != self.teleports.len() {
            r.reject();
            return;
        }
        for teleport in self.teleports.iter_mut() {
            teleport.gate.load_dynamic(r);
            teleport.cooldown = r.u16();
        }
        self.targets.load_dynamic(r);
        self.secrets.load_dynamic(r);
        self.monsters.load_dynamic(r);
        self.event_lightning.load_dynamic(r);
        self.lightning_pair = if r.bool() {
            let (a, b) = (usize::from(r.u16()), usize::from(r.u16()));
            if a >= self.entities.len() || b >= self.entities.len() {
                r.reject();
                None
            } else {
                Some([a, b])
            }
        } else {
            None
        };
    }

    /// `Host_Loadgame_f` re-enters a skill and the runes in hand before the
    /// map loads; the entity loader reads both while it spawns.
    pub fn restore_game(&mut self, skill: u8, runes: u8) {
        self.skill = skill;
        self.runes = runes;
    }
}
