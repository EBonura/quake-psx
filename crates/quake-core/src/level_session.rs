//! Mutable presentation state for one level session.
//!
//! Resident map assets may survive a same-map restart, but transient state
//! must not. Keeping the session generation beside the effects makes that
//! distinction explicit: map generations identify immutable bytes, while a
//! level-session generation identifies one gameplay lifetime over those
//! bytes.

use crate::{
    effects::{ExplosionEffects, ImpactParticles},
    screenblend::ScreenBlend,
};

/// One authored or built-in center-screen message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CenterprintText {
    /// Offset into the resident map's cooked string table.
    Cooked(u16),
    /// Built-in gameplay text.
    Fixed(&'static str),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Centerprint {
    text: CenterprintText,
    ticks: u16,
}

/// Presentation state that must be recreated for every gameplay session.
pub struct LevelPresentation {
    generation: u32,
    centerprint: Option<Centerprint>,
    screen_blend: ScreenBlend,
    explosion_effects: ExplosionEffects,
    impact_particles: ImpactParticles,
    elapsed_ticks: u32,
}

impl LevelPresentation {
    /// Empty state before the initial level has committed.
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            generation: 0,
            centerprint: None,
            screen_blend: ScreenBlend::new(),
            explosion_effects: ExplosionEffects::new(),
            impact_particles: ImpactParticles::new(),
            elapsed_ticks: 0,
        }
    }

    /// Begin a fresh gameplay lifetime over the current resident map.
    ///
    /// Generation zero is reserved for the pre-load state, including after
    /// integer wrap, so stale session handles never become current again by
    /// matching the sentinel.
    #[optimize(size)]
    pub fn reset(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        self.centerprint = None;
        self.screen_blend = ScreenBlend::new();
        self.explosion_effects.enter_map(self.generation);
        self.impact_particles.enter_map(self.generation);
        self.elapsed_ticks = 0;
        self.generation
    }

    /// Publish a rebuilt level only after every mutable subsystem succeeded.
    ///
    /// The caller builds Player, entities, and audio first, then passes the
    /// completed value here. A failed rebuild leaves presentation state and
    /// its generation untouched; the shipping guest treats that error as
    /// fail-stop rather than exposing a half-reset session.
    #[optimize(size)]
    pub fn commit_ready<T>(&mut self, ready: Option<T>) -> Option<T> {
        let value = ready?;
        self.reset();
        Some(value)
    }

    /// Current nonzero gameplay-session generation, or zero before load.
    #[optimize(size)]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Replace the current center-screen message and its lifetime.
    #[optimize(size)]
    pub fn set_centerprint(&mut self, text: CenterprintText, ticks: u16) {
        self.centerprint = Some(Centerprint { text, ticks });
    }

    /// Advance and expire the center-screen message.
    #[optimize(size)]
    pub fn tick_centerprint(&mut self, ticks: u16) {
        let Some(active) = self.centerprint.as_mut() else {
            return;
        };
        active.ticks = active.ticks.saturating_sub(ticks);
        if active.ticks == 0 {
            self.centerprint = None;
        }
    }

    /// Message which should be resolved and drawn this frame.
    #[optimize(size)]
    pub const fn centerprint(&self) -> Option<CenterprintText> {
        match self.centerprint {
            Some(active) => Some(active.text),
            None => None,
        }
    }

    /// Advance the level clock the intermission panel prints.
    ///
    /// Only gameplay frames call this, so a paused game or the panel itself
    /// does not inflate the completion time.
    #[optimize(size)]
    pub fn tick_level_clock(&mut self, ticks: u16) {
        self.elapsed_ticks = self.elapsed_ticks.saturating_add(u32::from(ticks));
    }

    /// `cl.completed_time`: whole seconds of gameplay on this level.
    #[optimize(size)]
    pub const fn elapsed_seconds(&self) -> u16 {
        let seconds = self.elapsed_ticks / 60;
        if seconds > u16::MAX as u32 {
            u16::MAX
        } else {
            seconds as u16
        }
    }

    /// Read the current Quake-style full-screen tints.
    #[optimize(size)]
    pub const fn screen_blend(&self) -> &ScreenBlend {
        &self.screen_blend
    }

    /// Mutate the current Quake-style full-screen tints.
    #[optimize(size)]
    pub fn screen_blend_mut(&mut self) -> &mut ScreenBlend {
        &mut self.screen_blend
    }

    /// Read short-lived world-space presentation effects.
    #[optimize(size)]
    pub const fn explosion_effects(&self) -> &ExplosionEffects {
        &self.explosion_effects
    }

    /// Mutate short-lived world-space presentation effects.
    #[optimize(size)]
    pub fn explosion_effects_mut(&mut self) -> &mut ExplosionEffects {
        &mut self.explosion_effects
    }

    /// Read short-lived weapon-impact particles.
    #[optimize(size)]
    pub const fn impact_particles(&self) -> &ImpactParticles {
        &self.impact_particles
    }

    /// Mutate short-lived weapon-impact particles.
    #[optimize(size)]
    pub fn impact_particles_mut(&mut self) -> &mut ImpactParticles {
        &mut self.impact_particles
    }
}

impl Default for LevelPresentation {
    #[optimize(size)]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::Vec3I32;

    #[optimize(size)]
    #[test]
    fn same_map_session_reset_clears_every_transient_presentation_layer() {
        let mut presentation = LevelPresentation::new();
        assert_eq!(presentation.reset(), 1);
        presentation.set_centerprint(CenterprintText::Fixed("stale"), 120);
        presentation.screen_blend_mut().pick_up();
        presentation.explosion_effects_mut().spawn(Vec3I32 {
            x: 1 << 12,
            y: 2 << 12,
            z: 3 << 12,
        });

        assert!(presentation.centerprint().is_some());
        assert!(presentation.screen_blend().flash_tint().is_some());
        assert_eq!(presentation.explosion_effects().active().count(), 1);

        assert_eq!(presentation.reset(), 2);
        assert_eq!(presentation.centerprint(), None);
        assert_eq!(presentation.screen_blend().flash_tint(), None);
        assert_eq!(presentation.screen_blend().contents_tint(), None);
        assert_eq!(presentation.explosion_effects().active().count(), 0);
    }

    #[optimize(size)]
    #[test]
    fn a_failed_level_rebuild_cannot_publish_a_partial_presentation_reset() {
        let mut presentation = LevelPresentation::new();
        presentation.reset();
        presentation.set_centerprint(CenterprintText::Fixed("keep until commit"), 120);
        presentation.screen_blend_mut().pick_up();
        presentation.explosion_effects_mut().spawn(Vec3I32 {
            x: 1 << 12,
            y: 0,
            z: 0,
        });

        assert_eq!(presentation.commit_ready::<()>(None), None);
        assert_eq!(presentation.generation(), 1);
        assert!(presentation.centerprint().is_some());
        assert!(presentation.screen_blend().flash_tint().is_some());
        assert_eq!(presentation.explosion_effects().active().count(), 1);

        assert_eq!(presentation.commit_ready(Some(7u8)), Some(7));
        assert_eq!(presentation.generation(), 2);
        assert_eq!(presentation.centerprint(), None);
        assert_eq!(presentation.screen_blend().flash_tint(), None);
        assert_eq!(presentation.explosion_effects().active().count(), 0);
    }

    #[optimize(size)]
    #[test]
    fn session_generation_never_uses_the_zero_sentinel() {
        let mut presentation = LevelPresentation::new();
        presentation.generation = u32::MAX;
        assert_eq!(presentation.reset(), 1);
        assert_eq!(presentation.generation(), 1);
    }
}
