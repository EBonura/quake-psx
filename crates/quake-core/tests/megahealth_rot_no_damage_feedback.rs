//! The megahealth's rot lowers health without any damage feedback.
//!
//! The port has no damage message, so `Inventory::take_damage` keeps the
//! figures `T_Damage` would have sent and the guest frame loop drains them
//! once a frame (`take_damage_report`). This replays that frame against the
//! screen blend and the pain face: the slow drain from 200 back to 100 must
//! never tint the screen or wake the face, while a real hit still must, even
//! in a frame where a pickup or the rot moves health the other way.

use quake_core::combat::{pickup_for_entity, WeaponState};
use quake_core::hud::PainFaceTimer;
use quake_core::screenblend::ScreenBlend;
use quake_core::survival::{
    SurvivalFrame, SurvivalInput, MEGAHEALTH_ROT_DELAY_TICKS, MEGAHEALTH_ROT_INTERVAL_TICKS,
    PLAYER_MAX_HEALTH, SOUND_PAIN,
};

/// Health the old frame loop would have inferred as damage: the raw drop.
fn raw_drop(before: i16, after: i16) -> u16 {
    before.max(0).saturating_sub(after.max(0)) as u16
}

/// One guest frame of `quake.rs`: the survival tick, then the screen blend and
/// the face fed from the damage report.
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
        let before = self.weapon.inventory().health();
        let survival = self.weapon.tick_survival(SurvivalInput {
            elapsed_ticks: ticks,
            ..SurvivalInput::default()
        });
        let after = self.weapon.inventory().health();
        let blood = self.feel_damage(ticks);
        (survival, blood, raw_drop(before, after))
    }

    /// The presentation half of the frame, fed from the damage report alone.
    fn feel_damage(&mut self, ticks: u16) -> u16 {
        let report = self.weapon.take_damage_report();
        self.blend.tick(ticks);
        self.blend.take_damage(report.blood, report.armor);
        self.face.tick(ticks);
        if report.blood != 0 || report.armor != 0 {
            self.face.take_damage();
        }
        report.blood
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
    let mut raw_drops = 0u32;
    while frame.weapon.inventory().health() > PLAYER_MAX_HEALTH {
        // Single ticks, the finest cadence a frame can have.
        let (survival, blood, raw) = frame.step(1);
        ticks += 1;
        // The raw health drop is what the old code read as damage; it is real
        // on a rot tick, and must still be filtered out of `blood`.
        raw_drops += u32::from(raw);
        assert_eq!(blood, 0, "rot is not damage (tick {ticks})");
        assert!(
            survival.sounds().iter().all(|&id| id != SOUND_PAIN),
            "no pain voice (tick {ticks})"
        );
        assert!(frame.quiet(), "no tint and no pain face (tick {ticks})");
        assert!(ticks < 200 * 60, "the rot must terminate");
    }
    // Five seconds, then one point a second for 100 points: the first point
    // lands on the tick the delay expires, the last one 99 seconds later.
    let expected = u32::from(MEGAHEALTH_ROT_DELAY_TICKS)
        + 99 * u32::from(MEGAHEALTH_ROT_INTERVAL_TICKS);
    assert_eq!(ticks, expected);
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
    // A hit lands in the same tick as the first rot point, the way a monster
    // or projectile does.
    let before = frame.weapon.inventory().health();
    assert_eq!(frame.weapon.take_damage(10), 10);
    let (_, blood, _) = frame.step(1);
    let after = frame.weapon.inventory().health();
    assert_eq!(after, 189);
    assert_eq!(
        raw_drop(before, after),
        11,
        "the health fell by the hit plus the rot point"
    );
    assert_eq!(blood, 10, "the hit is felt in full, the rot point is not");
    assert!(frame.blend.flash_tint().is_some(), "a hit flashes");
    assert!(frame.face.active());
}

#[test]
fn a_health_pickup_in_the_frame_of_a_hit_does_not_hide_the_hit() {
    let mut frame = Frame::new();
    frame.weapon.take_damage(50);
    frame.weapon.take_damage_report();
    assert!(frame.quiet());
    assert_eq!(frame.weapon.inventory().health(), 50);
    // 30 damage, then +25 health from a small medkit, in one frame: health
    // falls by 5 overall but the hit was 30.
    assert_eq!(frame.weapon.take_damage(30), 30);
    let medkit = pickup_for_entity(0x22, 0).expect("health");
    assert!(frame.weapon.apply_pickup(medkit).consumed);
    assert_eq!(frame.weapon.inventory().health(), 45);
    let blood = frame.feel_damage(1);
    assert_eq!(blood, 30, "a raw drop of 5 would have under-reported it");
    assert!(frame.blend.flash_tint().is_some());
    assert!(frame.face.active());
    // A pickup that outweighs the hit used to read as no damage at all.
    let mut frame = Frame::new();
    frame.weapon.take_damage(50);
    frame.weapon.take_damage_report();
    assert_eq!(frame.weapon.take_damage(10), 10);
    assert!(frame.weapon.apply_pickup(medkit).consumed);
    assert!(frame.weapon.inventory().health() > 50);
    assert_eq!(frame.feel_damage(1), 10);
    assert!(frame.blend.flash_tint().is_some());
}

#[test]
fn armor_absorbed_in_the_frame_of_an_armor_pickup_is_still_reported() {
    use quake_core::combat::Pickup;
    let mut frame = Frame::new();
    let green = pickup_for_entity(0x1a, 0).expect("green armor");
    assert!(matches!(green, Pickup::Armor { .. }));
    assert!(frame.weapon.apply_pickup(green).consumed);
    let before = frame.weapon.inventory().armor();
    frame.weapon.take_damage(20);
    let red = pickup_for_entity(0x1c, 0).expect("red armor");
    assert!(frame.weapon.apply_pickup(red).consumed);
    assert!(frame.weapon.inventory().armor() > before);
    let report = frame.weapon.take_damage_report();
    assert_eq!(report.blood, 14);
    assert_eq!(report.armor, 6);
    assert_eq!(frame.weapon.take_damage_report().blood, 0, "drained once");
}

#[test]
fn a_hit_on_a_corpse_is_not_reported_as_more_blood() {
    let mut frame = Frame::new();
    frame.weapon.take_damage(1000);
    assert_eq!(frame.weapon.take_damage_report().blood, 100);
    frame.weapon.take_damage(50);
    assert_eq!(frame.weapon.take_damage_report().blood, 0);
}

#[test]
fn a_hit_on_a_normal_player_flashes_and_wakes_the_face() {
    let mut frame = Frame::new();
    assert_eq!(frame.weapon.inventory().health(), 100);
    frame.weapon.take_damage(25);
    let (_, blood, _) = frame.step(1);
    assert_eq!(blood, 25);
    assert!(frame.blend.flash_tint().is_some());
    assert!(frame.face.active());
}
