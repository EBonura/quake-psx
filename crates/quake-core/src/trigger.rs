//! `trigger_multiple`'s two spawn shapes.
//!
//! The original reads one key to choose between them:
//!
//! ```text
//! if (self.health)
//! {
//!     self.max_health = self.health;
//!     self.th_die = multi_killed;
//!     self.takedamage = DAMAGE_YES;
//!     self.solid = SOLID_BBOX;
//!     setorigin (self, self.origin);  // make sure it links into the world
//! }
//! else if ( !(self.spawnflags & SPAWNFLAG_NOTOUCH) )
//!     self.touch = multi_touch;
//! ```
//!
//! The branches are exclusive: a trigger authored with a `health` key never
//! gains a touch function, so it fires only from `multi_killed`, and a trigger
//! without one is never solid and never takes damage. `multi_trigger` clears
//! `takedamage` when it fires and `multi_wait` restores `max_health` once the
//! authored wait elapses, so a shootable trigger with a positive wait can be
//! shot again.

use quake_formats::Vec3I32;

use crate::combat::segment_aabb_fraction;

/// Cooked `trigger_multiple`. Only this class reads `health`: `trigger_once`
/// and `trigger_secret` reach `trigger_multiple`'s spawn function through their
/// own, which never sets one.
pub const CLASS_TRIGGER_MULTIPLE: u8 = 0x4b;

/// What one `T_Damage` call did to a shootable trigger.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MultiTriggerDamage {
    /// Points that actually landed. Zero when the trigger is not shootable or
    /// has already been killed and not yet healed.
    pub applied: i16,
    /// `multi_killed` ran on this call.
    pub killed: bool,
}

/// The health half of one cooked trigger volume.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MultiTrigger {
    health: i16,
    max_health: i16,
    killed: bool,
}

impl MultiTrigger {
    /// Spawn from the cooked class and `health` key. A non-`trigger_multiple`
    /// class, or a value the original would treat as no key at all, produces
    /// the ordinary touch trigger.
    #[optimize(size)]
    pub const fn new(class_name: u8, health: i16) -> Self {
        let health = if class_name == CLASS_TRIGGER_MULTIPLE && health > 0 {
            health
        } else {
            0
        };
        Self {
            health,
            max_health: health,
            killed: false,
        }
    }

    /// `self.solid == SOLID_BBOX && self.takedamage == DAMAGE_YES`.
    #[optimize(size)]
    pub const fn shootable(&self) -> bool {
        self.max_health > 0
    }

    /// The original only ever assigns `multi_touch` on the branch it did not
    /// take here, so a shootable trigger is deaf to touch forever.
    #[optimize(size)]
    pub const fn responds_to_touch(&self) -> bool {
        !self.shootable()
    }

    /// A live shootable trigger stops shots and takes them.
    #[optimize(size)]
    pub const fn takes_damage(&self) -> bool {
        self.shootable() && self.health > 0
    }

    #[optimize(size)]
    pub const fn health(&self) -> i16 {
        self.health
    }

    /// `T_Damage`. Reaching zero runs `multi_killed`, which is latched here
    /// because the shot is resolved in the weapon phase and the trigger's
    /// targets fire from the gameplay phase.
    #[optimize(size)]
    pub fn take_damage(&mut self, damage: i16) -> MultiTriggerDamage {
        if damage <= 0 || !self.takes_damage() {
            return MultiTriggerDamage::default();
        }
        self.health = self.health.saturating_sub(damage);
        let killed = self.health <= 0;
        if killed {
            self.killed = true;
        }
        MultiTriggerDamage {
            applied: damage,
            killed,
        }
    }

    /// Consume a pending `multi_killed`.
    #[optimize(size)]
    pub fn take_kill(&mut self) -> bool {
        core::mem::take(&mut self.killed)
    }

    /// `multi_wait`: hand the health back once the authored wait has elapsed.
    /// A kill that has not been carried to `SUB_UseTargets` yet is never healed
    /// out from under the fire it is about to cause.
    #[optimize(size)]
    pub fn heal_after_wait(&mut self, wait_elapsed: bool) {
        if wait_elapsed && self.shootable() && self.health <= 0 && !self.killed {
            self.health = self.max_health;
        }
    }
}

/// One trigger volume offered to a shot, in cooked order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShotCandidate {
    pub trigger: MultiTrigger,
    /// Everything outside this module that can take a volume out of play: a
    /// spent `trigger_once`, a `killtarget`ed one, a disabled target.
    pub enabled: bool,
    /// Quake Q20.12 world bounds.
    pub mins: Vec3I32,
    pub maxs: Vec3I32,
}

/// The nearest live shootable trigger a segment reaches strictly before
/// `limit`, which is the fraction at which the shot already stops on world
/// brush or on a damageable body. A shootable trigger is `SOLID_BBOX`, so the
/// winner stops the shot as well as taking it. Ties keep the earlier candidate,
/// so the answer only depends on cooked order.
#[optimize(size)]
pub fn nearest_shot<I>(
    start: Vec3I32,
    end: Vec3I32,
    limit: i32,
    candidates: I,
) -> Option<(usize, i32)>
where
    I: IntoIterator<Item = ShotCandidate>,
{
    let mut best = None;
    let mut best_fraction = limit;
    for (index, candidate) in candidates.into_iter().enumerate() {
        if !candidate.enabled || !candidate.trigger.takes_damage() {
            continue;
        }
        let Some(fraction) = segment_aabb_fraction(start, end, candidate.mins, candidate.maxs)
        else {
            continue;
        };
        if fraction < best_fraction {
            best_fraction = fraction;
            best = Some(index);
        }
    }
    best.map(|index| (index, best_fraction))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS_TRIGGER_ONCE: u8 = 0x4c;

    const Q12_ONE: i32 = 4096;

    #[optimize(size)]
    fn point(x: i32, y: i32, z: i32) -> Vec3I32 {
        Vec3I32 {
            x: x << 12,
            y: y << 12,
            z: z << 12,
        }
    }

    /// A plate on a wall, 100 units down the +x axis from the origin.
    #[optimize(size)]
    fn plate(class_name: u8, health: i16, enabled: bool) -> ShotCandidate {
        ShotCandidate {
            trigger: MultiTrigger::new(class_name, health),
            enabled,
            mins: point(100, -16, -16),
            maxs: point(106, 16, 16),
        }
    }

    #[optimize(size)]
    #[test]
    fn a_health_key_spawns_the_shootable_form() {
        let trigger = MultiTrigger::new(CLASS_TRIGGER_MULTIPLE, 1);
        assert!(trigger.shootable());
        assert!(trigger.takes_damage());
        assert!(!trigger.responds_to_touch());
        assert_eq!(trigger.health(), 1);
    }

    #[optimize(size)]
    #[test]
    fn a_trigger_without_a_health_key_is_untouched_by_this() {
        for class_name in [CLASS_TRIGGER_MULTIPLE, CLASS_TRIGGER_ONCE] {
            let mut trigger = MultiTrigger::new(class_name, 0);
            assert!(!trigger.shootable(), "class {class_name:#04x}");
            assert!(trigger.responds_to_touch(), "class {class_name:#04x}");
            assert!(!trigger.takes_damage(), "class {class_name:#04x}");
            assert_eq!(trigger.take_damage(100), MultiTriggerDamage::default());
            assert!(!trigger.take_kill());
            trigger.heal_after_wait(true);
            assert_eq!(trigger.health(), 0);
        }
    }

    /// Only `trigger_multiple` runs the branch, so a `health` key cooked onto
    /// any other trigger class stays an ordinary touch volume.
    #[optimize(size)]
    #[test]
    fn only_trigger_multiple_reads_the_health_key() {
        let trigger = MultiTrigger::new(CLASS_TRIGGER_ONCE, 1);
        assert!(!trigger.shootable());
        assert!(trigger.responds_to_touch());
    }

    #[optimize(size)]
    #[test]
    fn killing_one_latches_multi_killed_exactly_once() {
        let mut trigger = MultiTrigger::new(CLASS_TRIGGER_MULTIPLE, 1);
        let damage = trigger.take_damage(4);
        assert_eq!(
            damage,
            MultiTriggerDamage {
                applied: 4,
                killed: true
            }
        );
        // Dead triggers stop shots but stop taking them.
        assert!(!trigger.takes_damage());
        assert_eq!(trigger.take_damage(4), MultiTriggerDamage::default());
        assert!(trigger.take_kill());
        assert!(!trigger.take_kill());
    }

    #[optimize(size)]
    #[test]
    fn a_partial_hit_does_not_fire_it() {
        let mut trigger = MultiTrigger::new(CLASS_TRIGGER_MULTIPLE, 10);
        let damage = trigger.take_damage(4);
        assert_eq!(
            damage,
            MultiTriggerDamage {
                applied: 4,
                killed: false
            }
        );
        assert_eq!(trigger.health(), 6);
        assert!(!trigger.take_kill());
        assert!(trigger.takes_damage());
    }

    #[optimize(size)]
    #[test]
    fn multi_wait_restores_it_only_after_the_kill_is_carried() {
        let mut trigger = MultiTrigger::new(CLASS_TRIGGER_MULTIPLE, 1);
        trigger.take_damage(1);
        // The wait cannot heal a kill that `SUB_UseTargets` has not seen.
        trigger.heal_after_wait(true);
        assert_eq!(trigger.health(), 0);
        assert!(trigger.take_kill());
        trigger.heal_after_wait(false);
        assert_eq!(trigger.health(), 0);
        trigger.heal_after_wait(true);
        assert_eq!(trigger.health(), 1);
        assert!(trigger.takes_damage());
    }

    /// A negative `health` key is not a live target: the original would spawn
    /// it `takedamage` and the first hit would kill it, which is the same
    /// observable answer as never arming it, and this keeps the arithmetic
    /// honest instead of leaving a trigger with negative health.
    #[optimize(size)]
    #[test]
    fn a_non_positive_health_key_is_not_shootable() {
        let trigger = MultiTrigger::new(CLASS_TRIGGER_MULTIPLE, -1);
        assert!(!trigger.shootable());
        assert!(trigger.responds_to_touch());
    }

    #[optimize(size)]
    #[test]
    fn a_shot_down_the_axis_reaches_the_plate() {
        let hit = nearest_shot(
            point(0, 0, 0),
            point(200, 0, 0),
            Q12_ONE,
            [plate(CLASS_TRIGGER_MULTIPLE, 1, true)],
        );
        // 100 of the segment's 200 units.
        assert_eq!(hit, Some((0, Q12_ONE / 2)));
    }

    #[optimize(size)]
    #[test]
    fn a_shot_that_misses_or_stops_first_reaches_nothing() {
        let candidate = plate(CLASS_TRIGGER_MULTIPLE, 1, true);
        assert_eq!(
            nearest_shot(point(0, 64, 0), point(200, 64, 0), Q12_ONE, [candidate]),
            None,
            "a segment outside the box on an axis is not a hit"
        );
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE / 4, [candidate]),
            None,
            "world brush stopping the shot first wins"
        );
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(50, 0, 0), Q12_ONE, [candidate]),
            None,
            "a segment that ends short of the box is not a hit"
        );
    }

    /// The whole point of the branch: a touch volume standing in the same
    /// place is not solid, takes nothing, and never stops a shot.
    #[optimize(size)]
    #[test]
    fn an_ordinary_touch_trigger_is_never_shot() {
        for candidate in [
            plate(CLASS_TRIGGER_MULTIPLE, 0, true),
            plate(CLASS_TRIGGER_ONCE, 1, true),
        ] {
            assert_eq!(
                nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE, [candidate]),
                None
            );
        }
    }

    #[optimize(size)]
    #[test]
    fn a_disabled_or_already_killed_plate_is_never_shot() {
        assert_eq!(
            nearest_shot(
                point(0, 0, 0),
                point(200, 0, 0),
                Q12_ONE,
                [plate(CLASS_TRIGGER_MULTIPLE, 1, false)]
            ),
            None
        );
        let mut dead = plate(CLASS_TRIGGER_MULTIPLE, 1, true);
        dead.trigger.take_damage(1);
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE, [dead]),
            None,
            "a killed plate still blocks in the original, but takes nothing"
        );
    }

    #[optimize(size)]
    #[test]
    fn the_nearest_plate_wins_and_ties_keep_cooked_order() {
        let near = ShotCandidate {
            mins: point(40, -16, -16),
            maxs: point(46, 16, 16),
            ..plate(CLASS_TRIGGER_MULTIPLE, 1, true)
        };
        let far = plate(CLASS_TRIGGER_MULTIPLE, 1, true);
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE, [far, near]),
            Some((1, Q12_ONE / 5))
        );
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE, [near, far]),
            Some((0, Q12_ONE / 5))
        );
        assert_eq!(
            nearest_shot(point(0, 0, 0), point(200, 0, 0), Q12_ONE, [far, far]),
            Some((0, Q12_ONE / 2)),
            "a tie keeps the earlier cooked volume"
        );
    }
}
