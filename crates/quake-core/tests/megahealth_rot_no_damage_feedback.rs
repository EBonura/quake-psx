//! The megahealth's rot lowers health without any damage feedback.
//!
//! The port has no damage message, so the guest frame loop infers damage from
//! the inventory either side of the frame. This replays that inference (the
//! same `SurvivalFrame::health_damage` the loop calls) against the screen
//! blend and the pain face: the slow drain from 200 back to 100 must never
//! tint the screen or wake the face, while a real hit still must.

use quake_core::combat::{pickup_for_entity, WeaponState};
use quake_core::hud::PainFaceTimer;
use quake_core::screenblend::ScreenBlend;
use quake_core::survival::{
    SurvivalFrame, SurvivalInput, MEGAHEALTH_ROT_DELAY_TICKS, MEGAHEALTH_ROT_INTERVAL_TICKS,
    PLAYER_MAX_HEALTH, SOUND_PAIN,
};

/// One guest frame of `quake.rs`: health either side of the survival tick,
/// then the screen blend and the face fed from the same damage figure.
struct Frame {
    weapon: WeaponState,
    blend: ScreenBlend,
    face: PainFaceTimer,
}

impl Frame {
    fn new() -> Self {
        Self {
            weapon: WeaponState::new(),
            blend: ScreenBlend::new(),
            face: PainFaceTimer::new(),
        }
    }

    fn step(&mut self, ticks: u16) -> (SurvivalFrame, u16, u16) {
        let before = self.weapon.inventory().health().max(0) as u16;
        let survival = self.weapon.tick_survival(SurvivalInput {
            elapsed_ticks: ticks,
            ..SurvivalInput::default()
        });
        let after = self.weapon.inventory().health().max(0) as u16;
        let blood = survival.health_damage(before, after);
        self.blend.tick(ticks);
        self.blend.take_damage(blood, 0);
        self.face.tick(ticks);
        if blood != 0 {
            self.face.take_damage();
        }
        (survival, blood, before.saturating_sub(after))
    }

    fn quiet(&self) -> bool {
        self.blend.flash_tint().is_none() && !self.face.active()
    }
}

fn megahealth() -> quake_core::combat::Pickup {
    // Class 0x22 with spawn flags 2 is the item_health megahealth.
    pickup_for_entity(0x22, 2).expect("megahealth")
}

#[test]
fn the_rot_from_200_to_100_never_flashes_the_screen_or_the_face() {
    let mut frame = Frame::new();
    assert!(frame.weapon.apply_pickup(megahealth()).consumed);
    assert_eq!(frame.weapon.inventory().health(), 200);
    // The pickup's own bonus flash is the original's V_BonusFlash and is not
    // the subject here; let it fade before watching the drain.
    frame.blend.tick(120);
    assert!(frame.quiet());

    let mut ticks = 0u32;
    let mut losses = 0u32;
    let mut raw_drops = 0u32;
    while frame.weapon.inventory().health() > PLAYER_MAX_HEALTH {
        // Single ticks, the finest cadence a frame can have.
        let (survival, blood, raw_drop) = frame.step(1);
        ticks += 1;
        // The raw health drop is what the old code read as damage; it is real
        // on a rot tick, and must still be filtered out of `blood`.
        assert_eq!(raw_drop, survival.rot_loss, "raw drop is the rot (tick {ticks})");
        raw_drops += u32::from(raw_drop);
        assert_eq!(blood, 0, "rot is not damage (tick {ticks})");
        assert!(
            survival.sounds().iter().all(|&id| id != SOUND_PAIN),
            "no pain voice (tick {ticks})"
        );
        assert!(frame.quiet(), "no tint and no pain face (tick {ticks})");
        losses += u32::from(survival.rot_loss);
        assert!(ticks < 200 * 60, "the rot must terminate");
    }
    // Five seconds, then one point a second for 100 points: the first point
    // lands on the tick the delay expires, the last one 99 seconds later.
    let expected = u32::from(MEGAHEALTH_ROT_DELAY_TICKS)
        + 99 * u32::from(MEGAHEALTH_ROT_INTERVAL_TICKS);
    assert_eq!(ticks, expected);
    assert_eq!(losses, 100);
    assert_eq!(raw_drops, 100, "the health really did fall 100 points");
    assert_eq!(frame.weapon.inventory().health(), PLAYER_MAX_HEALTH);
}

#[test]
fn a_catch_up_frame_that_rots_several_points_is_still_not_damage() {
    let mut frame = Frame::new();
    assert!(frame.weapon.apply_pickup(megahealth()).consumed);
    frame.blend.tick(120);
    // One long frame spanning the delay and several rot intervals.
    let span = MEGAHEALTH_ROT_DELAY_TICKS + 3 * MEGAHEALTH_ROT_INTERVAL_TICKS;
    for _ in 0..span / 4 {
        let (_, blood, _) = frame.step(4);
        assert_eq!(blood, 0);
        assert!(frame.quiet());
    }
    assert!(frame.weapon.inventory().health() < 200);
}

#[test]
fn a_real_hit_in_the_frame_the_rot_lands_flashes_by_exactly_its_own_damage() {
    let mut frame = Frame::new();
    assert!(frame.weapon.apply_pickup(megahealth()).consumed);
    frame.blend.tick(120);
    // Wait out everything but the last tick of the five second delay.
    for _ in 0..MEGAHEALTH_ROT_DELAY_TICKS - 1 {
        frame.step(1);
    }
    assert!(frame.quiet());
    assert_eq!(frame.weapon.inventory().health(), 200);
    // A hit lands between the frame's `health_before` and the survival tick,
    // the way a monster or projectile does, in the same tick as the first rot.
    let before = frame.weapon.inventory().health().max(0) as u16;
    assert_eq!(frame.weapon.take_damage(10), 10);
    let survival = frame.weapon.tick_survival(SurvivalInput {
        elapsed_ticks: 1,
        ..SurvivalInput::default()
    });
    assert_eq!(survival.rot_loss, 1, "the rot landed in the hit's frame");
    let after = frame.weapon.inventory().health().max(0) as u16;
    assert_eq!(after, 189);
    let blood = survival.health_damage(before, after);
    assert_eq!(blood, 10, "the hit is felt in full, the rot point is not");
    frame.blend.take_damage(blood, 0);
    assert!(frame.blend.flash_tint().is_some(), "a hit flashes");
}

#[test]
fn a_hit_on_a_normal_player_flashes_and_wakes_the_face() {
    let mut frame = Frame::new();
    assert_eq!(frame.weapon.inventory().health(), 100);
    let before = 100u16;
    frame.weapon.take_damage(25);
    let survival = frame.weapon.tick_survival(SurvivalInput {
        elapsed_ticks: 1,
        ..SurvivalInput::default()
    });
    let after = frame.weapon.inventory().health().max(0) as u16;
    let blood = survival.health_damage(before, after);
    assert_eq!(blood, 25);
    frame.blend.take_damage(blood, 0);
    frame.face.take_damage();
    assert!(frame.blend.flash_tint().is_some());
    assert!(frame.face.active());
}
