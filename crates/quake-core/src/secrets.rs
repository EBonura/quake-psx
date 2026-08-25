//! Original `trigger_secret` bookkeeping.
//!
//! The original counts `total_secrets` once per spawned `trigger_secret` and
//! raises `found_secrets` in `multi_trigger`, but only when the activator is
//! the player. Both are per-level globals.

use crate::targets::{excluded_for_skill, TargetEntitySource};

const CLASS_TRIGGER_SECRET: u8 = 0x50;

/// Default `trigger_secret` message when the map authors none.
pub const SECRET_MESSAGE: &str = "You found a secret area!";
/// `trigger_secret` forces `sounds 1`, which is misc/secret.
pub const SECRET_SOUND_ID: i16 = 0x7a;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretCounter {
    found: u16,
    total: u16,
}

impl SecretCounter {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self { found: 0, total: 0 }
    }

    #[optimize(size)]
    pub const fn found(&self) -> u16 {
        self.found
    }

    #[optimize(size)]
    pub const fn total(&self) -> u16 {
        self.total
    }

    /// Recount `total_secrets` for a freshly loaded map.
    #[optimize(size)]
    pub fn load<S>(&mut self, source: &S, skill: u8)
    where
        S: TargetEntitySource + ?Sized,
    {
        self.found = 0;
        self.total = count_authored(source, skill);
    }

    /// `found_secrets = found_secrets + 1`, clamped at the authored total so a
    /// re-armed trigger can never overcount.
    #[optimize(size)]
    pub fn record_found(&mut self) {
        if self.found < self.total {
            self.found += 1;
        }
    }
}

/// Number of `trigger_secret` entities a skill actually spawns.
#[optimize(size)]
pub fn count_authored<S>(source: &S, skill: u8) -> u16
where
    S: TargetEntitySource + ?Sized,
{
    let mut total = 0u16;
    for index in 0..source.entity_count() {
        let Some(entity) = source.entity_at(index) else {
            break;
        };
        if entity.class_name == CLASS_TRIGGER_SECRET
            && !excluded_for_skill(entity.spawn_flags, skill)
        {
            total = total.saturating_add(1);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::SPAWNFLAG_NOT_EASY;
    use quake_formats::MapEntity;

    #[optimize(size)]
    fn secret(spawn_flags: u16) -> MapEntity {
        MapEntity {
            class_name: CLASS_TRIGGER_SECRET,
            spawn_flags,
            ..MapEntity::default()
        }
    }

    #[optimize(size)]
    #[test]
    fn the_total_counts_only_the_secrets_this_skill_spawns() {
        let source = [
            secret(0),
            secret(SPAWNFLAG_NOT_EASY),
            secret(0),
            MapEntity::default(),
        ];
        assert_eq!(count_authored(&source[..], 0), 2);
        assert_eq!(count_authored(&source[..], 1), 3);
    }

    #[optimize(size)]
    #[test]
    fn found_rises_to_the_total_and_stops() {
        let source = [secret(0), secret(0)];
        let mut counter = SecretCounter::new();
        counter.load(&source[..], 0);
        assert_eq!((counter.found(), counter.total()), (0, 2));
        counter.record_found();
        counter.record_found();
        counter.record_found();
        assert_eq!((counter.found(), counter.total()), (2, 2));
    }

    #[optimize(size)]
    #[test]
    fn loading_a_new_map_resets_the_found_count() {
        let first = [secret(0), secret(0), secret(0)];
        let second = [secret(0)];
        let mut counter = SecretCounter::new();
        counter.load(&first[..], 0);
        counter.record_found();
        counter.load(&second[..], 0);
        assert_eq!((counter.found(), counter.total()), (0, 1));
    }
}
